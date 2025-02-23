mod audio;
mod channels;
mod commands;
mod error;
mod mixer;
mod source;
mod track;

use std::{path::PathBuf, sync::Arc};

use audio::AudioOutput;
pub use error::PlaybackError;
use parking_lot::RwLock;
pub use playback_primitives::Deck;
pub use track::Track;

pub struct PlaybackEngine {
    audio: AudioOutput,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        let audio = AudioOutput::new()?;
        Ok(Self { audio })
    }

    pub fn play(&mut self, channel: Deck) -> Result<(), PlaybackError> {
        tracing::info!("PlaybackEngine::play called for channel {:?}", channel);
        if let Some(track) = self.find_track(channel) {
            tracing::info!("Found track for channel {:?}, setting to play", channel);
            track.write().play();
            tracing::info!("Track set to play for channel {:?}", channel);
            Ok(())
        } else {
            tracing::error!("No track found for channel {:?}", channel);
            Err(PlaybackError::NoTrackLoaded(channel))
        }
    }

    fn find_track(&self, channel: Deck) -> Option<Arc<RwLock<Track>>> {
        tracing::info!("Finding track for channel {:?}", channel);
        let result = self.audio.channels().get_track(channel);
        tracing::info!(
            "Track lookup result for channel {:?}: found={}",
            channel,
            result.is_some()
        );
        result
    }
    pub fn stop(&mut self, channel: Deck) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(channel) {
            track.write().stop();
        }
        Ok(())
    }

    pub fn set_volume(&mut self, channel: Deck, db: f32) -> Result<(), PlaybackError> {
        if !(-96.0..=0.0).contains(&db) {
            return Err(PlaybackError::InvalidVolume(db));
        }

        if let Some(track) = self.find_track(channel) {
            track.write().set_volume(db);
        }
        Ok(())
    }

    pub fn load_track(&mut self, path: PathBuf, channel: Deck) -> Result<(), PlaybackError> {
        // Create new track
        let track = Track::new(&path)?;
        self.audio.add_track(channel, track)
    }

    pub fn unload_track(&mut self, channel: Deck) -> Result<(), PlaybackError> {
        // Even if no track is loaded, unloading should succeed as a no-op
        match self.find_track(channel) {
            Some(_) => self.audio.remove_track(channel),
            None => Ok(()), // No track to unload is still a success
        }
    }
}
