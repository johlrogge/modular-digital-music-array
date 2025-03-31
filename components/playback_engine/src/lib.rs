mod commands;
mod error;
mod mixer;
mod pipewire_output;
mod position;
mod source;
mod track;

use std::{collections::HashMap, path::Path, sync::Arc};

pub use error::PlaybackError;
use mixer::Mixer;
use parking_lot::RwLock;
use pipewire_output::PipewireOutput;
pub use playback_primitives::Deck;
use ringbuf::{HeapConsumer, HeapRb};
pub use source::{FlacSource, Source};
pub use track::Track;

type Decks = Arc<RwLock<HashMap<Deck, Arc<RwLock<Track>>>>>;

pub struct PlaybackEngine {
    decks: Decks,
    audio_output: PipewireOutput,
    mixer_shared: Arc<parking_lot::Mutex<Mixer>>,
    consumers_shared: Arc<parking_lot::Mutex<HashMap<Deck, HeapConsumer<f32>>>>,
    mix_task: Option<std::thread::JoinHandle<()>>,
}
impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        // Create ringbuffer for mixer output
        const MIXER_BUFFER_SIZE: usize = 4096; // Larger than audio callback
        let mixer_rb = HeapRb::<f32>::new(MIXER_BUFFER_SIZE);
        let (mixer_producer, mixer_consumer) = mixer_rb.split();

        // Create mixer with the producer
        let mixer = Mixer::new(mixer_producer);

        // Create PipeWire audio output with consumer
        let audio_output = PipewireOutput::new(mixer_consumer)?;

        // Rest of the method stays the same...
        let decks = Arc::new(RwLock::new(HashMap::new()));

        // Create shared objects
        let mixer_shared = Arc::new(parking_lot::Mutex::new(mixer));
        let consumers_shared = Arc::new(parking_lot::Mutex::new(
            HashMap::<Deck, HeapConsumer<f32>>::new(),
        ));

        // Clone for the mix thread
        let mixer_clone = Arc::clone(&mixer_shared);
        let consumers_clone = Arc::clone(&consumers_shared);

        // Start the mix thread
        let mix_task = std::thread::spawn(move || {
            // Add immediate confirmation that thread started
            tracing::info!("MIX THREAD: Started, will process audio");

            let mut temp_buffer = vec![0.0; 1920 * 2];

            loop {
                // CHANGE: First lock the consumers, then the mixer
                let mut consumers_guard = consumers_clone.lock();

                // Now lock the mixer
                let mut mixer_guard = mixer_clone.lock(); // Calculate buffer length once
                let buffer_len = temp_buffer.len();

                // Mix (handle errors properly)
                if let Err(e) = mixer_guard.mix(&mut temp_buffer, buffer_len, &mut consumers_guard)
                {
                    tracing::error!("MIX THREAD: Error mixing: {}", e);
                }

                // Sleep a bit to avoid spinning
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        // Return the engine with updated field
        Ok(Self {
            decks,
            audio_output,
            mixer_shared,
            consumers_shared,
            mix_task: Some(mix_task),
        })
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

            // Add debugging for mix thread
            {
                let consumers = self.consumers_shared.lock();
                tracing::info!("DEBUG PLAY: Consumer map has {} entries", consumers.len());

                for (d, consumer) in consumers.iter() {
                    tracing::info!(
                        "DEBUG PLAY: Deck {:?} has {} samples available",
                        d,
                        consumer.len()
                    );
                }
            }

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

    pub fn set_volume(&mut self, deck: Deck, db: f32) -> Result<(), PlaybackError> {
        if !(-96.0..=0.0).contains(&db) {
            return Err(PlaybackError::InvalidVolume(db));
        }

        // Get mixer and set volume
        let mut mixer = self.mixer_shared.lock();
        mixer.set_volume(deck, db);
        tracing::info!("Setting volume for deck {:?} to {}dB", deck, db);
        Ok(())
    }

    pub async fn load_track(&mut self, deck: Deck, path: &Path) -> Result<(), PlaybackError> {
        // Create ringbuffer for this deck (unchanged)
        const BUFFER_SIZE: usize = 16384; // 16K samples (~170ms at 48kHz stereo)
        let rb = HeapRb::<f32>::new(BUFFER_SIZE);
        let (producer, consumer) = rb.split();

        // Create new track with producer
        let track = Track::new(FlacSource::new(path)?, producer).await?;

        // Wait for the track to be ready (unchanged)
        tracing::info!("Track is ready for playback");

        // Get position tracker from the track
        let position_tracker = Arc::clone(&track.position_tracker);

        // Store the track (unchanged)
        let mut decks = self.decks.write();
        decks.insert(deck, Arc::new(RwLock::new(track)));

        // Store the consumer in the shared map
        let mut consumers = self.consumers_shared.lock();
        let old_size = consumers.len();
        consumers.insert(deck, consumer);
        tracing::info!(
            "TRACK STATE: Consumer map changed from {} to {} entries",
            old_size,
            consumers.len()
        );

        // Register position tracker with the mixer
        let mut mixer = self.mixer_shared.lock();
        mixer.register_position_tracker(deck, position_tracker);

        tracing::info!("Loaded track from {:?} into deck {:?}", path, deck);
        Ok(())
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

    pub fn get_position(&self, deck: Deck) -> Result<usize, PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            Ok(track.read().position())
        } else {
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }
}
