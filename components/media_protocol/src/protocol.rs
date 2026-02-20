use playback_primitives::{ContentHash, Deck};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    LoadTrack {
        path: PathBuf,
        deck: Deck,
    },
    Play {
        deck: Deck,
    },
    Stop {
        deck: Deck,
    },
    SetVolume {
        deck: Deck,
        db: f32,
    },
    Unload {
        deck: Deck,
    },
    Seek {
        deck: Deck,
        position: usize,
    },
    GetLength {
        deck: Deck,
    },
    // Queue management — queue feeds deck A only
    QueueNext {
        hash: ContentHash,
        path: PathBuf,
    },
    QueueAppend {
        hash: ContentHash,
        path: PathBuf,
    },
    QueueList,
    QueueClear,
    QueueRemove {
        hashes: Vec<ContentHash>,
    },
    /// Pop from queue head, load on deck A, and start playing.
    PlayQueue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub success: bool,
    pub error_message: String,
    pub data: Option<ResponseData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum ResponseData {
    Position(usize),
    Length(usize),
    Queue(Vec<ContentHash>),
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
