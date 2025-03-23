mod buffer;
mod commands;
mod error;
mod mixer;
mod source;
mod track;

use std::{collections::HashMap, path::Path, sync::Arc};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Stream,
};
pub use error::PlaybackError;
use mixer::Mixer;
use parking_lot::RwLock;
pub use playback_primitives::Deck;
use ringbuf::{HeapConsumer, HeapRb};
pub use source::{FlacSource, Source};
pub use track::Track;

type Decks = Arc<RwLock<HashMap<Deck, Arc<RwLock<Track>>>>>;

pub struct PlaybackEngine {
    decks: Decks,
    _audio_stream: Stream,
    audio_consumers: HashMap<Deck, HeapConsumer<f32>>,
    mixer: Option<Mixer>,                           // Add mixer instance
    _mix_task: Option<std::thread::JoinHandle<()>>, // Add background mix task
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        // Create ringbuffer for mixer output
        const MIXER_BUFFER_SIZE: usize = 4096; // Larger than audio callback
        let mixer_rb = HeapRb::<f32>::new(MIXER_BUFFER_SIZE);
        let (mixer_producer, mixer_consumer) = mixer_rb.split();

        // Create mixer with the producer
        let mixer = Mixer::new(mixer_producer);

        // Create audio stream with consumer
        let audio_stream = Self::create_audio_stream(mixer_consumer)?;

        let decks = Arc::new(RwLock::new(HashMap::new()));
        let audio_consumers = HashMap::new();

        // Create an Arc<Mutex<Option<Mixer>>> for sharing with the mix thread
        let mixer_shared = Arc::new(parking_lot::Mutex::new(Some(mixer)));
        let mixer_clone = mixer_shared.clone();

        // Create a shared map for the audio consumers
        let consumers_shared = Arc::new(parking_lot::Mutex::new(audio_consumers));
        let consumers_clone = consumers_shared.clone();

        // Start the mix thread (using std::thread for real-time reliability)
        let mix_task = std::thread::spawn(move || {
            let mut temp_buffer = vec![0.0; 1920 * 2];

            loop {
                // Try to lock the mixer
                if let Some(mixer) = &mut *mixer_clone.lock() {
                    let buffer_len = temp_buffer.len();
                    let _ = mixer.mix(&mut temp_buffer, buffer_len, &mut *consumers_clone.lock());
                } else {
                    // Mixer has been taken, exit the thread
                    break;
                }

                // Sleep for a short time
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        // Extract the mixer from the shared container
        let mixer = mixer_shared.lock().take();

        // Extract the consumers from the shared container
        let audio_consumers = std::mem::take(&mut *consumers_shared.lock());

        Ok(Self {
            decks,
            _audio_stream: audio_stream,
            audio_consumers,
            mixer,
            _mix_task: Some(mix_task),
        })
    }

    pub fn mix(&mut self) -> Result<(), PlaybackError> {
        if let Some(mixer) = &mut self.mixer {
            // Create a temporary buffer for mixing
            let mut temp_buffer = vec![0.0; 1920 * 2];
            let buffer_len = temp_buffer.len(); // Calculate length before borrowing

            // Mix all active tracks
            mixer.mix(&mut temp_buffer, buffer_len, &mut self.audio_consumers)?;
        }
        Ok(())
    }

    fn find_track(&self, deck: Deck) -> Option<Arc<RwLock<Track>>> {
        let decks = self.decks.read();
        decks.get(&deck).cloned()
    }

    pub fn play(&mut self, deck: Deck) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            tracing::info!("Playing deck {:?}", deck);
            track.write().play();
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
        if let Some(mixer) = &mut self.mixer {
            mixer.set_volume(deck, db);
            tracing::info!("Setting volume for deck {:?} to {}dB", deck, db);
            Ok(())
        } else {
            Err(PlaybackError::AudioDevice("Mixer not initialized".into()))
        }
    }
    pub async fn load_track(&mut self, deck: Deck, path: &Path) -> Result<(), PlaybackError> {
        // Create ringbuffer for this deck
        const BUFFER_SIZE: usize = 16384; // 16K samples (~170ms at 48kHz stereo)
        let rb = HeapRb::<f32>::new(BUFFER_SIZE);
        let (producer, consumer) = rb.split();

        // Create new track with producer - pass both arguments
        let track = Track::new(FlacSource::new(path)?, producer).await?;

        // Store the track and consumer
        let mut decks = self.decks.write();
        decks.insert(deck, Arc::new(RwLock::new(track)));
        self.audio_consumers.insert(deck, consumer);

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

    fn create_audio_stream(mixer_consumer: HeapConsumer<f32>) -> Result<Stream, PlaybackError> {
        const SAMPLE_RATE: u32 = 48000;
        const CHANNELS: u16 = 2;
        const BUFFER_SIZE: u32 = 1920;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::AudioDevice("No output device found".into()))?;

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(BUFFER_SIZE),
        };

        tracing::info!("Creating audio stream with buffer size: {}", BUFFER_SIZE);

        // Create a thread-safe wrapper for the consumer
        let consumer = Arc::new(parking_lot::Mutex::new(mixer_consumer));

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let callback_start = std::time::Instant::now();

                // Get consumer lock
                let mut consumer = consumer.lock();

                // Read from consumer to fill output buffer
                for sample_idx in 0..data.len() {
                    data[sample_idx] = consumer.pop().unwrap_or(0.0);
                }

                let total_time = callback_start.elapsed();
                if total_time > std::time::Duration::from_millis(1) {
                    tracing::warn!("Audio callback total time: {:?}", total_time);
                }
            },
            move |err| eprintln!("Audio stream error: {}", err),
            None,
        )?;

        stream.play()?;
        Ok(stream)
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
