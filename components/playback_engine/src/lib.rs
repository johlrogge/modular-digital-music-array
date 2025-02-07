mod audio;
mod error;
mod track;

use audio::AudioOutput;
pub use error::PlaybackError;
use std::path::PathBuf;
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

    pub fn play(&mut self, _channel: Channel) -> Result<(), PlaybackError> {
        Ok(())
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
