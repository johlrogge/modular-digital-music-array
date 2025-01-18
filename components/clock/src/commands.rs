use playback_types::{Channel, PlaybackError, Volume};
use std::path::PathBuf;
use thiserror::Error;
use time_primitives::Ticks;
use time_primitives::TimeError;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("Invalid timing")]
    Timing(#[from] TimeError),
    #[error("Track not found: {0}")]
    TrackNotFound(String),
    #[error("Channel error: {0}")]
    Channel(#[from] PlaybackError),
}

/// Unique identifier for a loaded track
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(Uuid);

impl TrackId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl ToString for TrackId {
    fn to_string(&self) -> String {
        self.0.to_string()
    }
}

/// A single volume point in time
#[derive(Debug, Clone, Copy)]
pub struct VolumePoint {
    pub tick: Ticks,
    pub volume: Volume,
}

/// Command to control playback across the system
#[derive(Debug, Clone)]
pub enum Command {
    /// Load a track into memory but don't start playback
    LoadTrack {
        track_id: TrackId,
        path: PathBuf,
        channel: Channel,
    },

    /// Begin playback of a loaded track
    StartTrack {
        track_id: TrackId,
        start_position: Ticks,
        initial_volume: Volume,
    },

    /// Stop playback on a channel
    StopChannel(Channel),

    /// Set a volume point for a channel
    SetVolumePoint {
        channel: Channel,
        point: VolumePoint,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_control() -> Result<(), CommandError> {
        // Simulate fader movement sampled at regular intervals
        let fader_movement = vec![
            VolumePoint {
                tick: Ticks::new(0),
                volume: Volume::new(0.0)?, // Unity gain
            },
            VolumePoint {
                tick: Ticks::new(480),      // Half a beat later
                volume: Volume::new(-3.0)?, // -3dB
            },
            VolumePoint {
                tick: Ticks::new(960),      // One beat later
                volume: Volume::new(-6.0)?, // -6dB
            },
            VolumePoint {
                tick: Ticks::new(1920), // Two beats later
                volume: Volume::SILENT, // Silent
            },
        ];

        // In practice, these would be sent as individual commands as the
        // fader is moved, capturing the actual performance
        let commands: Vec<Command> = fader_movement
            .into_iter()
            .map(|point| Command::SetVolumePoint {
                channel: Channel::CHANNEL_A,
                point,
            })
            .collect();

        assert_eq!(commands.len(), 4);
        Ok(())
    }
}
