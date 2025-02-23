use playback_primitives::Deck;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    LoadTrack { path: PathBuf, deck: Deck },
    Play { deck: Deck },
    Stop { deck: Deck },
    SetVolume { deck: Deck, db: f32 },
    Unload { deck: Deck },
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
        let cmd = Command::Play { deck: Deck::A };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();

        assert!(matches!(decoded, Command::Play { deck: Deck::A }));
    }
}
