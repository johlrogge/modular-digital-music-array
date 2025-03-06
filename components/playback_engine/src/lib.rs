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
use parking_lot::RwLock;
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
        let track = Track::<FlacSource>::new(path).await?;

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

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Clear output buffer
                data.fill(0.0);

                // Mix active tracks
                let decks_guard = decks.read();
                for (_deck, track) in decks_guard.iter() {
                    let mut track = track.write();
                    if track.is_playing() {
                        let mut buffer = vec![0.0f32; data.len()];
                        if let Ok(len) = track.get_next_samples(&mut buffer) {
                            if len > 0 {
                                let volume = track.get_volume();
                                for i in 0..len {
                                    data[i] += buffer[i] * volume;
                                }
                            }
                        }
                    }
                }
            },
            move |err| eprintln!("Audio stream error: {}", err),
            None,
        )?;

        stream.play()?;
        Ok(stream)
    }

    pub fn seek(&mut self, deck: Deck, position: usize) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(deck) {
            tracing::info!("Seeking deck {:?} to position {}", deck, position);
            track.write().seek(position)?;
            Ok(())
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
