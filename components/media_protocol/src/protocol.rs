use playback_primitives::{ContentHash, Deck, Volume};
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
    /// Pause playback without clearing the current track.
    Pause {
        deck: Deck,
    },
    /// Resume playback after a pause.
    Resume {
        deck: Deck,
    },
    SetVolume {
        deck: Deck,
        volume: Volume,
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
    /// Atomically replace the entire queue with a new ordered list.
    QueueReplace {
        entries: Vec<(ContentHash, PathBuf)>,
    },
    /// Pop from queue head, load on deck A, and start playing.
    PlayQueue,
    /// Return the hash of the track currently loaded on deck A (None if nothing playing).
    NowPlaying,
    /// Stop the current track and advance to the next track in the queue atomically.
    Skip,
    /// Return the current session ID (None if no session is active).
    GetSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: Option<ResponseData> },
    Err { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum ResponseData {
    Position(usize),
    Length(usize),
    Queue(Vec<ContentHash>),
    NowPlaying(Option<ContentHash>),
    Count(usize),
    Session(Option<String>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn response_ok_serialization() {
        let resp = Response::Ok { data: None };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Response::Ok { data: None }));
    }

    #[test]
    fn response_err_serialization() {
        let resp = Response::Err {
            message: "something went wrong".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Response::Err { .. }));
        if let Response::Err { message } = decoded {
            assert_eq!(message, "something went wrong");
        }
    }

    #[test]
    fn response_ok_with_data_serialization() {
        let resp = Response::Ok {
            data: Some(ResponseData::Position(42)),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            Response::Ok {
                data: Some(ResponseData::Position(42))
            }
        ));
    }

    #[test]
    fn set_volume_command_serialization() {
        use playback_primitives::Volume;
        let vol = Volume::new(-6.0).unwrap();
        let cmd = Command::SetVolume {
            deck: Deck::A,
            volume: vol,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            Command::SetVolume {
                deck: Deck::A,
                volume: _
            }
        ));
    }

    #[test]
    fn play_command_serialization() {
        let cmd = Command::Play { deck: Deck::A };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();

        assert!(matches!(decoded, Command::Play { deck: Deck::A }));
    }

    #[test]
    fn skip_command_serialization() {
        let cmd = Command::Skip;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#""skip""#);
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::Skip));
    }

    #[test]
    fn get_session_command_serialization() {
        let cmd = Command::GetSession;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#""get_session""#);
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::GetSession));
    }

    #[test]
    fn pause_command_serialization() {
        let cmd = Command::Pause { deck: Deck::A };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::Pause { deck: Deck::A }));
    }

    #[test]
    fn resume_command_serialization() {
        let cmd = Command::Resume { deck: Deck::A };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::Resume { deck: Deck::A }));
    }

    #[test]
    fn session_response_data_serialization() {
        let data = ResponseData::Session(Some("2026-02-24T12:00:00+00:00".to_string()));
        let json = serde_json::to_string(&data).unwrap();
        let decoded: ResponseData = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ResponseData::Session(Some(_))));

        let none_data = ResponseData::Session(None);
        let json2 = serde_json::to_string(&none_data).unwrap();
        let decoded2: ResponseData = serde_json::from_str(&json2).unwrap();
        assert!(matches!(decoded2, ResponseData::Session(None)));
    }
}
