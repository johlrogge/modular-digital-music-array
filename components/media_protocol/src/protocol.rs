use playback_primitives::Channel;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    LoadTrack { path: PathBuf, channel: Channel },
    Play { channel: Channel },
    Stop { channel: Channel },
    SetVolume { channel: Channel, db: f32 },
    Unload { channel: Channel },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub success: bool,
    pub error_message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_play_command_serialization() {
        let cmd = Command::Play {
            channel: Channel::ChannelA,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();

        assert!(matches!(
            decoded,
            Command::Play {
                channel: Channel::ChannelA
            }
        ));
    }
}
