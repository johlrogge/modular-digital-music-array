use playback_primitives::{Channel, Volume};
use serde::{Deserialize, Serialize};
use time_primitives::Ticks;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef(String);

impl FileRef {
    pub fn new(path: impl AsRef<str>) -> Self {
        Self(path.as_ref().to_owned())
    }
}

/// Protocol Messages
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Load a track into memory but don't start playback
    LoadTrack { file: FileRef, channel: Channel },

    /// Begin playback of a loaded track
    StartTrack {
        channel: Channel,
        start_position: Ticks,
        initial_volume: Volume,
    },

    /// Stop playback on a channel
    StopChannel(Channel),

    /// Set volume for a channel
    SetVolume {
        channel: Channel,
        tick: Ticks,
        volume: Volume,
    },

    /// Set mute state for a channel
    SetMute {
        channel: Channel,
        tick: Ticks,
        muted: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mute_sequence() {
        let sequence = vec![
            Message::SetMute {
                channel: Channel::CHANNEL_A,
                tick: Ticks::new(0),
                muted: true,
            },
            Message::SetVolume {
                channel: Channel::CHANNEL_A,
                tick: Ticks::new(0),
                volume: Volume::new(-6.0).unwrap(),
            },
            Message::SetMute {
                channel: Channel::CHANNEL_A,
                tick: Ticks::new(960),
                muted: false,
            },
        ];

        let json = serde_json::to_string(&sequence).unwrap();
        let decoded: Vec<Message> = serde_json::from_str(&json).unwrap();
        assert_eq!(sequence, decoded);
    }
}
