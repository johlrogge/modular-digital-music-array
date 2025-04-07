mod error;
mod mixer;
mod pipewire_output;
mod source;
mod track;

use std::{
    collections::HashMap,
    path::Path,
    sync::{mpsc, Arc},
};

pub use error::PlaybackError;
use mixer::Mixer;
use parking_lot::RwLock;
use pipewire_output::PipewireOutput;
pub use playback_primitives::Deck;
use ringbuf::{HeapConsumer, HeapRb};
pub use source::{FlacSource, Source};
use tracing::info;
pub use track::Track;

type Decks = Arc<RwLock<HashMap<Deck, Arc<RwLock<Track>>>>>;

pub struct PlaybackEngine {
    decks: Decks,
    _audio_output: PipewireOutput,
    command_sender: mpsc::Sender<MixerCommand>,
    _mix_task: Option<std::thread::JoinHandle<()>>,
}
enum MixerCommand {
    RegisterTrack {
        deck: Deck,
        consumer: HeapConsumer<f32>,
    },
    SetVolume {
        deck: Deck,
        db: f32,
    },
}
impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        // Create a channel for mixer commands - std::sync::mpsc doesn't take a capacity
        let (command_sender, command_receiver) = std::sync::mpsc::channel();

        // Create ringbuffer for mixer output
        const MIXER_BUFFER_SIZE: usize = 32768;
        let mixer_rb = HeapRb::<f32>::new(MIXER_BUFFER_SIZE);
        let (mixer_producer, mixer_consumer) = mixer_rb.split();

        // Create PipeWire audio output with consumer
        // Need to add conversion from pipewire::Error to PlaybackError
        info!("spawn pipewire output");
        let audio_output = match PipewireOutput::new(mixer_consumer) {
            Ok(output) => output,
            Err(e) => return Err(PlaybackError::AudioDevice(format!("PipeWire error: {}", e))),
        };

        // Start the mix thread with command receiver
        let mix_task = std::thread::spawn(move || {
            let mut mixer = Mixer::new(mixer_producer);
            let mut consumers = HashMap::<Deck, HeapConsumer<f32>>::new();
            let mut temp_buffer = vec![0.0; 1920 * 2];

            tracing::info!("MIX THREAD: Started, will process audio");

            loop {
                // Process any pending commands
                while let Ok(cmd) = command_receiver.try_recv() {
                    match cmd {
                        MixerCommand::RegisterTrack { deck, consumer } => {
                            tracing::info!("MIX THREAD: Registering track for deck {:?}", deck);
                            consumers.insert(deck, consumer);
                        }
                        MixerCommand::SetVolume { deck, db } => {
                            mixer.set_volume(deck, db);
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
        });

        // Return the engine
        Ok(Self {
            decks: Arc::new(RwLock::new(HashMap::new())),
            _audio_output: audio_output,
            command_sender,
            _mix_task: Some(mix_task),
        })
    }

    pub async fn load_track(&mut self, deck: Deck, path: &Path) -> Result<(), PlaybackError> {
        tracing::info!("Starting track load for deck {:?}", deck);

        // Create ringbuffer for this deck
        const BUFFER_SIZE: usize = 16384;
        let rb = HeapRb::<f32>::new(BUFFER_SIZE);
        let (producer, consumer) = rb.split();

        // Create new track with producer
        let track = Track::new(FlacSource::new(path)?, producer).await?;
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

    pub fn set_volume(&mut self, deck: Deck, db: f32) -> Result<(), PlaybackError> {
        // Validate the volume value first
        if !(-96.0..=0.0).contains(&db) {
            return Err(PlaybackError::InvalidVolume(db));
        }

        // Send the volume command through the channel - use send instead of try_send
        match self
            .command_sender
            .send(MixerCommand::SetVolume { deck, db })
        {
            Ok(_) => {
                tracing::info!("Setting volume for deck {:?} to {}dB", deck, db);
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

    pub async fn seek(&mut self, deck: Deck, position: usize) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            tracing::info!("Seeking deck {:?} to position {}", deck, position);
            // We need to pass the RwLockWriteGuard to the async context, which is tricky
            // We'll need to get a write lock, perform the seek, and release
            let mut track_guard = track.write();
            track_guard.seek(position)
        } else {
            tracing::error!("No track loaded in deck {:?}", deck);
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }
}
