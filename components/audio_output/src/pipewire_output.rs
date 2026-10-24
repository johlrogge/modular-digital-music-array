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
    /// The main-loop handler signals the sender once `stream.flush(false)` has
    /// returned, so the caller can wait for the flush to be done.
    Flush(mpsc::SyncSender<()>),
}

impl std::fmt::Debug for StreamCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamCommand::Shutdown(_) => write!(f, "Shutdown(..)"),
            StreamCommand::Flush(_) => write!(f, "Flush(..)"),
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
    /// A partial fill awaits judgement by the next callback (see `record`).
    pending_underrun: AtomicBool,
    pending_provided: AtomicUsize,
    pending_requested: AtomicUsize,
    /// Snapshot of the underrun last returned as a warning.
    reported_streak: AtomicU32,
    reported_provided: AtomicUsize,
    reported_requested: AtomicUsize,
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
    /// Returns the number of samples discarded (0 if no flush was pending).
    fn service_flush(&self, consumer: &mut HeapConsumer<f32>) -> usize {
        let requested = self.flush_requested.load(Ordering::Acquire);
        if self.flush_completed.load(Ordering::Acquire) >= requested {
            return 0;
        }
        let drained = consumer.clear();
        // The ring was emptied by a flush, not played out: restart drain and
        // underrun tracking. RT thread is the single writer of both counters.
        self.silent_callbacks.store(0, Ordering::Relaxed);
        self.underrun_streak.store(0, Ordering::Relaxed);
        self.pending_underrun.store(false, Ordering::Relaxed);
        self.flush_drained.store(drained, Ordering::Release);
        self.flush_completed.fetch_max(requested, Ordering::AcqRel);
        drained
    }

    /// Record one callback. Returns `true` if a mid-stream underrun warning
    /// should be logged; the details are then in
    /// [`last_underrun`](Self::last_underrun).
    ///
    /// A *partial* fill (`0 < samples_read < samples_requested`) is only a
    /// real underrun if audio keeps coming afterwards. At the end of a track
    /// the last buffer is always partial and is followed by silence, which is
    /// expected. So a partial fill is held as pending and survives any number
    /// of fully silent callbacks. It is settled by the next data-delivering
    /// callback (full or partial) => a real underrun, reported now; or by a
    /// flush (every new load flushes) => end of stream, no warning.
    /// Reports fire for the first underrun after a clean run, then every
    /// 100th, to stay readable during a sustained dropout.
    pub fn record(&self, samples_read: usize, samples_requested: usize) -> bool {
        if samples_read == 0 {
            // Saturate at the threshold: no wraparound, no further increments.
            // Single writer (RT thread), so load/store is race-free.
            let silent = self.silent_callbacks.load(Ordering::Relaxed);
            if silent < DRAINED_AFTER_SILENT_CALLBACKS {
                self.silent_callbacks.store(silent + 1, Ordering::Relaxed);
            }
            // A pending partial fill stays pending: a mid-track stall is
            // partial, silence, then data and must still be reported.
            self.underrun_streak.store(0, Ordering::Relaxed);
            return false;
        }
        self.silent_callbacks.store(0, Ordering::Relaxed);

        // Data arrived: a pending partial fill was a real underrun.
        let mut warn = false;
        if self.pending_underrun.swap(false, Ordering::Relaxed) {
            let streak = self.underrun_streak.fetch_add(1, Ordering::Relaxed) + 1;
            if streak == 1 || streak.is_multiple_of(100) {
                self.reported_streak.store(streak, Ordering::Relaxed);
                self.reported_provided.store(
                    self.pending_provided.load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                self.reported_requested.store(
                    self.pending_requested.load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                warn = true;
            }
        }

        if samples_read < samples_requested {
            self.pending_provided.store(samples_read, Ordering::Relaxed);
            self.pending_requested
                .store(samples_requested, Ordering::Relaxed);
            self.pending_underrun.store(true, Ordering::Relaxed);
        } else {
            self.underrun_streak.store(0, Ordering::Relaxed);
        }
        warn
    }

    /// True once [`DRAINED_AFTER_SILENT_CALLBACKS`] consecutive fully silent
    /// callbacks were recorded.
    pub fn is_drained(&self) -> bool {
        self.silent_callbacks.load(Ordering::Relaxed) >= DRAINED_AFTER_SILENT_CALLBACKS
    }

    /// Details of the most recent underrun for which [`record`](Self::record)
    /// returned `true`.
    pub fn last_underrun(&self) -> UnderrunReport {
        UnderrunReport {
            streak: self.reported_streak.load(Ordering::Relaxed),
            samples_provided: self.reported_provided.load(Ordering::Relaxed),
            samples_requested: self.reported_requested.load(Ordering::Relaxed),
        }
    }
}

/// A real mid-stream underrun: a partial fill that was followed by more audio.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UnderrunReport {
    /// Consecutive underruns up to and including this one.
    pub streak: u32,
    /// Samples the ring could provide in the starved callback.
    pub samples_provided: usize,
    /// Samples the callback asked for.
    pub samples_requested: usize,
}

/// Engine-owned state shared with the RT callback. Owned by the engine (not
/// the output) so it survives output recreation on sample-rate change.
#[derive(Debug, Clone)]
pub struct OutputShared {
    /// While set the callback emits silence and leaves the ring untouched.
    pub paused: Arc<AtomicBool>,
    /// Monotonic count of samples consumed from the ring: played + flushed.
    /// See [`render`].
    pub consumed_total: Arc<AtomicU64>,
    /// Linear gain as `f32::to_bits`. Applied in the callback with a ramp.
    pub gain: Arc<AtomicU32>,
}

impl OutputShared {
    /// Fresh shared state around `paused`: zero consumed, unity gain.
    pub fn new(paused: Arc<AtomicBool>) -> Self {
        Self {
            paused,
            consumed_total: Arc::new(AtomicU64::new(0)),
            gain: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        }
    }
}

/// Multiply interleaved `samples` by a gain ramping linearly per frame from
/// `from` to `to` (last frame lands exactly on `to`). Both 1.0 => untouched.
fn apply_gain(samples: &mut [f32], from: f32, to: f32) {
    if from == 1.0 && to == 1.0 {
        return;
    }
    let ch = DEFAULT_CHANNELS as usize;
    let frames = samples.len().div_ceil(ch);
    if from == to {
        for s in samples.iter_mut() {
            *s *= to;
        }
        return;
    }
    let step = (to - from) / frames as f32;
    for (i, frame) in samples.chunks_mut(ch).enumerate() {
        let g = if i + 1 == frames {
            to
        } else {
            from + step * (i + 1) as f32
        };
        for s in frame {
            *s *= g;
        }
    }
}

/// Frames to fill in a callback: what PipeWire requested, clamped to the
/// mapped buffer capacity; falls back to the full capacity when it reports 0.
fn frames_to_fill(requested: u64, capacity_frames: usize) -> usize {
    if requested == 0 {
        capacity_frames
    } else {
        usize::try_from(requested).map_or(capacity_frames, |r| r.min(capacity_frames))
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
///
/// # Counters and gain
///
/// `shared.consumed_total` grows by every sample popped for playback AND by
/// every sample discarded by a serviced flush: the engine keeps the invariant
/// `produced_total == consumed_total + ring_len`, and a flush throws away
/// samples the mixer already produced, so they must count as consumed. Paused
/// renders add nothing.
///
/// `shared.gain` (linear, f32 bits) is applied to the popped samples. The gain
/// ramps linearly per frame from `*prev_gain` (the gain used by the previous
/// callback) to the current value, ending exactly on it, and `*prev_gain` is
/// updated. A gain of exactly 1.0 (both ends) is a bit-exact passthrough.
pub fn render(
    consumer: &mut HeapConsumer<f32>,
    out: &mut [f32],
    monitor: &PlayoutMonitor,
    shared: &OutputShared,
    prev_gain: &mut f32,
) -> Rendered {
    let flushed = monitor.service_flush(consumer);
    if flushed > 0 {
        shared
            .consumed_total
            .fetch_add(flushed as u64, Ordering::Release);
    }
    if shared.paused.load(Ordering::Acquire) {
        out.fill(0.0);
        return Rendered::Paused;
    }
    let samples = consumer.pop_slice(out);
    if samples > 0 {
        shared
            .consumed_total
            .fetch_add(samples as u64, Ordering::Release);
    }
    let target = f32::from_bits(shared.gain.load(Ordering::Acquire));
    apply_gain(&mut out[..samples], *prev_gain, target);
    *prev_gain = target;
    let underrun_warn = monitor.record(samples, out.len());
    Rendered::Played {
        samples,
        underrun_warn,
    }
}

/// How long [`PipewireOutput::flush`] waits for the callback to acknowledge.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(100);
const FLUSH_POLL: Duration = Duration::from_millis(1);
/// How long [`PipewireOutput::flush`] waits for the PipeWire main loop to
/// acknowledge its own buffer flush.
const PW_FLUSH_TIMEOUT: Duration = Duration::from_millis(100);

pub struct PipewireOutput {
    // Thread handle to keep the PipeWire thread alive
    pw_thread: Option<thread::JoinHandle<Result<(), pw::Error>>>,
    command_sender: pw::channel::Sender<StreamCommand>,
    monitor: Arc<PlayoutMonitor>,
}

impl PipewireOutput {
    pub fn new(
        sample_consumer: HeapConsumer<f32>,
        shared: OutputShared,
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
                /// Engine-owned pause flag, consumed counter and gain.
                shared: OutputShared,
                /// Gain used by the previous callback (ramp start).
                prev_gain: f32,
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
                prev_gain: f32::from_bits(shared.gain.load(Ordering::Acquire)),
                shared,
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
                        let buffer_requested = buffer.requested();
                        let datas = buffer.datas_mut();
                        let stride = CHAN_SIZE * DEFAULT_CHANNELS as usize;
                        let data = &mut datas[0];

                        let requested_frames = buffer_requested;
                        let n_frames = if let Some(slice) = data.data() {
                            let n_frames = frames_to_fill(requested_frames, slice.len() / stride);

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
                                    &user_data.shared,
                                    &mut user_data.prev_gain,
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
                                // Describes the earlier starved callback, which is
                                // only recognised as an underrun now that audio
                                // has resumed.
                                let underrun = user_data.monitor.last_underrun();
                                warn!(
                                    samples_provided = underrun.samples_provided,
                                    silence_samples =
                                        underrun.samples_requested - underrun.samples_provided,
                                    underrun_streak = underrun.streak,
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
                StreamCommand::Flush(done) => {
                    // The mixer ring itself is drained by the RT callback (it owns
                    // the consumer); see `PipewireOutput::flush`. Here we only
                    // tell PipeWire to drop its own in-flight buffers
                    // (drain=false = discard).
                    if let Err(e) = stream_for_cmd.flush(false) {
                        tracing::warn!("Flush: stream.flush(false) failed: {}", e);
                    }
                    // Acknowledge (also after a failure: the caller must not
                    // wait for something that will never happen). The caller
                    // may have given up; a dropped receiver is fine.
                    let _ = done.send(());
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
    /// sent even on timeout, and `flush` then waits (up to
    /// [`PW_FLUSH_TIMEOUT`]) for the main loop to acknowledge it, so it has
    /// completed when `flush` returns: otherwise it could run after the caller
    /// registered the next track and drop that track's first quantum. The
    /// drained count is logged here, off the RT thread.
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

        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        if let Err(e) = self.command_sender.send(StreamCommand::Flush(done_tx)) {
            tracing::warn!("Failed to send Flush to PW thread: {:?}", e);
            return;
        }
        if done_rx.recv_timeout(PW_FLUSH_TIMEOUT).is_err() {
            tracing::warn!(
                "Flush: PipeWire main loop did not acknowledge within {:?}",
                PW_FLUSH_TIMEOUT
            );
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

    /// Shim for the pre-gain tests: unity gain, fresh counters, pause state
    /// read from `paused` on every call. Wraps `super::render`.
    fn render_unity(
        c: &mut HeapConsumer<f32>,
        out: &mut [f32],
        m: &PlayoutMonitor,
        paused: &AtomicBool,
    ) -> Rendered {
        let shared = OutputShared::new(Arc::new(AtomicBool::new(paused.load(Ordering::Acquire))));
        super::render(c, out, m, &shared, &mut 1.0)
    }

    fn shared() -> OutputShared {
        OutputShared::new(Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn consumed_total_counts_popped_samples() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let s = shared();
        let mut out = [0.0f32; 4];
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        assert_eq!(s.consumed_total.load(Ordering::Acquire), 8);
    }

    #[test]
    fn consumed_total_counts_partial_pop() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(3);
        let s = shared();
        let mut out = [0.0f32; 4];
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        assert_eq!(s.consumed_total.load(Ordering::Acquire), 3);
    }

    #[test]
    fn consumed_total_unchanged_when_paused() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let s = shared();
        s.paused.store(true, Ordering::Release);
        let mut out = [0.0f32; 4];
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        assert_eq!(s.consumed_total.load(Ordering::Acquire), 0);
    }

    #[test]
    fn flush_adds_cleared_count_to_consumed_total() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let s = shared();
        m.request_flush();
        let mut out = [0.0f32; 4];
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        assert_eq!(s.consumed_total.load(Ordering::Acquire), 10);
    }

    #[test]
    fn flush_while_paused_adds_cleared_count() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(10);
        let s = shared();
        s.paused.store(true, Ordering::Release);
        m.request_flush();
        let mut out = [0.0f32; 4];
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        assert_eq!(s.consumed_total.load(Ordering::Acquire), 10);
    }

    #[test]
    fn unity_gain_is_bit_exact() {
        let m = PlayoutMonitor::new();
        let (mut p, mut c) = HeapRb::<f32>::new(16).split();
        let vals = [0.1f32, -0.3333333, 0.987654, f32::MIN_POSITIVE];
        for v in vals {
            p.push(v).unwrap();
        }
        let s = shared();
        let mut out = [0.0f32; 4];
        super::render(&mut c, &mut out, &m, &s, &mut 1.0);
        for (a, b) in out.iter().zip(vals) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn constant_gain_scales_samples() {
        let m = PlayoutMonitor::new();
        let (mut p, mut c) = HeapRb::<f32>::new(16).split();
        for _ in 0..8 {
            p.push(1.0).unwrap();
        }
        let s = shared();
        s.gain.store(0.5f32.to_bits(), Ordering::Release);
        let mut prev = 0.5;
        let mut out = [0.0f32; 8];
        super::render(&mut c, &mut out, &m, &s, &mut prev);
        assert!(out.iter().all(|v| (*v - 0.5).abs() < 1e-6));
        assert_eq!(prev, 0.5);
    }

    #[test]
    fn gain_ramps_from_previous_to_new_across_buffer() {
        let m = PlayoutMonitor::new();
        let (mut p, mut c) = HeapRb::<f32>::new(2048).split();
        for _ in 0..1000 {
            p.push(1.0).unwrap();
        }
        let s = shared();
        s.gain.store(0.0f32.to_bits(), Ordering::Release);
        let mut prev = 1.0;
        let mut out = [0.0f32; 1000];
        super::render(&mut c, &mut out, &m, &s, &mut prev);
        assert!(out[0] > 0.99, "first sample near old gain, got {}", out[0]);
        assert!(
            out[999] < 0.01,
            "last sample near new gain, got {}",
            out[999]
        );
        assert!(out.windows(2).all(|w| w[1] <= w[0] + 1e-6), "monotonic");
        assert_eq!(out[0], out[1], "both channels of a frame share gain");
        assert_eq!(prev, 0.0, "previous gain updated");
    }

    #[test]
    fn frames_to_fill_uses_requested_when_nonzero_and_clamps() {
        assert_eq!(frames_to_fill(256, 1024), 256);
        assert_eq!(frames_to_fill(4096, 1024), 1024);
        assert_eq!(frames_to_fill(0, 1024), 1024);
        assert_eq!(frames_to_fill(1024, 1024), 1024);
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
            render_unity(&mut c, &mut out, &m, &PLAYING),
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
        assert_eq!(
            render_unity(&mut c, &mut out, &m, &paused),
            Rendered::Paused
        );
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
            render_unity(&mut c, &mut out, &m, &paused);
        }
        assert!(!m.is_drained(), "paused callbacks must not be recorded");
    }

    #[test]
    fn silent_playing_renders_count_as_drained() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let mut out = [0.0f32; 4];
        for _ in 0..DRAINED_AFTER_SILENT_CALLBACKS {
            render_unity(&mut c, &mut out, &m, &PLAYING);
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
        render_unity(&mut c, &mut out, &m, &paused);
        assert!(!m.is_drained(), "flush must reset the drain tracking");
    }

    #[test]
    fn underrun_warn_is_reported_through_render_once_data_resumes() {
        let m = PlayoutMonitor::new();
        let (mut p, mut c) = ring_with(1);
        let mut out = [0.0f32; 4];
        assert_eq!(
            render_unity(&mut c, &mut out, &m, &PLAYING),
            Rendered::Played {
                samples: 1,
                underrun_warn: false
            }
        );
        for i in 0..4 {
            p.push(i as f32).unwrap();
        }
        assert_eq!(
            render_unity(&mut c, &mut out, &m, &PLAYING),
            Rendered::Played {
                samples: 4,
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
        assert_eq!(
            render_unity(&mut c, &mut out, &m, &paused),
            Rendered::Paused
        );
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
        render_unity(&mut c, &mut out, &m, &paused);
        paused.store(false, Ordering::Release);
        assert_eq!(popped(render_unity(&mut c, &mut out, &m, &paused)), Some(4));
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
            popped(render_unity(&mut c, &mut out, &m, &PLAYING)),
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
        assert_eq!(
            popped(render_unity(&mut c, &mut out, &m, &PLAYING)),
            Some(0)
        );
        assert!(m.flush_acked(generation));
        assert_eq!(m.last_flush_drained(), 0);
    }

    #[test]
    fn flush_is_serviced_once() {
        let m = PlayoutMonitor::new();
        let (mut p, mut c) = ring_with(5);
        m.request_flush();
        let mut out = [0.0f32; 4];
        render_unity(&mut c, &mut out, &m, &PLAYING);
        p.push(9.0).unwrap();
        assert_eq!(
            popped(render_unity(&mut c, &mut out, &m, &PLAYING)),
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
        assert_eq!(
            popped(render_unity(&mut c, &mut out, &m, &PLAYING)),
            Some(4)
        );
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
    fn final_partial_fill_followed_by_silence_and_flush_does_not_warn() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let mut out = [0.0f32; 4];
        assert!(!m.record(512, 512), "full fill must not warn");
        assert!(!m.record(10, 512), "partial fill is not judged yet");
        assert!(!m.record(0, 512), "silence does not settle it");
        // The next load flushes, which settles the pending fill as end of stream.
        m.request_flush();
        let paused = AtomicBool::new(true);
        render_unity(&mut c, &mut out, &m, &paused);
        assert!(
            !m.record(512, 512),
            "end-of-stream partial fill is expected"
        );
    }

    #[test]
    fn partial_fill_then_silence_then_data_warns_once() {
        let m = PlayoutMonitor::new();
        assert!(!m.record(10, 512));
        assert!(!m.record(0, 512));
        assert!(!m.record(0, 512));
        assert!(m.record(512, 512), "mid-track stall is a real underrun");
        assert!(!m.record(512, 512), "reported exactly once");
    }

    #[test]
    fn partial_fill_followed_by_data_warns_with_the_partial_fills_numbers() {
        let m = PlayoutMonitor::new();
        assert!(!m.record(10, 512));
        assert!(m.record(512, 512), "audio resumed: it was a real underrun");
        assert_eq!(
            m.last_underrun(),
            UnderrunReport {
                streak: 1,
                samples_provided: 10,
                samples_requested: 512
            }
        );
    }

    #[test]
    fn underrun_warning_throttled_to_first_and_every_hundredth() {
        let m = PlayoutMonitor::new();
        let warns = (0..200).filter(|_| m.record(10, 512)).count();
        assert_eq!(warns, 2, "199 partials judged so far: streaks 1 and 100");
        assert!(
            m.record(512, 512),
            "200th partial is judged by the next data"
        );
        assert_eq!(m.last_underrun().streak, 200);
    }

    #[test]
    fn silence_resets_underrun_streak() {
        let m = PlayoutMonitor::new();
        m.record(10, 512);
        assert!(m.record(10, 512));
        m.record(0, 512);
        assert!(m.record(512, 512), "pending fill survived the silence");
        assert_eq!(m.last_underrun().streak, 1, "streak restarts after silence");
    }

    #[test]
    fn flush_discards_pending_partial_fill() {
        let m = PlayoutMonitor::new();
        let (_p, mut c) = ring_with(0);
        let mut out = [0.0f32; 4];
        m.record(1, 4);
        m.request_flush();
        let paused = AtomicBool::new(true);
        render_unity(&mut c, &mut out, &m, &paused);
        assert!(!m.record(4, 4), "flushed partial fill must not be reported");
    }
}
