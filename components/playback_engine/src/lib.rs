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
use parking_lot::{Mutex, RwLock};
pub use playback_primitives::Deck;
pub use source::{FlacSource, Source};
pub use track::Track;

pub struct PlaybackEngine {
    decks: Arc<RwLock<HashMap<Deck, Arc<RwLock<Track<FlacSource>>>>>>,
    _audio_stream: Stream,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        let decks = Arc::new(RwLock::new(HashMap::new()));
        let audio_stream = Self::create_audio_stream(decks.clone())?;

        Ok(Self {
            decks,
            _audio_stream: audio_stream,
        })
    }

    fn find_track(&self, deck: Deck) -> Option<Arc<RwLock<Track<FlacSource>>>> {
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

        if let Some(track) = self.find_track(deck) {
            tracing::info!("Setting volume for deck {:?} to {}dB", deck, db);
            track.write().set_volume(db);
            Ok(())
        } else {
            tracing::error!("No track loaded in deck {:?}", deck);
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }

    pub async fn load_track(&mut self, deck: Deck, path: &Path) -> Result<(), PlaybackError> {
        // Create new track
        let track = Track::<FlacSource>::new(FlacSource::new(path)?).await?;

        // Acquire lock and insert track
        let mut decks = self.decks.write();
        decks.insert(deck, Arc::new(RwLock::new(track)));

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

    fn create_audio_stream(
        decks: Arc<RwLock<HashMap<Deck, Arc<RwLock<Track<FlacSource>>>>>>,
    ) -> Result<Stream, PlaybackError> {
        const SAMPLE_RATE: u32 = 48000;
        const CHANNELS: u16 = 2;
        const BUFFER_SIZE: u32 = 480; // 10ms at 48kHz

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::AudioDevice("No output device found".into()))?;

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(BUFFER_SIZE),
        };

        // Create a mixer that will be shared across audio callbacks
        let mixer = Arc::new(Mutex::new(Mixer::new(
            BUFFER_SIZE as usize * CHANNELS as usize,
        )));

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut mixer = mixer.lock();

                // Use the mixer to mix all active tracks
                if let Err(e) = mixer.mix(&decks, data, data.len()) {
                    tracing::error!("Error during mixing: {}", e);
                    // If mixing fails, output silence
                    data.fill(0.0);
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

    pub fn get_length(&self, deck: Deck) -> Result<usize, PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            Ok(track.read().length())
        } else {
            Err(PlaybackError::NoTrackLoaded(deck))
        }
    }
}
