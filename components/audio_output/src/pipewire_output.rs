use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::{properties::properties, spa};
use ringbuf::HeapConsumer;
use spa::pod::Pod;
use tracing::{info, warn};

pub const DEFAULT_CHANNELS: u32 = 2;
pub const CHAN_SIZE: usize = std::mem::size_of::<f32>();

pub enum StreamCommand {
    Shutdown(mpsc::SyncSender<HeapConsumer<f32>>),
    /// Flush PipeWire's own in-flight buffers. The mixer ring is drained by the
    /// RT callback itself (see `PipewireOutput::flush`), not by this command.
    Flush,
}

impl std::fmt::Debug for StreamCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamCommand::Shutdown(_) => write!(f, "Shutdown(..)"),
            StreamCommand::Flush => write!(f, "Flush"),
        }
    }
}

/// Frames of scratch space preallocated for the RT callback. Larger than any
/// quantum PipeWire normally negotiates, so the steady state never allocates.
const SCRATCH_FRAMES: usize = 8192;

/// Consecutive fully silent callbacks required before the output is
/// considered drained.
///
/// The first silent callback proves the last real samples reached the graph;
/// the driver/ALSA buffer holds roughly 1-2 further quanta that must still be
/// played out. Requiring 2 silent callbacks gives that margin.
pub const DRAINED_AFTER_SILENT_CALLBACKS: u32 = 2;

/// Per-callback bookkeeping shared between the PipeWire RT callback and
/// [`PipewireOutput`]. Independent of PipeWire so it can be unit tested.
///
/// Only atomic loads/stores are used: safe to call from the RT thread
/// (no locks, no allocation).
///
/// # Drain semantics
///
/// A fully silent callback (`samples_read == 0`) means the *previous* callback
/// already handed the last real samples to the graph, so the in-flight
/// quantum is accounted for and the output has played everything it was given.
/// That alone does not mean the driver/ALSA buffer (~1-2 quanta) has played
/// it, so [`is_drained`](Self::is_drained) requires
/// [`DRAINED_AFTER_SILENT_CALLBACKS`] consecutive fully silent callbacks. A
/// fresh stream has recorded nothing and is NOT drained.
///
/// # Invariant
///
/// A *paused* stream (engine pause flag) does receive callbacks, but
/// [`render`] deliberately does not record them, so the counter freezes at its
/// last value rather than counting up. A serviced flush resets the counter.
#[derive(Debug, Default)]
pub struct PlayoutMonitor {
    silent_callbacks: AtomicU32,
    underrun_streak: AtomicU32,
    /// Generation of the most recent flush request (caller side writes).
    flush_requested: AtomicU64,
    /// Generation of the most recent flush acknowledged (RT callback writes;
    /// the caller may also raise it to abandon a request).
    flush_completed: AtomicU64,
    /// Samples discarded by the most recently serviced flush.
    flush_drained: AtomicUsize,
}

impl PlayoutMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the RT callback to discard everything in its ring consumer.
    /// Returns the generation to pass to [`flush_acked`](Self::flush_acked).
    /// Callable from any thread; atomics only.
    pub fn request_flush(&self) -> u64 {
        self.flush_requested.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// True once the callback has serviced (or the caller abandoned) request
    /// `generation`.
    pub fn flush_acked(&self, generation: u64) -> bool {
        self.flush_completed.load(Ordering::Acquire) >= generation
    }

    /// Give up on request `generation` so a stalled stream does not
    /// later discard audio produced after the caller stopped waiting.
    pub fn abandon_flush(&self, generation: u64) {
        self.flush_completed.fetch_max(generation, Ordering::AcqRel);
    }

    /// Samples discarded by the most recently serviced flush. Meaningful after
    /// [`flush_acked`](Self::flush_acked) returned true.
    pub fn last_flush_drained(&self) -> usize {
        self.flush_drained.load(Ordering::Acquire)
    }

    /// RT side: if a flush is pending, discard everything in `consumer`,
    /// record the count and acknowledge. No allocation, no locks.
    fn service_flush(&self, consumer: &mut HeapConsumer<f32>) {
        let requested = self.flush_requested.load(Ordering::Acquire);
        if self.flush_completed.load(Ordering::Acquire) >= requested {
            return;
        }
        let drained = consumer.clear();
        // The ring was emptied by a flush, not played out: restart drain and
        // underrun tracking. RT thread is the single writer of both counters.
        self.silent_callbacks.store(0, Ordering::Relaxed);
        self.underrun_streak.store(0, Ordering::Relaxed);
        self.flush_drained.store(drained, Ordering::Release);
        self.flush_completed.fetch_max(requested, Ordering::AcqRel);
    }

    /// Record one callback. Returns `true` if a mid-stream underrun warning
    /// should be logged for it.
    ///
    /// Only *partial* fills (`0 < samples_read < samples_requested`) are real
    /// starvation; fully silent callbacks are expected when idle or after a
    /// track ends. Warnings fire on the first partial fill after a clean run,
    /// then every 100th, to stay readable during a sustained dropout.
    pub fn record(&self, samples_read: usize, samples_requested: usize) -> bool {
        if samples_read == 0 {
            // Saturate at the threshold: no wraparound, no further increments.
            // Single writer (RT thread), so load/store is race-free.
            let silent = self.silent_callbacks.load(Ordering::Relaxed);
            if silent < DRAINED_AFTER_SILENT_CALLBACKS {
                self.silent_callbacks.store(silent + 1, Ordering::Relaxed);
            }
            self.underrun_streak.store(0, Ordering::Relaxed);
            return false;
        }
        self.silent_callbacks.store(0, Ordering::Relaxed);
        if samples_read < samples_requested {
            let streak = self.underrun_streak.fetch_add(1, Ordering::Relaxed) + 1;
            streak == 1 || streak.is_multiple_of(100)
        } else {
            self.underrun_streak.store(0, Ordering::Relaxed);
            false
        }
    }

    /// True once [`DRAINED_AFTER_SILENT_CALLBACKS`] consecutive fully silent
    /// callbacks were recorded.
    pub fn is_drained(&self) -> bool {
        self.silent_callbacks.load(Ordering::Relaxed) >= DRAINED_AFTER_SILENT_CALLBACKS
    }

    fn underrun_streak(&self) -> u32 {
        self.underrun_streak.load(Ordering::Relaxed)
    }
}

/// Outcome of one [`render`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendered {
    /// Paused: `out` is zeroed, the ring untouched, the monitor not updated.
    Paused,
    /// Played: `samples` were popped into `out` (the rest is for the caller to
    /// treat as silence). `underrun_warn` is the monitor's verdict for this
    /// callback.
    Played { samples: usize, underrun_warn: bool },
}

/// Fill `out` from `consumer` for one callback and update `monitor`.
///
/// A pending flush is always serviced first, even while paused. If `paused`
/// is set, `out` is zeroed, the consumer is left untouched and
/// [`PlayoutMonitor::record`] is NOT called, so the silence counter stays
/// frozen and a paused track never counts as drained. Otherwise the callback
/// is recorded against `out.len()` requested samples.
///
/// PipeWire-independent and RT-safe (no allocation, no locks).
pub fn render(
    consumer: &mut HeapConsumer<f32>,
    out: &mut [f32],
    monitor: &PlayoutMonitor,
    paused: &AtomicBool,
) -> Rendered {
    monitor.service_flush(consumer);
    if paused.load(Ordering::Acquire) {
        out.fill(0.0);
        return Rendered::Paused;
    }
    let samples = consumer.pop_slice(out);
    let underrun_warn = monitor.record(samples, out.len());
    Rendered::Played {
        samples,
        underrun_warn,
    }
}

/// How long [`PipewireOutput::flush`] waits for the callback to acknowledge.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(100);
const FLUSH_POLL: Duration = Duration::from_millis(1);

pub struct PipewireOutput {
    // Thread handle to keep the PipeWire thread alive
    pw_thread: Option<thread::JoinHandle<Result<(), pw::Error>>>,
    command_sender: pw::channel::Sender<StreamCommand>,
    monitor: Arc<PlayoutMonitor>,
}

impl PipewireOutput {
    pub fn new(
        sample_consumer: HeapConsumer<f32>,
        paused: Arc<AtomicBool>,
        sample_rate: u32,
        target_device: Option<&str>,
    ) -> Result<Self, pw::Error> {
        let (cmd_sender, cmd_receiver) = pw::channel::channel::<StreamCommand>();
        let monitor = Arc::new(PlayoutMonitor::new());
        let monitor_for_cb = monitor.clone();

        // Clone target_device into an owned String so it can move into the thread.
        let target_device: Option<String> = target_device.map(|s| s.to_owned());

        // Create a ring buffer for audio samples
        info!("create pipe wire thread");
        // Spawn PipeWire thread
        let pw_thread = thread::spawn(move || {
            pw::init();
            let mainloop = pw::main_loop::MainLoop::new(None)?;
            let context = pw::context::Context::new(&mainloop)?;
            let core = context.connect(None)?;

            // The consumer is owned exclusively by the RT callback's user data.
            // On drop (after the stream is disconnected, so the callback can no
            // longer run) it is handed back through `consumer_return_tx`.
            let (consumer_return_tx, consumer_return_rx) = mpsc::channel::<HeapConsumer<f32>>();

            struct UserData {
                consumer: Option<HeapConsumer<f32>>,
                consumer_return: mpsc::Sender<HeapConsumer<f32>>,
                /// Drain / underrun / flush bookkeeping shared with `PipewireOutput`.
                monitor: Arc<PlayoutMonitor>,
                /// Engine-owned pause flag: while set the callback emits silence
                /// and leaves the ring untouched.
                paused: Arc<AtomicBool>,
                /// Preallocated read buffer; grown only if a larger quantum arrives.
                scratch: Vec<f32>,
            }

            impl Drop for UserData {
                fn drop(&mut self) {
                    if let Some(c) = self.consumer.take() {
                        let _ = self.consumer_return.send(c);
                    }
                }
            }

            let user_data = UserData {
                consumer: Some(sample_consumer),
                consumer_return: consumer_return_tx,
                monitor: monitor_for_cb,
                paused,
                scratch: vec![0.0f32; SCRATCH_FRAMES * DEFAULT_CHANNELS as usize],
            };

            let stream = if let Some(ref name) = target_device {
                Rc::new(pw::stream::Stream::new(
                    &core,
                    "mdma-audio-output",
                    properties! {
                        *pw::keys::MEDIA_TYPE => "Audio",
                        *pw::keys::MEDIA_ROLE => "Music",
                        *pw::keys::MEDIA_CATEGORY => "Playback",
                        *pw::keys::AUDIO_CHANNELS => "2",
                        "target.object" => name.as_str(),
                    },
                )?)
            } else {
                Rc::new(pw::stream::Stream::new(
                    &core,
                    "mdma-audio-output",
                    properties! {
                        *pw::keys::MEDIA_TYPE => "Audio",
                        *pw::keys::MEDIA_ROLE => "Music",
                        *pw::keys::MEDIA_CATEGORY => "Playback",
                        *pw::keys::AUDIO_CHANNELS => "2",
                    },
                )?)
            };

            // Held alive until the mainloop exits; dropping it would detach the process callback.
            let listener = stream
                .add_local_listener_with_user_data(user_data)
                .process(|stream, user_data| match stream.dequeue_buffer() {
                    None => tracing::warn!("No buffer received"),
                    Some(mut buffer) => {
                        let datas = buffer.datas_mut();
                        let stride = CHAN_SIZE * DEFAULT_CHANNELS as usize;
                        let data = &mut datas[0];

                        let n_frames = if let Some(slice) = data.data() {
                            let n_frames = slice.len() / stride;

                            let requested = n_frames * DEFAULT_CHANNELS as usize;
                            if user_data.scratch.len() < requested {
                                warn!(
                                    requested,
                                    capacity = user_data.scratch.len(),
                                    "pw quantum larger than scratch buffer; growing (allocates in RT callback)"
                                );
                                user_data.scratch.resize(requested, 0.0);
                            }
                            let f32_buffer = &mut user_data.scratch[..requested];

                            // Services any pending flush, pops, and records on the
                            // monitor (paused callbacks are not recorded).
                            let rendered = match user_data.consumer.as_mut() {
                                Some(consumer) => render(
                                    consumer,
                                    f32_buffer,
                                    &user_data.monitor,
                                    &user_data.paused,
                                ),
                                None => Rendered::Paused,
                            };
                            let samples_read = match rendered {
                                Rendered::Paused => 0,
                                Rendered::Played { samples, .. } => samples,
                            };

                            // Write f32 samples directly to output buffer
                            for i in 0..n_frames {
                                for c in 0..DEFAULT_CHANNELS {
                                    let f32_idx = i * DEFAULT_CHANNELS as usize + c as usize;

                                    let f32_sample = if f32_idx < samples_read {
                                        f32_buffer[f32_idx]
                                    } else {
                                        0.0
                                    };

                                    let start = i * stride + (c as usize * CHAN_SIZE);
                                    let end = start + CHAN_SIZE;
                                    let chan = &mut slice[start..end];
                                    chan.copy_from_slice(&f32_sample.to_le_bytes());
                                }
                            }

                            if matches!(
                                rendered,
                                Rendered::Played {
                                    underrun_warn: true,
                                    ..
                                }
                            ) {
                                warn!(
                                    n_frames,
                                    samples_provided = samples_read,
                                    silence_samples = requested - samples_read,
                                    underrun_streak = user_data.monitor.underrun_streak(),
                                    "pw underrun: ring buffer starved"
                                );
                            }

                            n_frames
                        } else {
                            0
                        };

                        let chunk = data.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.stride_mut() = stride as _;
                        *chunk.size_mut() = (stride * n_frames) as _;
                    }
                })
                .register()?;

            let mut audio_info = spa::param::audio::AudioInfoRaw::new();
            audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
            audio_info.set_rate(sample_rate);
            audio_info.set_channels(DEFAULT_CHANNELS);
            let mut position = [0; spa::param::audio::MAX_CHANNELS];
            position[0] = spa_sys::SPA_AUDIO_CHANNEL_FL;
            position[1] = spa_sys::SPA_AUDIO_CHANNEL_FR;
            audio_info.set_position(position);

            let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &pw::spa::pod::Value::Object(pw::spa::pod::Object {
                    type_: spa_sys::SPA_TYPE_OBJECT_Format,
                    id: spa_sys::SPA_PARAM_EnumFormat,
                    properties: audio_info.into(),
                }),
            )
            .unwrap()
            .0
            .into_inner();

            let mut params = [Pod::from_bytes(&values).unwrap()];

            let stream_flags = pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS;

            stream.connect(
                spa::utils::Direction::Output,
                None,
                stream_flags,
                &mut params,
            )?;

            // Attach command receiver to the mainloop. The stream is shared via Rc.
            // Held alive until the mainloop exits; dropping it would detach the channel listener.
            let stream_for_cmd = stream.clone();
            let mainloop_for_cmd = mainloop.clone();
            // Main-loop-thread only; filled by Shutdown, answered after the loop exits.
            let shutdown_reply: Rc<RefCell<Option<mpsc::SyncSender<HeapConsumer<f32>>>>> =
                Rc::new(RefCell::new(None));
            let shutdown_reply_for_cmd = shutdown_reply.clone();
            // Set if disconnect failed: the RT callback may still be running.
            let disconnect_failed = Rc::new(Cell::new(false));
            let disconnect_failed_for_cmd = disconnect_failed.clone();
            let cmd_attached = cmd_receiver.attach(mainloop.loop_(), move |cmd| match cmd {
                StreamCommand::Shutdown(tx) => {
                    // Disconnect first: this removes the stream from the data
                    // loop, so the RT callback can no longer run once we drop
                    // its user data (and thus recover the consumer) below.
                    if let Err(e) = stream_for_cmd.disconnect() {
                        tracing::error!("Shutdown: stream.disconnect() failed: {}", e);
                        disconnect_failed_for_cmd.set(true);
                    }
                    *shutdown_reply_for_cmd.borrow_mut() = Some(tx);
                    mainloop_for_cmd.quit();
                }
                StreamCommand::Flush => {
                    // The mixer ring itself is drained by the RT callback (it owns
                    // the consumer); see `PipewireOutput::flush`. Here we only
                    // tell PipeWire to drop its own in-flight buffers
                    // (drain=false = discard).
                    if let Err(e) = stream_for_cmd.flush(false) {
                        tracing::warn!("Flush: stream.flush(false) failed: {}", e);
                    }
                }
            });

            mainloop.run();

            // Stream is disconnected on Shutdown; dropping the listener drops the
            // user data, which returns the consumer through the channel.
            if disconnect_failed.get() {
                // The callback may still be running; dropping its user data
                // would be a use-after-free. Leak it and report no consumer
                // (dropping the reply sender makes `shutdown()` return None).
                // Also leak everything the callback or the command closure
                // (which holds stream/mainloop clones) may touch.
                tracing::error!("Shutdown: leaking stream state (disconnect failed)");
                std::mem::forget(listener);
                std::mem::forget(cmd_attached);
                std::mem::forget(stream);
                std::mem::forget(core);
                std::mem::forget(context);
                std::mem::forget(mainloop);
                shutdown_reply.borrow_mut().take();
                return Ok(());
            }
            drop(listener);
            if let Some(tx) = shutdown_reply.borrow_mut().take() {
                if let Ok(consumer) = consumer_return_rx.try_recv() {
                    let _ = tx.send(consumer);
                }
            }
            Ok(())
        });

        Ok(Self {
            pw_thread: Some(pw_thread),
            command_sender: cmd_sender,
            monitor,
        })
    }

    /// True when the output has played everything it was given: the last
    /// callback(s) found the ring buffer completely empty. A fresh stream is
    /// not drained. See [`PlayoutMonitor`] for semantics.
    pub fn is_drained(&self) -> bool {
        self.monitor.is_drained()
    }

    /// Drain the mixer output ring buffer and flush PipeWire's in-flight buffers.
    ///
    /// Should be called after `MixerCommand::ClearTrack` and before registering the
    /// next track's consumer so that no old-track audio leaks into the new track's
    /// playback window.
    ///
    /// # Protocol
    ///
    /// The RT callback exclusively owns the ring consumer, so it (not this
    /// thread) discards the samples: `flush` bumps a flush generation in the
    /// [`PlayoutMonitor`], sleep-polls on the *caller* thread (up to 100 ms,
    /// roughly two quanta) until the callback acknowledges, and only then asks
    /// the PW main loop to flush PipeWire's own buffers (so that flush cannot
    /// run before the last old quantum has been queued). The PipeWire flush is
    /// sent even on timeout. The drained count is logged here, off the RT
    /// thread.
    ///
    /// Pausing is done with the engine-owned `paused` flag (the callback keeps
    /// running and emits silence), so flushes are serviced even while paused.
    ///
    /// If the PipeWire thread has already exited there are no callbacks at
    /// all: the flush returns immediately with a warning. If callbacks stall
    /// for another reason we time out, log a warning, and abandon the request
    /// (leaving it pending would discard audio produced *after* this flush).
    pub fn flush(&self) {
        let generation = self.monitor.request_flush();

        let deadline = Instant::now() + FLUSH_TIMEOUT;
        let mut acked = true;
        while !self.monitor.flush_acked(generation) {
            if self.pw_thread.as_ref().is_none_or(|t| t.is_finished()) {
                self.monitor.abandon_flush(generation);
                tracing::warn!("Flush: PipeWire thread has exited; mixer ring not drained");
                return;
            }
            if Instant::now() >= deadline {
                self.monitor.abandon_flush(generation);
                tracing::warn!(
                    "Flush: no callback acknowledged within {:?}; mixer ring not drained",
                    FLUSH_TIMEOUT
                );
                acked = false;
                break;
            }
            thread::sleep(FLUSH_POLL);
        }
        if acked {
            tracing::info!(
                "Flush: drained {} samples from mixer ring",
                self.monitor.last_flush_drained()
            );
        }

        if let Err(e) = self.command_sender.send(StreamCommand::Flush) {
            tracing::warn!("Failed to send Flush to PW thread: {:?}", e);
        }
    }

    /// Shut down the PipeWire thread and recover the ring-buffer consumer.
    ///
    /// Sends `Shutdown` to the PW thread which quits the mainloop and sends the
    /// `HeapConsumer` back via a rendezvous channel.  Returns `Some(consumer)` on
    /// success, `None` if the thread does not respond within 1 second.
    pub fn shutdown(mut self) -> Option<HeapConsumer<f32>> {
        // We use capacity=1 here so the PW thread can send without blocking even
        // if we are briefly delayed reaching recv.
        let (tx, rx) = mpsc::sync_channel::<HeapConsumer<f32>>(1);

        if let Err(e) = self.command_sender.send(StreamCommand::Shutdown(tx)) {
            tracing::warn!("Failed to send Shutdown to PW thread: {:?}", e);
            return None;
        }

        let consumer = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .inspect_err(|e| {
                tracing::warn!("Timeout waiting for PW thread to shut down: {:?}", e);
            })
            .ok();

        // Join the thread regardless of whether we got the consumer back
        if let Some(handle) = self.pw_thread.take() {
            let _ = handle.join();
        }

        consumer
    }
}

/// Safety-net `Drop`: if a `PipewireOutput` is dropped without an explicit
/// `shutdown()` call, send a `Shutdown` without expecting the consumer back
/// (the channel sender is discarded immediately) and join the thread so the
/// PW mainloop exits cleanly rather than being orphaned.
impl Drop for PipewireOutput {
    fn drop(&mut self) {
        if let Some(handle) = self.pw_thread.take() {
            // Capacity-1 channel: PW thread can send the consumer even if we
            // never call recv — but we discard it here because there is nobody
            // left to hand it back to.
            let (tx, _rx) = mpsc::sync_channel::<HeapConsumer<f32>>(1);
            let _ = self.command_sender.send(StreamCommand::Shutdown(tx));
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;

    static PLAYING: AtomicBool = AtomicBool::new(false);

    fn ring_with(n: usize) -> (ringbuf::HeapProducer<f32>, HeapConsumer<f32>) {
        let (mut p, c) = HeapRb::<f32>::new(1024).split();
        for i in 0..n {
            p.push(i as f32).unwrap();
        }
        (p, c)
    }

    /// Samples popped by a render, `None` if it was paused.
    fn popped(r: Rendered) -> Option<usize> {
        match r {
            Rendered::Paused => None,
            Rendered::Played { samples, .. } => Some(samples),
        }
    }

    #[test]
    fn render_without_flush_pops_normally() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let mut out = [0.0f32; 4];
        assert_eq!(
            render(&mut c, &mut out, &m, &PLAYING),
            Rendered::Played {
                samples: 4,
                underrun_warn: false
            }
        );
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(c.len(), 6);
    }

    #[test]
    fn paused_leaves_consumer_untouched_and_output_zeroed() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let paused = AtomicBool::new(true);
        let mut out = [7.0f32; 4];
        assert_eq!(render(&mut c, &mut out, &m, &paused), Rendered::Paused);
        assert_eq!(out, [0.0; 4]);
        assert_eq!(c.len(), 10);
    }

    #[test]
    fn paused_renders_do_not_count_as_drained() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let paused = AtomicBool::new(true);
        let mut out = [0.0f32; 4];
        for _ in 0..(DRAINED_AFTER_SILENT_CALLBACKS * 2) {
            render(&mut c, &mut out, &m, &paused);
        }
        assert!(!m.is_drained(), "paused callbacks must not be recorded");
    }

    #[test]
    fn silent_playing_renders_count_as_drained() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let mut out = [0.0f32; 4];
        for _ in 0..DRAINED_AFTER_SILENT_CALLBACKS {
            render(&mut c, &mut out, &m, &PLAYING);
        }
        assert!(m.is_drained());
    }

    #[test]
    fn serviced_flush_resets_drained() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let mut out = [0.0f32; 4];
        for _ in 0..DRAINED_AFTER_SILENT_CALLBACKS {
            m.record(0, 4);
        }
        assert!(m.is_drained());
        m.request_flush();
        // Service the flush while paused so nothing is recorded afterwards.
        let paused = AtomicBool::new(true);
        render(&mut c, &mut out, &m, &paused);
        assert!(!m.is_drained(), "flush must reset the drain tracking");
    }

    #[test]
    fn underrun_warn_is_reported_through_render() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(1);
        let mut out = [0.0f32; 4];
        assert_eq!(
            render(&mut c, &mut out, &m, &PLAYING),
            Rendered::Played {
                samples: 1,
                underrun_warn: true
            }
        );
    }

    #[test]
    fn flush_is_serviced_while_paused() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let paused = AtomicBool::new(true);
        let generation = m.request_flush();
        let mut out = [7.0f32; 4];
        assert_eq!(render(&mut c, &mut out, &m, &paused), Rendered::Paused);
        assert!(c.is_empty());
        assert!(m.flush_acked(generation));
        assert_eq!(m.last_flush_drained(), 10);
    }

    #[test]
    fn unpausing_resumes_with_next_samples() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let paused = AtomicBool::new(true);
        let mut out = [0.0f32; 4];
        render(&mut c, &mut out, &m, &paused);
        paused.store(false, Ordering::Release);
        assert_eq!(popped(render(&mut c, &mut out, &m, &paused)), Some(4));
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn flush_request_drains_on_next_render_and_acks() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let generation = m.request_flush();
        assert!(!m.flush_acked(generation), "not acked before callback runs");
        let mut out = [0.0f32; 4];
        assert_eq!(
            popped(render(&mut c, &mut out, &m, &PLAYING)),
            Some(0),
            "old audio must not be rendered"
        );
        assert!(c.is_empty());
        assert!(m.flush_acked(generation));
        assert_eq!(m.last_flush_drained(), 10);
    }

    #[test]
    fn flush_with_empty_ring_acks_with_zero() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let generation = m.request_flush();
        let mut out = [0.0f32; 4];
        assert_eq!(popped(render(&mut c, &mut out, &m, &PLAYING)), Some(0));
        assert!(m.flush_acked(generation));
        assert_eq!(m.last_flush_drained(), 0);
    }

    #[test]
    fn flush_is_serviced_once() {
        let m = PlayoutMonitor::new();
        let (mut p, mut c) = ring_with(5);
        m.request_flush();
        let mut out = [0.0f32; 4];
        render(&mut c, &mut out, &m, &PLAYING);
        p.push(9.0).unwrap();
        assert_eq!(
            popped(render(&mut c, &mut out, &m, &PLAYING)),
            Some(1),
            "new audio after flush is kept"
        );
    }

    #[test]
    fn abandoned_flush_does_not_drain_later() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(5);
        let generation = m.request_flush();
        m.abandon_flush(generation);
        let mut out = [0.0f32; 4];
        assert_eq!(popped(render(&mut c, &mut out, &m, &PLAYING)), Some(4));
    }

    #[test]
    fn fresh_monitor_is_not_drained() {
        assert!(!PlayoutMonitor::new().is_drained());
    }

    #[test]
    fn partial_fill_is_not_drained() {
        let m = PlayoutMonitor::new();
        m.record(100, 512);
        assert!(!m.is_drained());
    }

    #[test]
    fn full_fill_is_not_drained() {
        let m = PlayoutMonitor::new();
        m.record(512, 512);
        assert!(!m.is_drained());
    }

    #[test]
    fn single_silent_callback_is_not_drained() {
        let m = PlayoutMonitor::new();
        m.record(512, 512);
        m.record(0, 512);
        assert!(!m.is_drained());
    }

    #[test]
    fn consecutive_silent_callbacks_are_drained() {
        let m = PlayoutMonitor::new();
        m.record(512, 512);
        for _ in 0..DRAINED_AFTER_SILENT_CALLBACKS {
            m.record(0, 512);
        }
        assert!(m.is_drained());
        m.record(0, 512);
        assert!(m.is_drained());
    }

    #[test]
    fn silent_counter_saturates_at_threshold() {
        let m = PlayoutMonitor::new();
        for _ in 0..(DRAINED_AFTER_SILENT_CALLBACKS * 10) {
            m.record(0, 512);
        }
        assert_eq!(
            m.silent_callbacks.load(Ordering::Relaxed),
            DRAINED_AFTER_SILENT_CALLBACKS
        );
    }

    #[test]
    fn data_after_silence_resets_drained() {
        let m = PlayoutMonitor::new();
        for _ in 0..DRAINED_AFTER_SILENT_CALLBACKS {
            m.record(0, 512);
        }
        assert!(m.is_drained());
        m.record(64, 512);
        assert!(!m.is_drained());
        m.record(0, 512);
        assert!(!m.is_drained(), "silence count restarts after data");
    }

    #[test]
    fn underrun_warns_only_on_partial_fills() {
        let m = PlayoutMonitor::new();
        assert!(!m.record(512, 512), "full fill must not warn");
        assert!(!m.record(0, 512), "fully silent must not warn");
        assert!(m.record(10, 512), "first partial fill warns");
        assert!(!m.record(10, 512), "subsequent partial fills are throttled");
    }

    #[test]
    fn underrun_warning_throttled_to_every_hundredth() {
        let m = PlayoutMonitor::new();
        let warns = (0..200).filter(|_| m.record(10, 512)).count();
        assert_eq!(warns, 3); // 1st, 100th, 200th
    }

    #[test]
    fn silence_resets_underrun_streak() {
        let m = PlayoutMonitor::new();
        assert!(m.record(10, 512));
        m.record(0, 512);
        assert!(m.record(10, 512), "streak restarts after silence");
    }
}
