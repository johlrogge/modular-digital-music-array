mod error;
mod mixer;
mod pipewire_output;
mod resampler;
mod source;
mod track;

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

pub use error::PlaybackError;
use mixer::Mixer;
use parking_lot::RwLock;
use pipewire_output::PipewireOutput;
pub use playback_primitives::{Db, Deck, Volume};
use ringbuf::{HeapConsumer, HeapRb};
pub use source::{AudioSource, Source};
use tracing::info;
pub use track::Track;

/// Fixed output sample rate. All sources are upsampled to this rate.
/// Matches the iFi HD USB Audio maximum capability.
const TARGET_RATE: u32 = 192_000;

type Decks = Arc<RwLock<HashMap<Deck, Arc<RwLock<Track>>>>>;

pub struct PlaybackEngine {
    decks: Decks,
    audio_output: Option<PipewireOutput>,
    mixer_consumer: Option<HeapConsumer<f32>>,
    current_sample_rate: Option<u32>,
    command_sender: mpsc::Sender<MixerCommand>,
    _mix_task: Option<std::thread::JoinHandle<()>>,
    stream_active: bool,
    mix_thread_running: Arc<AtomicBool>,
}
enum MixerCommand {
    RegisterTrack {
        deck: Deck,
        consumer: HeapConsumer<f32>,
    },
    SetVolume {
        deck: Deck,
        volume: Volume,
    },
}
impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        // Create a channel for mixer commands - std::sync::mpsc doesn't take a capacity
        let (command_sender, command_receiver) = std::sync::mpsc::channel();

        // Create ringbuffer for mixer output.
        // Sized for ~0.7 s at 192 kHz stereo.
        const MIXER_BUFFER_SIZE: usize = 262_144;
        let mixer_rb = HeapRb::<f32>::new(MIXER_BUFFER_SIZE);
        let (mixer_producer, mixer_consumer) = mixer_rb.split();

        let mix_thread_running = Arc::new(AtomicBool::new(true));
        let running = Arc::clone(&mix_thread_running);

        // Start the mix thread with command receiver
        let mix_task = std::thread::spawn(move || {
            let mut mixer = Mixer::new(mixer_producer);
            let mut consumers = HashMap::<Deck, HeapConsumer<f32>>::new();
            let mut temp_buffer = vec![0.0; 1920 * 2];

            tracing::info!("MIX THREAD: Started, will process audio");

            while running.load(Ordering::Relaxed) {
                // Process any pending commands
                while let Ok(cmd) = command_receiver.try_recv() {
                    match cmd {
                        MixerCommand::RegisterTrack { deck, consumer } => {
                            tracing::info!("MIX THREAD: Registering track for deck {:?}", deck);
                            consumers.insert(deck, consumer);
                        }
                        MixerCommand::SetVolume { deck, volume } => {
                            mixer.set_volume(deck, volume);
                        }
                    }
                }
                let l = temp_buffer.len();

                // Mix audio
                if let Err(e) = mixer.mix(&mut temp_buffer, l, &mut consumers) {
                    tracing::error!("MIX THREAD: Error mixing: {}", e);
                }

                // Sleep briefly
                std::thread::sleep(std::time::Duration::from_micros(500)); // 0.5ms instead of 5ms
            }

            tracing::info!("MIX THREAD: Exiting cleanly");
        });

        // Return the engine — PipeWire output deferred until first track load
        Ok(Self {
            decks: Arc::new(RwLock::new(HashMap::new())),
            audio_output: None,
            mixer_consumer: Some(mixer_consumer),
            current_sample_rate: None,
            command_sender,
            _mix_task: Some(mix_task),
            stream_active: true,
            mix_thread_running,
        })
    }

    /// Signal the mix thread to stop and wait for it to join.
    pub fn shutdown(&mut self) {
        self.mix_thread_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self._mix_task.take() {
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

    pub async fn load_track(&mut self, deck: Deck, path: &Path) -> Result<(), PlaybackError> {
        tracing::info!("Starting track load for deck {:?}", deck);

        // Stop and remove any existing track on this deck
        {
            let mut decks = self.decks.write();
            if let Some(existing) = decks.remove(&deck) {
                tracing::info!("Unloading existing track from deck {:?}", deck);
                existing.write().stop();
            }
        }

        // Create source and read its native sample rate
        let source = AudioSource::new(path)?;
        let source_rate = source.sample_rate();

        // Create PipeWire output on first track load, always at TARGET_RATE.
        // All sources are upsampled by the decoder task regardless of their native rate.
        if self.audio_output.is_none() {
            if let Some(consumer) = self.mixer_consumer.take() {
                info!(
                    "Creating PipeWire output at {}Hz (source: {}Hz)",
                    TARGET_RATE, source_rate
                );
                let audio_output = PipewireOutput::new(consumer, TARGET_RATE)
                    .map_err(|e| PlaybackError::AudioDevice(format!("PipeWire error: {}", e)))?;
                self.audio_output = Some(audio_output);
                self.current_sample_rate = Some(TARGET_RATE);
            }
        }

        // Create ringbuffer for this deck.
        // Sized for ~0.34 s at 192 kHz stereo.
        const BUFFER_SIZE: usize = 131_072;
        let rb = HeapRb::<f32>::new(BUFFER_SIZE);
        let (producer, consumer) = rb.split();

        // Create new track — decoder task will resample source_rate → TARGET_RATE
        let track = Track::new(source, producer, source_rate, TARGET_RATE).await?;
        tracing::info!("Track is ready for playback");

        // Store the track - no lock conflicts possible with mix thread now
        let mut decks = self.decks.write();
        decks.insert(deck, Arc::new(RwLock::new(track)));
        drop(decks);

        // Send consumer to mix thread via command - using standard send, not try_send
        self.command_sender
            .send(MixerCommand::RegisterTrack { deck, consumer })
            .map_err(|_| PlaybackError::TaskCancelled)?;

        tracing::info!("Loaded track from {:?} into deck {:?}", path, deck);
        Ok(())
    }

    pub fn set_volume(&mut self, deck: Deck, volume: Volume) -> Result<(), PlaybackError> {
        match self
            .command_sender
            .send(MixerCommand::SetVolume { deck, volume })
        {
            Ok(_) => {
                tracing::info!("Setting volume for deck {:?} to {}dB", deck, volume.raw());
                Ok(())
            }
            Err(_) => {
                tracing::error!("Failed to send volume command for deck {:?}", deck);
                Err(PlaybackError::TaskCancelled)
            }
        }
    }

    fn find_track(&self, deck: Deck) -> Option<Arc<RwLock<Track>>> {
        let decks = self.decks.read();
        decks.get(&deck).cloned()
    }

    pub fn play(&mut self, deck: Deck) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            tracing::info!("DEBUG PLAY: About to set track to playing state");
            track.write().play();
            tracing::info!("DEBUG PLAY: Track set to playing state");

            Ok(())
        } else {
            tracing::error!("No track loaded in deck {:?}", deck);
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }

    pub fn stop(&mut self, deck: Deck) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            tracing::info!("Stopping deck {:?}", deck);
            track.write().stop();
            Ok(())
        } else {
            tracing::error!("No track loaded in deck {:?}", deck);
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }

    pub fn unload_track(&mut self, deck: Deck) -> Result<(), PlaybackError> {
        let mut decks = self.decks.write();

        // Remove returns the old value if it existed
        match decks.remove(&deck) {
            Some(_) => {
                tracing::info!("Unloaded track from deck {:?}", deck);
                Ok(())
            }
            None => {
                tracing::info!("No track to unload from deck {:?}", deck);
                Ok(()) // No track is still a success
            }
        }
    }

    pub fn is_track_finished(&self, deck: Deck) -> bool {
        if let Some(track) = self.find_track(deck) {
            track.read().is_finished()
        } else {
            false
        }
    }

    /// Returns the current playback position in milliseconds for the given deck,
    /// or `None` if no track is loaded.
    pub fn position_ms(&self, deck: Deck) -> Option<u64> {
        self.find_track(deck)
            .map(|track| track.read().position_ms())
    }

    /// Returns the total duration in milliseconds for the track on the given deck,
    /// or `None` if no track is loaded. Returns `Some(0)` if duration is unknown.
    pub fn duration_ms(&self, deck: Deck) -> Option<u64> {
        self.find_track(deck)
            .map(|track| track.read().duration_ms())
    }

    /// Returns whether the stream is currently active.
    pub fn is_stream_active(&self) -> bool {
        self.stream_active
    }

    pub fn seek(&mut self, deck: Deck, position: usize) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            tracing::info!("Seeking deck {:?} to position {}", deck, position);
            let mut track_guard = track.write();
            track_guard.seek(position)
        } else {
            tracing::error!("No track loaded in deck {:?}", deck);
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.mix_thread_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self._mix_task.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that set_stream_active only flips the flag when the value changes.
    #[test]
    fn set_stream_active_tracks_state() {
        let mut engine = PlaybackEngine::new().expect("engine created");
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
        let mut engine = PlaybackEngine::new().expect("engine created");
        assert!(engine._mix_task.is_some());
        engine.shutdown();
        assert!(engine._mix_task.is_none());
    }
}
