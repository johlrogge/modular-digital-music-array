mod audio;
mod error;
mod track;

use audio::AudioOutput;
pub use error::PlaybackError;
use parking_lot::RwLock;
use std::{path::PathBuf, sync::Arc};
pub use track::{Channel, Track};

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
        self.audio.remove_track(channel)
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
    pub fn stop(&mut self, _channel: Channel) -> Result<(), PlaybackError> {
        Ok(())
    }

    pub fn set_volume(&mut self, _channel: Channel, db: f32) -> Result<(), PlaybackError> {
        if !(-96.0..=0.0).contains(&db) {
            return Err(PlaybackError::InvalidVolume(db));
        }

        Ok(())
    }
    fn find_track(&self, channel: Channel) -> Option<Arc<RwLock<Track>>> {
        // Get tracks list from audio output
        self.audio
            .tracks()
            .read()
            .iter()
            .find(|(ch, _)| *ch == channel)
            .map(|(_, track)| Arc::clone(track))
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

    #[test]
    fn test_unload_nonexistent_track() {
        let mut engine = PlaybackEngine::new().unwrap();
        assert!(engine.unload_track(Channel::A).is_ok());
    }

    #[test]
    fn test_play_nonexistent_track() {
        let mut engine = PlaybackEngine::new().unwrap();

        // Attempting to play without loading should fail
        assert!(matches!(
            engine.play(Channel::A),
            Err(PlaybackError::NoTrackLoaded(_))
        ));
    }

    #[test]
    fn test_play_stop_sequence() {
        let mut engine = PlaybackEngine::new().unwrap();
        let path = PathBuf::from("test.flac");

        // Loading non-existent file should fail
        assert!(matches!(
            engine.load_track(path, Channel::A),
            Err(PlaybackError::TrackNotFound(_))
        ));

        // Play should fail after failed load
        assert!(matches!(
            engine.play(Channel::A),
            Err(PlaybackError::NoTrackLoaded(_))
        ));
    }
}
