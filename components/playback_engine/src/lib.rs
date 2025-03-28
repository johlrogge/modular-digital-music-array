mod commands;
mod error;
mod mixer;
mod position;
mod source;
mod track;

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

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
    audio_stream: Stream,
    // Remove direct audio_consumers field
    // Remove direct mixer field
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

        // Create audio stream with consumer
        let audio_stream = Self::create_audio_stream(mixer_consumer)?;

        let decks = Arc::new(RwLock::new(HashMap::new()));

        // Create shared objects
        let mixer_shared = Arc::new(parking_lot::Mutex::new(mixer));
        let consumers_shared = Arc::new(parking_lot::Mutex::new(
            HashMap::<Deck, HeapConsumer<f32>>::new(),
        ));

        // Clone for the mix thread
        let mixer_clone = Arc::clone(&mixer_shared);
        let consumers_clone = Arc::clone(&consumers_shared);

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

        // Return the engine
        Ok(Self {
            decks,
            audio_stream: audio_stream,
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

    fn create_audio_stream(mut mixer_consumer: HeapConsumer<f32>) -> Result<Stream, PlaybackError> {
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
        // let consumer = Arc::new(parking_lot::Mutex::new(mixer_consumer));

        // Add debug counter
        let read_samples = Arc::new(AtomicUsize::new(0));
        let read_samples_clone = read_samples.clone();

        // Create periodic logging task
        std::thread::spawn(move || loop {
            let samples = read_samples_clone.swap(0, Ordering::Relaxed);
            tracing::info!("Audio callback read {} samples in the last second", samples);
            std::thread::sleep(std::time::Duration::from_secs(1));
        });

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let callback_start = std::time::Instant::now();

                // Get consumer lock
                //let mut consumer = consumer.lock();

                // Count available samples
                let available = mixer_consumer.len();
                // Read from consumer to fill output buffer
                let read = mixer_consumer.pop_slice(data);
                if read < data.len() {
                    tracing::warn!(
                        "buffer underrun!!! {read}/{} samples available {available}",
                        data.len()
                    );
                }

                // Update read counter
                read_samples.fetch_add(read, Ordering::Relaxed);

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
