mod error;
mod track;
mod audio;

use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::RwLock;
pub use error::PlaybackError;
pub use track::{Track, Channel};
use audio::AudioOutput;

pub struct PlaybackEngine {
    tracks: Arc<RwLock<Vec<(Channel, Track)>>>,
    audio: AudioOutput,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, PlaybackError> {
        let audio = AudioOutput::new()?;
        
        Ok(Self {
            tracks: Arc::new(RwLock::new(Vec::new())),
            audio,
        })
    }

    pub fn load_track(&mut self, path: PathBuf, channel: Channel) -> Result<(), PlaybackError> {
        // Check if channel is already in use
        let tracks = self.tracks.read();
        if tracks.iter().any(|(c, _)| *c == channel) {
            return Err(PlaybackError::ChannelInUse(channel));
        }
        drop(tracks);
        
        // Create new track
        let track = Track::new(&path)?;
        
        // Store track
        self.tracks.write().push((channel, track));
        Ok(())
    }

    pub fn play(&mut self, channel: Channel) -> Result<(), PlaybackError> {
        let mut tracks = self.tracks.write();
        let track = tracks
            .iter_mut()
            .find(|(c, _)| *c == channel)
            .ok_or(PlaybackError::NoTrackLoaded(channel))?;
            
        track.1.play();
        Ok(())
    }

    pub fn stop(&mut self, channel: Channel) -> Result<(), PlaybackError> {
        let mut tracks = self.tracks.write();
        let track = tracks
            .iter_mut()
            .find(|(c, _)| *c == channel)
            .ok_or(PlaybackError::NoTrackLoaded(channel))?;
            
        track.1.stop();
        Ok(())
    }

    pub fn set_volume(&mut self, channel: Channel, db: f32) -> Result<(), PlaybackError> {
        if db > 0.0 || db < -96.0 {
            return Err(PlaybackError::InvalidVolume(db));
        }
        
        let mut tracks = self.tracks.write();
        let track = tracks
            .iter_mut()
            .find(|(c, _)| *c == channel)
            .ok_or(PlaybackError::NoTrackLoaded(channel))?;
            
        track.1.set_volume(db);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_load_track_duplicate_channel() {
        let mut engine = PlaybackEngine::new().unwrap();
        let path = PathBuf::from("test.flac");

        // First load should fail because file doesn't exist
        assert!(matches!(
            engine.load_track(path.clone(), Channel::A),
            Err(PlaybackError::TrackNotFound(_))
        ));
    }

    #[test]
    fn test_volume_validation() {
        let mut engine = PlaybackEngine::new().unwrap();
        
        // Test invalid volume levels
        assert!(matches!(
            engine.set_volume(Channel::A, 1.0),
            Err(PlaybackError::InvalidVolume(_))
        ));
        
        assert!(matches!(
            engine.set_volume(Channel::A, -100.0),
            Err(PlaybackError::InvalidVolume(_))
        ));
    }
}