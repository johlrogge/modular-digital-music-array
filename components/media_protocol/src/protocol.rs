use playback_primitives::{AudioOutputConfig, AudioSinkInfo, ContentHash, Deck, Volume};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[allow(dead_code)] // used by serde via string reference in #[serde(default = "...")]
fn default_audio_source() -> String {
    "audio".to_string()
}

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
        #[serde(default = "default_audio_source")]
        source: String,
    },
    QueueAppend {
        hash: ContentHash,
        #[serde(default = "default_audio_source")]
        source: String,
    },
    QueueList,
    QueueClear,
    QueueRemove {
        hashes: Vec<ContentHash>,
    },
    /// Atomically replace the entire queue with a new ordered list.
    QueueReplace {
        entries: Vec<(ContentHash, String)>,
    },
    /// Pop from queue head, load on deck A, and start playing.
    PlayQueue,
    /// Return the hash of the track currently loaded on deck A (None if nothing playing).
    NowPlaying,
    /// Stop the current track and advance to the next track in the queue atomically.
    Skip,
    /// Return the current session ID (None if no session is active).
    GetSession,
    /// List available audio output devices.
    ListAudioOutputs,
    /// Select an audio output device by name.
    SetAudioOutput {
        device_name: String,
    },
    /// Get the currently selected audio output.
    GetAudioOutput,
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
    AudioOutputs(Vec<AudioSinkInfo>),
    AudioOutput(AudioOutputConfig),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn queue_append_without_source_defaults_to_audio() {
        let json = r#"{"queue_append":{"hash":"abc123def456abc123def456abc123def456abc123def456abc123def456abc1"}}"#;
        let decoded: Command = serde_json::from_str(json).unwrap();
        if let Command::QueueAppend { source, .. } = decoded {
            assert_eq!(source, "audio");
        } else {
            panic!("expected QueueAppend variant");
        }
    }

    #[test]
    fn queue_next_without_source_defaults_to_audio() {
        let json = r#"{"queue_next":{"hash":"abc123def456abc123def456abc123def456abc123def456abc123def456abc1"}}"#;
        let decoded: Command = serde_json::from_str(json).unwrap();
        if let Command::QueueNext { source, .. } = decoded {
            assert_eq!(source, "audio");
        } else {
            panic!("expected QueueNext variant");
        }
    }

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
    fn list_audio_outputs_command_serialization() {
        let cmd = Command::ListAudioOutputs;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#""list_audio_outputs""#);
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::ListAudioOutputs));
    }

    #[test]
    fn set_audio_output_command_serialization() {
        let cmd = Command::SetAudioOutput {
            device_name: "alsa_output.pci-0000_00_1f.3.analog-stereo".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::SetAudioOutput { .. }));
        if let Command::SetAudioOutput { device_name } = decoded {
            assert_eq!(device_name, "alsa_output.pci-0000_00_1f.3.analog-stereo");
        }
    }

    #[test]
    fn get_audio_output_command_serialization() {
        let cmd = Command::GetAudioOutput;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#""get_audio_output""#);
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::GetAudioOutput));
    }

    #[test]
    fn audio_outputs_response_data_serialization() {
        let sinks = vec![AudioSinkInfo {
            name: "alsa_output.pci-0000_00_1f.3.analog-stereo".to_string(),
            description: Some("Built-in Audio Analog Stereo".to_string()),
            max_sample_rate: Some(48000),
        }];
        let data = ResponseData::AudioOutputs(sinks);
        let json = serde_json::to_string(&data).unwrap();
        let decoded: ResponseData = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ResponseData::AudioOutputs(_)));
        if let ResponseData::AudioOutputs(sinks) = decoded {
            assert_eq!(sinks.len(), 1);
            assert_eq!(sinks[0].name, "alsa_output.pci-0000_00_1f.3.analog-stereo");
            assert_eq!(
                sinks[0].description.as_deref(),
                Some("Built-in Audio Analog Stereo")
            );
            assert_eq!(sinks[0].max_sample_rate, Some(48000));
        }
    }

    #[test]
    fn audio_output_response_data_serialization() {
        let config = AudioOutputConfig {
            device_name: Some("alsa_output.pci-0000_00_1f.3.analog-stereo".to_string()),
            sample_rate: Some(44100),
            channels: None,
        };
        let data = ResponseData::AudioOutput(config);
        let json = serde_json::to_string(&data).unwrap();
        let decoded: ResponseData = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ResponseData::AudioOutput(_)));
        if let ResponseData::AudioOutput(cfg) = decoded {
            assert_eq!(
                cfg.device_name.as_deref(),
                Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
            );
            assert_eq!(cfg.sample_rate, Some(44100));
        }

        let none_config = AudioOutputConfig {
            device_name: None,
            sample_rate: Some(48000),
            channels: None,
        };
        let data2 = ResponseData::AudioOutput(none_config);
        let json2 = serde_json::to_string(&data2).unwrap();
        let decoded2: ResponseData = serde_json::from_str(&json2).unwrap();
        assert!(matches!(decoded2, ResponseData::AudioOutput(_)));
        if let ResponseData::AudioOutput(cfg) = decoded2 {
            assert!(cfg.device_name.is_none());
            assert_eq!(cfg.sample_rate, Some(48000));
        }
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
