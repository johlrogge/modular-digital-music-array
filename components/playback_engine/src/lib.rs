mod audio;
mod channels;
mod commands;
mod error;
mod mixer;
mod track;

use std::{path::PathBuf, sync::Arc};

use audio::AudioOutput;
pub use error::PlaybackError;
use parking_lot::RwLock;
pub use playback_primitives::Channel;
pub use track::Track;

pub struct PlaybackEngine {
    audio: AudioOutput,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        let audio = AudioOutput::new()?;
        Ok(Self { audio })
    }

    pub fn load_track(&mut self, path: PathBuf, channel: Channel) -> Result<(), PlaybackError> {
        // Create new track
        let track = Track::new(&path)?;
        self.audio.add_track(channel, track)
    }

    pub fn unload_track(&mut self, channel: Channel) -> Result<(), PlaybackError> {
        // Even if no track is loaded, unloading should succeed as a no-op
        match self.find_track(channel) {
            Some(_) => self.audio.remove_track(channel),
            None => Ok(()), // No track to unload is still a success
        }
    }

    pub fn play(&mut self, channel: Channel) -> Result<(), PlaybackError> {
        // Find track on specified channel and start playback
        if let Some(track) = self.find_track(channel) {
            track.write().play();
            Ok(())
        } else {
            Err(PlaybackError::NoTrackLoaded(channel))
        }
    }

    pub fn stop(&mut self, channel: Channel) -> Result<(), PlaybackError> {
        if let Some(track) = self.find_track(channel) {
            track.write().stop();
        }
        Ok(())
    }

    pub fn set_volume(&mut self, channel: Channel, db: f32) -> Result<(), PlaybackError> {
        if !(-96.0..=0.0).contains(&db) {
            return Err(PlaybackError::InvalidVolume(db));
        }

        if let Some(track) = self.find_track(channel) {
            track.write().set_volume(db);
        }
        Ok(())
    }

    fn find_track(&self, channel: Channel) -> Option<Arc<RwLock<Track>>> {
        self.audio.channels().get_track(channel)
    }
}
