pub mod audio_config;
mod error;
mod mixer;
mod track;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

pub use audio_config::{load_audio_config, save_audio_config, AudioOutputConfig};
pub use audio_decoder::{AudioSource, DecoderError, Source};
pub use audio_output::pipewire_devices::{self, AudioSink};
pub use audio_output::{AudioOutputError, PipewireOutput};
pub use audio_types::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};
pub use error::PlaybackError;
use mixer::Mixer;
use playback_primitives::Db;
pub use playback_primitives::Volume;
use ringbuf::{HeapConsumer, HeapRb};
use tracing::info;
pub use track::Track;

use audio_output::pipewire_devices::list_sinks;

/// Track ring buffer size in f32 samples. Stereo: divide by 2 for frames;
/// divide by sample_rate for seconds of pre-decoded audio.
/// 131_072 = ~1.49 s at 44.1 kHz stereo, ~0.34 s at 192 kHz stereo.
const TRACK_BUFFER_SAMPLES: usize = 131_072;

/// Mixer ring buffer size in f32 samples.
/// 262_144 = ~2.97 s at 44.1 kHz stereo, ~0.68 s at 192 kHz stereo.
const MIXER_BUFFER_SAMPLES: usize = 262_144;

pub struct PlaybackEngine {
    track: Option<Track>,
    audio_output: Option<PipewireOutput>,
    mixer_consumer: Option<HeapConsumer<f32>>,
    current_sample_rate: Option<u32>,
    command_sender: mpsc::Sender<MixerCommand>,
    mix_task: Option<std::thread::JoinHandle<()>>,
    stream_active: bool,
    mix_thread_running: Arc<AtomicBool>,
    config_path: PathBuf,
    audio_config: AudioOutputConfig,
}

enum MixerCommand {
    RegisterTrack {
        consumer: HeapConsumer<f32>,
    },
    SetVolume {
        volume: Volume,
    },
    /// Drop the current track consumer without replacing it.
    /// After this, the mixer stops pulling samples until a RegisterTrack arrives.
    ClearTrack,
}

/// Compute the PipeWire output rate for a track.
///
/// Passes the source's native sample rate through directly so no resampling
/// is required. Falls back to `fallback_rate` only when `source_rate` is 0
/// (defensive; Symphonia should never produce this, but we guard against it).
/// If PipeWire rejects an unusual rate that is a real error surfaced upward —
/// we do not silently mask it with a clamp.
pub fn select_target_rate(source_rate: u32, fallback_rate: u32) -> u32 {
    if source_rate == 0 {
        fallback_rate
    } else {
        source_rate
    }
}

impl PlaybackEngine {
    pub fn new(config_path: PathBuf) -> Result<Self, PlaybackError> {
        let audio_config = load_audio_config(&config_path)?;

        let (command_sender, command_receiver) = std::sync::mpsc::channel();

        // Create ringbuffer for mixer output.
        let mixer_rb = HeapRb::<f32>::new(MIXER_BUFFER_SAMPLES);
        let (mixer_producer, mixer_consumer) = mixer_rb.split();

        let mix_thread_running = Arc::new(AtomicBool::new(true));
        let running = Arc::clone(&mix_thread_running);

        // Start the mix thread with command receiver
        let mix_task = std::thread::spawn(move || {
            let mut mixer = Mixer::new(mixer_producer);
            let mut consumer: Option<HeapConsumer<f32>> = None;
            let mut temp_buffer = vec![0.0; 1920 * 2];

            tracing::info!("MIX THREAD: Started, will process audio");

            while running.load(Ordering::Acquire) {
                // Process any pending commands
                while let Ok(cmd) = command_receiver.try_recv() {
                    match cmd {
                        MixerCommand::RegisterTrack {
                            consumer: new_consumer,
                        } => {
                            tracing::info!("MIX THREAD: Registering new track");
                            consumer = Some(new_consumer);
                        }
                        MixerCommand::SetVolume { volume } => {
                            mixer.set_volume(volume);
                        }
                        MixerCommand::ClearTrack => {
                            tracing::info!("MIX THREAD: Clearing track consumer");
                            consumer = None;
                        }
                    }
                }
                let l = temp_buffer.len();

                if let Err(e) = mixer.mix(&mut temp_buffer, l, &mut consumer) {
                    tracing::error!("MIX THREAD: Error mixing: {}", e);
                }

                std::thread::sleep(std::time::Duration::from_micros(500));
            }

            tracing::info!("MIX THREAD: Exiting cleanly");
        });

        Ok(Self {
            track: None,
            audio_output: None,
            mixer_consumer: Some(mixer_consumer),
            current_sample_rate: None,
            command_sender,
            mix_task: Some(mix_task),
            stream_active: true,
            mix_thread_running,
            config_path,
            audio_config,
        })
    }

    /// Signal the mix thread to stop and wait for it to join.
    pub fn shutdown(&mut self) {
        self.mix_thread_running.store(false, Ordering::Release);
        if let Some(handle) = self.mix_task.take() {
            let _ = handle.join();
        }
    }

    /// Activate or deactivate the PipeWire stream.
    /// Only sends the command when the state actually changes.
    pub fn set_stream_active(&mut self, active: bool) {
        if self.stream_active != active {
            if let Some(ref output) = self.audio_output {
                output.set_active(active);
            }
            self.stream_active = active;
        }
    }

    /// Enumerate available audio output sinks via pw-dump.
    pub fn list_outputs(&self) -> Result<Vec<AudioSink>, PlaybackError> {
        list_sinks().map_err(PlaybackError::AudioOutput)
    }

    /// Return the current audio output configuration.
    pub fn get_output(&self) -> &AudioOutputConfig {
        &self.audio_config
    }

    /// Select a named audio output device.
    ///
    /// Looks up the device in the live sink list to obtain its maximum sample rate,
    /// persists the new config, then hot-swaps the PipeWire output: if one is
    /// currently active its consumer is recovered via `shutdown()` and a new output
    /// is created immediately on the new device.
    pub fn set_output(&mut self, device_name: String) -> Result<AudioOutputConfig, PlaybackError> {
        let sinks = list_sinks()?;
        let sink = sinks
            .iter()
            .find(|s| s.name == device_name)
            .ok_or_else(|| {
                PlaybackError::AudioOutput(audio_output::AudioOutputError::AudioDevice(format!(
                    "Audio device not found: {device_name}"
                )))
            })?;

        let new_config = AudioOutputConfig {
            device_name: Some(device_name.clone()),
            sample_rate: sink.max_sample_rate,
        };

        save_audio_config(&self.config_path, &new_config)?;

        if let Some(current_rate) = self.current_sample_rate {
            if current_rate != sink.max_sample_rate {
                tracing::info!(
                    "Sample rate change from {}Hz to {}Hz takes effect on next track load",
                    current_rate,
                    sink.max_sample_rate
                );
            }
        }

        let recovered_consumer = if let Some(old_output) = self.audio_output.take() {
            old_output.shutdown()
        } else {
            None
        };

        self.audio_config = new_config.clone();

        if let Some(consumer) = recovered_consumer {
            tracing::info!(
                "Hot-swapping PipeWire output to device {:?} at {}Hz",
                new_config.device_name,
                new_config.sample_rate
            );
            let audio_output =
                PipewireOutput::new(consumer, new_config.sample_rate, Some(device_name.as_str()))
                    .map_err(|e| {
                    PlaybackError::AudioOutput(audio_output::AudioOutputError::AudioDevice(
                        format!("PipeWire error: {}", e),
                    ))
                })?;
            self.audio_output = Some(audio_output);
        }

        Ok(new_config)
    }

    pub async fn load_track(&mut self, path: &Path) -> Result<(), PlaybackError> {
        tracing::info!("Starting track load");

        // Stop and remove any existing track
        if let Some(ref mut t) = self.track {
            tracing::info!("Unloading existing track");
            t.stop();
        }
        self.track = None;

        // If audio was flowing, clear the mixer consumer so no old samples are produced,
        // then flush the mixer output ring and PipeWire's in-flight buffers.
        // This reduces skip latency from ~3 s (full ring drain) to ~50 ms.
        if self.audio_output.is_some() {
            self.command_sender
                .send(MixerCommand::ClearTrack)
                .map_err(|_| PlaybackError::TaskCancelled)?;
            if let Some(ref output) = self.audio_output {
                output.flush();
            }
        }

        // Create source and read its native sample rate
        let source = AudioSource::new(path)?;
        let source_rate = source.sample_rate();

        let max_rate = self.audio_config.sample_rate;
        let target_rate = select_target_rate(source_rate, max_rate);
        let target_device = self.audio_config.device_name.clone();

        // (Re-)create the PipeWire output when needed:
        //   • no output yet (first track load), OR
        //   • the new track's native rate differs from the current output rate.
        // When the rate changes, shut down the existing output, recover its
        // ring-buffer consumer, then spin up a new output at the track's rate.
        let need_new_output = match self.current_sample_rate {
            None => true,
            Some(current) => current != target_rate,
        };

        if need_new_output {
            // Recover the consumer from the old output (if any) so we can
            // hand it to the new one without touching the mix thread.
            if let Some(old_output) = self.audio_output.take() {
                if let Some(consumer) = old_output.shutdown() {
                    self.mixer_consumer = Some(consumer);
                } else {
                    tracing::warn!(
                        "load_track: failed to recover consumer from old PipeWire output"
                    );
                }
            }

            if self.mixer_consumer.is_none() {
                tracing::error!(
                    "load_track: no mixer consumer available and no audio output — audio will be silent"
                );
            }
            if let Some(consumer) = self.mixer_consumer.take() {
                info!(
                    "Creating PipeWire output at {}Hz (source: {}Hz, device: {:?})",
                    target_rate, source_rate, target_device
                );
                let audio_output =
                    PipewireOutput::new(consumer, target_rate, target_device.as_deref()).map_err(
                        |e| {
                            PlaybackError::AudioOutput(audio_output::AudioOutputError::AudioDevice(
                                format!("PipeWire error: {}", e),
                            ))
                        },
                    )?;
                self.audio_output = Some(audio_output);
                self.current_sample_rate = Some(target_rate);
            }
        }

        // Create ringbuffer for this track.
        let rb = HeapRb::<f32>::new(TRACK_BUFFER_SAMPLES);
        let (producer, consumer) = rb.split();

        // Create new track — decoder task will resample source_rate → target_rate
        let track = Track::new(source, producer, source_rate, target_rate)?;
        tracing::info!("Track is ready for playback");

        // Store the track and register its consumer with the mix thread
        self.track = Some(track);
        self.command_sender
            .send(MixerCommand::RegisterTrack { consumer })
            .map_err(|_| PlaybackError::TaskCancelled)?;

        tracing::info!("Loaded track from {:?}", path);
        Ok(())
    }

    pub fn set_volume(&mut self, volume: Volume) -> Result<(), PlaybackError> {
        match self.command_sender.send(MixerCommand::SetVolume { volume }) {
            Ok(_) => {
                tracing::debug!("Setting volume to {}dB", volume.raw());
                Ok(())
            }
            Err(_) => {
                tracing::error!("Failed to send volume command");
                Err(PlaybackError::TaskCancelled)
            }
        }
    }

    pub fn play(&mut self) -> Result<(), PlaybackError> {
        if let Some(ref mut track) = self.track {
            tracing::debug!("Setting track to playing state");
            track.play();
            Ok(())
        } else {
            tracing::error!("No track loaded");
            Err(PlaybackError::NoTrackLoaded)
        }
    }

    pub fn stop(&mut self) -> Result<(), PlaybackError> {
        if let Some(ref mut track) = self.track {
            tracing::info!("Stopping track");
            track.stop();
            Ok(())
        } else {
            tracing::error!("No track loaded");
            Err(PlaybackError::NoTrackLoaded)
        }
    }

    pub fn unload_track(&mut self) -> Result<(), PlaybackError> {
        if self.track.is_some() {
            self.track = None;
            tracing::info!("Unloaded track");
        } else {
            tracing::info!("No track to unload");
        }
        Ok(())
    }

    pub fn is_track_finished(&self) -> bool {
        self.track
            .as_ref()
            .map(|t| t.is_finished())
            .unwrap_or(false)
    }

    /// Returns the current playback position in milliseconds, or `None` if no track is loaded.
    pub fn position_ms(&self) -> Option<u64> {
        self.track.as_ref().map(|t| t.position_ms())
    }

    /// Returns the total duration in milliseconds for the loaded track,
    /// or `None` if no track is loaded. Returns `Some(0)` if duration is unknown.
    pub fn duration_ms(&self) -> Option<u64> {
        self.track.as_ref().map(|t| t.duration_ms())
    }

    /// Returns whether the stream is currently active.
    pub fn is_stream_active(&self) -> bool {
        self.stream_active
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        if let Some(ref mut track) = self.track {
            tracing::info!("Seeking to position {}", position);
            track.seek(position)
        } else {
            tracing::error!("No track loaded");
            Err(PlaybackError::NoTrackLoaded)
        }
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.mix_thread_running.store(false, Ordering::Release);
        if let Some(handle) = self.mix_task.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with_tempdir() -> (PlaybackEngine, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("audio_config.json");
        let engine = PlaybackEngine::new(config_path).expect("engine created");
        (engine, tmp)
    }

    /// Verify that set_stream_active only flips the flag when the value changes.
    #[test]
    fn set_stream_active_tracks_state() {
        let (mut engine, _tmp) = engine_with_tempdir();
        assert!(engine.is_stream_active(), "should start active");

        engine.set_stream_active(false);
        assert!(
            !engine.is_stream_active(),
            "should be inactive after deactivate"
        );

        // Calling again with same value should be idempotent.
        engine.set_stream_active(false);
        assert!(!engine.is_stream_active(), "should remain inactive");

        engine.set_stream_active(true);
        assert!(
            engine.is_stream_active(),
            "should be active after reactivate"
        );
    }

    /// Verify that the mix thread stops cleanly when shutdown() is called.
    #[test]
    fn shutdown_stops_mix_thread() {
        let (mut engine, _tmp) = engine_with_tempdir();
        assert!(engine.mix_task.is_some());
        engine.shutdown();
        assert!(engine.mix_task.is_none());
    }

    /// Verify that a missing config file results in the default config being used.
    #[test]
    fn new_with_missing_config_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("nonexistent.json");
        let engine = PlaybackEngine::new(config_path).expect("engine created");
        let config = engine.get_output();
        assert_eq!(config.device_name, None);
        assert_eq!(config.sample_rate, 192_000);
    }

    /// Verify that a pre-existing config file is loaded on construction.
    #[test]
    fn new_loads_existing_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("audio_config.json");
        let saved = AudioOutputConfig {
            device_name: Some("test-device".to_string()),
            sample_rate: 48_000,
        };
        save_audio_config(&config_path, &saved).expect("save");

        let engine = PlaybackEngine::new(config_path).expect("engine created");
        assert_eq!(engine.get_output(), &saved);
    }

    /// Verify that when mixer_consumer is None and audio_output is None,
    /// the engine starts with a consumer available (the normal path), and that
    /// taking the consumer leaves it as None (simulating the post-hot-swap state
    /// where audio_output is Some and mixer_consumer is None).
    #[test]
    fn mixer_consumer_is_some_after_new_and_none_after_take() {
        let (mut engine, _tmp) = engine_with_tempdir();
        // Fresh engine: consumer present, no output yet
        assert!(
            engine.mixer_consumer.is_some(),
            "mixer_consumer should be Some after new()"
        );
        assert!(
            engine.audio_output.is_none(),
            "audio_output should be None after new()"
        );

        // Manually drain it to represent the post-spawn state.
        let _ = engine.mixer_consumer.take();
        assert!(
            engine.mixer_consumer.is_none(),
            "mixer_consumer should be None after take()"
        );
    }

    /// play() with no track loaded returns NoTrackLoaded error.
    #[test]
    fn play_with_no_track_returns_error() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let result = engine.play();
        assert!(
            matches!(result, Err(PlaybackError::NoTrackLoaded)),
            "expected NoTrackLoaded, got {:?}",
            result
        );
    }

    /// stop() with no track loaded returns NoTrackLoaded error.
    #[test]
    fn stop_with_no_track_returns_error() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let result = engine.stop();
        assert!(
            matches!(result, Err(PlaybackError::NoTrackLoaded)),
            "expected NoTrackLoaded, got {:?}",
            result
        );
    }

    /// unload_track() with no track loaded returns Ok (no-op).
    #[test]
    fn unload_track_with_no_track_is_ok() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let result = engine.unload_track();
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
    }

    /// is_track_finished() returns false when no track is loaded.
    #[test]
    fn is_track_finished_no_track() {
        let (engine, _tmp) = engine_with_tempdir();
        assert!(!engine.is_track_finished());
    }

    /// position_ms() returns None when no track is loaded.
    #[test]
    fn position_ms_no_track() {
        let (engine, _tmp) = engine_with_tempdir();
        assert_eq!(engine.position_ms(), None);
    }

    /// duration_ms() returns None when no track is loaded.
    #[test]
    fn duration_ms_no_track() {
        let (engine, _tmp) = engine_with_tempdir();
        assert_eq!(engine.duration_ms(), None);
    }

    /// select_target_rate passes source rate through without clamping (no 192 kHz ceiling).
    #[test]
    fn select_target_rate_passes_through_high_rate() {
        assert_eq!(select_target_rate(384_000, 192_000), 384_000);
    }

    /// select_target_rate passes through source rate when below default.
    #[test]
    fn select_target_rate_passes_through_44100() {
        assert_eq!(select_target_rate(44_100, 192_000), 44_100);
    }

    /// select_target_rate passes through 96 kHz (common hi-res rate).
    #[test]
    fn select_target_rate_passes_through_96000() {
        assert_eq!(select_target_rate(96_000, 192_000), 96_000);
    }

    /// select_target_rate falls back to default when source rate is zero.
    #[test]
    fn select_target_rate_zero_source_falls_back_to_max() {
        assert_eq!(select_target_rate(0, 192_000), 192_000);
    }

    /// select_target_rate passes through exact default rate.
    #[test]
    fn select_target_rate_at_ceiling_is_unchanged() {
        assert_eq!(select_target_rate(192_000, 192_000), 192_000);
    }

    /// After sending ClearTrack, the mix thread stops producing samples.
    /// We verify by registering a consumer with data, sending ClearTrack,
    /// letting the mix thread run briefly, then checking the mixer output ring
    /// is still empty (because the consumer was cleared before the mixer ran).
    #[test]
    fn clear_track_command_stops_mix_thread_producing_samples() {
        use ringbuf::HeapRb;
        use std::time::Duration;

        // Build a standalone mix thread replicating the engine's mix loop
        // so we can inspect the mixer output consumer directly.
        let mixer_rb = HeapRb::<f32>::new(262_144);
        let (mixer_producer, mut mixer_output) = mixer_rb.split();

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<MixerCommand>();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let thread = std::thread::spawn(move || {
            let mut mixer = mixer::Mixer::new(mixer_producer);
            let mut consumer: Option<HeapConsumer<f32>> = None;
            let mut temp_buffer = vec![0.0f32; 1920 * 2];

            while running_clone.load(Ordering::Acquire) {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        MixerCommand::RegisterTrack { consumer: new } => {
                            consumer = Some(new);
                        }
                        MixerCommand::ClearTrack => {
                            consumer = None;
                        }
                        MixerCommand::SetVolume { .. } => {}
                    }
                }
                let l = temp_buffer.len();
                let _ = mixer.mix(&mut temp_buffer, l, &mut consumer);
                std::thread::sleep(Duration::from_micros(500));
            }
        });

        // Push samples into a track ring buffer and register it
        let track_rb = HeapRb::<f32>::new(131_072);
        let (mut track_producer, track_consumer) = track_rb.split();
        for _ in 0..1024 {
            track_producer.push(1.0f32).ok();
        }
        cmd_tx
            .send(MixerCommand::RegisterTrack {
                consumer: track_consumer,
            })
            .unwrap();

        // Let the mix thread run a few cycles so it starts consuming
        std::thread::sleep(Duration::from_millis(5));

        // Now send ClearTrack and drain any samples that already landed
        cmd_tx.send(MixerCommand::ClearTrack).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let before_len = mixer_output.len();
        // Drain whatever was produced before/during the clear
        let mut drain = vec![0.0f32; 262_144];
        mixer_output.pop_slice(&mut drain);

        // Wait another round — mix thread should not produce any more samples
        std::thread::sleep(Duration::from_millis(5));

        let after_len = mixer_output.len();

        running.store(false, Ordering::Release);
        thread.join().unwrap();

        assert_eq!(
            after_len, 0,
            "after ClearTrack, mix thread must produce 0 new samples (had {} before drain)",
            before_len
        );
    }
}
