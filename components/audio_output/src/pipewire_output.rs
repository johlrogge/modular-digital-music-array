use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use pipewire as pw;
use pw::{properties::properties, spa};
use ringbuf::HeapConsumer;
use spa::pod::Pod;
use tracing::{debug, info, warn};

pub const DEFAULT_CHANNELS: u32 = 2;
pub const CHAN_SIZE: usize = std::mem::size_of::<f32>();

pub enum StreamCommand {
    SetActive(bool),
    Shutdown(mpsc::SyncSender<HeapConsumer<f32>>),
    /// Drain the mixer output ring buffer (pop-and-discard) and flush
    /// PipeWire's own in-flight buffers.
    Flush,
}

impl std::fmt::Debug for StreamCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamCommand::SetActive(v) => write!(f, "SetActive({v})"),
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
/// A deactivated stream receives no callbacks, so the counter freezes at its
/// last value rather than counting up.
#[derive(Debug, Default)]
pub struct PlayoutMonitor {
    silent_callbacks: AtomicU32,
    underrun_streak: AtomicU32,
}

impl PlayoutMonitor {
    pub fn new() -> Self {
        Self::default()
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

pub struct PipewireOutput {
    // Thread handle to keep the PipeWire thread alive
    pw_thread: Option<thread::JoinHandle<Result<(), pw::Error>>>,
    command_sender: pw::channel::Sender<StreamCommand>,
    monitor: Arc<PlayoutMonitor>,
}

impl PipewireOutput {
    pub fn new(
        sample_consumer: HeapConsumer<f32>,
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

            // Wrap consumer so it can be extracted on shutdown without moving out of UserData
            let shared_consumer: Rc<RefCell<Option<HeapConsumer<f32>>>> =
                Rc::new(RefCell::new(Some(sample_consumer)));

            // Create user data struct to hold consumer
            struct UserData {
                consumer: Rc<RefCell<Option<HeapConsumer<f32>>>>,
                frame_count: usize, // For debugging
                /// Drain / underrun bookkeeping shared with `PipewireOutput`.
                monitor: Arc<PlayoutMonitor>,
                /// Preallocated read buffer; grown only if a larger quantum arrives.
                scratch: Vec<f32>,
            }

            let user_data = UserData {
                consumer: shared_consumer.clone(),
                frame_count: 0,
                monitor: monitor_for_cb,
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
            let _listener = stream
                .add_local_listener_with_user_data(user_data)
                .process(|stream, user_data| match stream.dequeue_buffer() {
                    None => tracing::warn!("No buffer received"),
                    Some(mut buffer) => {
                        let datas = buffer.datas_mut();
                        let stride = CHAN_SIZE * DEFAULT_CHANNELS as usize;
                        let data = &mut datas[0];

                        let n_frames = if let Some(slice) = data.data() {
                            let n_frames = slice.len() / stride;

                            // Log every 100 frames for debugging
                            user_data.frame_count += 1;

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

                            // Read from consumer (if still present — taken on shutdown)
                            let samples_read =
                                if let Some(ref mut consumer) = *user_data.consumer.borrow_mut() {
                                    if user_data.frame_count % 100 == 0 {
                                        debug!(
                                            "Processing {} frames, consumer has {} samples",
                                            n_frames,
                                            consumer.len()
                                        );
                                    }
                                    let n = consumer.pop_slice(f32_buffer);
                                    if user_data.frame_count % 100 == 0 && n > 0 {
                                        debug!("Read {} samples from consumer", n);
                                    }
                                    n
                                } else {
                                    0
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

                            if user_data.monitor.record(samples_read, requested) {
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
            let consumer_for_cmd = shared_consumer.clone();
            let _cmd_receiver = cmd_receiver.attach(mainloop.loop_(), move |cmd| match cmd {
                StreamCommand::SetActive(active) => {
                    if let Err(e) = stream_for_cmd.set_active(active) {
                        tracing::warn!("Failed to set stream active={}: {}", active, e);
                    } else {
                        info!("Stream set_active({})", active);
                    }
                }
                StreamCommand::Shutdown(tx) => {
                    // Take the consumer out before quitting so we can hand it back
                    let consumer = consumer_for_cmd.borrow_mut().take();
                    if let Some(c) = consumer {
                        let _ = tx.send(c);
                    }
                    mainloop_for_cmd.quit();
                }
                StreamCommand::Flush => {
                    // Drain the mixer output ring buffer: discard all pending samples
                    // so old-track audio does not reach PipeWire after a skip.
                    if let Some(ref mut consumer) = *consumer_for_cmd.borrow_mut() {
                        let pending = consumer.len();
                        let mut scratch = vec![0.0f32; pending];
                        consumer.pop_slice(&mut scratch);
                        tracing::info!("Flush: drained {} samples from mixer ring", pending);
                    }
                    // Tell PipeWire to drop its own in-flight buffers (drain=false = discard).
                    if let Err(e) = stream_for_cmd.flush(false) {
                        tracing::warn!("Flush: stream.flush(false) failed: {}", e);
                    }
                }
            });

            mainloop.run();
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
    pub fn flush(&self) {
        if let Err(e) = self.command_sender.send(StreamCommand::Flush) {
            tracing::warn!("Failed to send Flush to PW thread: {:?}", e);
        }
    }

    /// Activate or deactivate the PipeWire stream.
    /// Deactivating causes the DAC indicator to go dark; reactivating resumes output.
    pub fn set_active(&self, active: bool) {
        if let Err(e) = self.command_sender.send(StreamCommand::SetActive(active)) {
            tracing::warn!("Failed to send SetActive({}) to PW thread: {:?}", active, e);
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

    /// Verify that `StreamCommand::Shutdown` carries a `SyncSender` and that the
    /// consumer can be round-tripped through the channel — i.e. the shutdown
    /// protocol compiles and works at the channel level without needing a live
    /// PipeWire daemon.
    #[test]
    fn shutdown_command_round_trips_consumer() {
        let rb = HeapRb::<f32>::new(1024);
        let (mut producer, consumer) = rb.split();

        // Push a sentinel sample so we can verify identity after round-trip
        producer.push(1.0_f32).unwrap();

        let (tx, rx) = mpsc::sync_channel::<HeapConsumer<f32>>(1);

        // Simulate the PW thread sending the consumer back on Shutdown
        tx.send(consumer).expect("send should succeed");

        let recovered = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("should receive consumer within 1 second");

        // The recovered consumer still has the sentinel sample
        assert_eq!(recovered.len(), 1);
    }
}
