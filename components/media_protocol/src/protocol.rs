use playback_primitives::{
    AudioOutputConfig, AudioSinkInfo, CanonicalHash, ContentHash, Deck, OutputHealth, PositionMs,
    SourceName, Volume,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[allow(dead_code)] // used by serde via string reference in #[serde(default = "...")]
fn default_audio_source() -> SourceName {
    SourceName::audio()
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
    /// Move the audible track to `position_ms` (kept within the track).
    /// Playing stays playing and paused stays paused; Idle is refused. The
    /// audio source drops its preload, which playback re-sends. Answers
    /// `ResponseData::Transport` with where the track actually is.
    Seek {
        deck: Deck,
        position_ms: PositionMs,
    },
    /// Where the deck is: transport state, audible track, position, duration.
    GetTransport {
        deck: Deck,
    },
    GetLength {
        deck: Deck,
    },
    // Queue management — queue feeds deck A only
    QueueNext {
        hash: ContentHash,
        #[serde(default = "default_audio_source")]
        source: SourceName,
    },
    QueueAppend {
        hash: ContentHash,
        #[serde(default = "default_audio_source")]
        source: SourceName,
    },
    QueueList,
    QueueClear,
    QueueRemove {
        hashes: Vec<ContentHash>,
    },
    /// Atomically replace the entire queue with a new ordered list.
    QueueReplace {
        entries: Vec<(ContentHash, SourceName)>,
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
    /// Ask whether the audio output is pulling samples. Deckless: output
    /// commands are exempt from naming a deck (ADR-017 section 2.5).
    GetOutputStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: Option<ResponseData> },
    Err { message: String },
}

impl Command {
    /// The content hashes this command carries, for the queue commands.
    pub fn hashes_held(&self) -> Vec<&ContentHash> {
        match self {
            Command::QueueAppend { hash, .. } | Command::QueueNext { hash, .. } => vec![hash],
            Command::QueueRemove { hashes } => hashes.iter().collect(),
            Command::QueueReplace { entries } => entries.iter().map(|(h, _)| h).collect(),
            _ => Vec::new(),
        }
    }

    /// Refuse the whole command if any hash it carries is not a full one.
    pub fn check_canonical(&self) -> Result<(), Response> {
        self.hashes_held()
            .into_iter()
            .try_for_each(|hash| require_canonical(hash.clone()).map(drop))
    }
}

/// The one place a hash received from a client becomes a [`CanonicalHash`]
/// (playback holds only full hashes, ADR-026). A prefix is refused with the
/// error response to send back, which tells the client what to do.
pub fn require_canonical(hash: ContentHash) -> Result<CanonicalHash, Response> {
    CanonicalHash::try_from(hash).map_err(|e| Response::Err {
        message: format!("{e}; resolve it through the library"),
    })
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
    /// `None` until the source has reported its output health (see ADR-028).
    OutputStatus(Option<OutputHealth>),
    Transport(TransportStatus),
}

/// Whether a loaded track is playing or paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Playing,
    Paused,
}

/// Where the audible track would return to when a preview ends (ADR-031).
/// Never sent until the preview hold exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewStatus {
    pub back_to: Option<ContentHash>,
    pub back_at_ms: Option<PositionMs>,
}

/// The answer to `GetTransport`, and to a successful `Seek`. An idle deck has
/// no track, so it has no position either: the track and its transport exist
/// together or not at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TransportStatus {
    Idle,
    Loaded {
        transport: Transport,
        /// The audible track.
        now: ContentHash,
        /// `None` when the source cannot say (an old source, or a failed poll).
        position_ms: Option<PositionMs>,
        duration_ms: Option<PositionMs>,
        /// Always `None` until the preview hold (ADR-031 phase S2) exists.
        #[serde(default)]
        preview: Option<PreviewStatus>,
    },
}

impl TransportStatus {
    /// The audible position, when a track is loaded and the source said where.
    pub fn position(&self) -> Option<PositionMs> {
        match self {
            TransportStatus::Loaded { position_ms, .. } => *position_ms,
            TransportStatus::Idle => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playback_primitives::SourceName;
    use pretty_assertions::assert_eq;

    #[test]
    fn require_canonical_refusal_names_the_hash_and_the_remedy() {
        let Err(Response::Err { message }) = require_canonical(ContentHash::new("9fb4105e")) else {
            panic!("expected a refusal");
        };
        assert_eq!(
            message,
            "not a full content hash: 9fb4105e; resolve it through the library"
        );
    }

    #[test]
    fn hashes_held_lists_every_hash_of_queue_commands() {
        let a = ContentHash::new("a");
        let b = ContentHash::new("b");
        let replace = Command::QueueReplace {
            entries: vec![
                (a.clone(), SourceName::audio()),
                (b.clone(), SourceName::audio()),
            ],
        };
        assert_eq!(replace.hashes_held(), vec![&a, &b]);
        let remove = Command::QueueRemove {
            hashes: vec![a.clone()],
        };
        assert_eq!(remove.hashes_held(), vec![&a]);
        assert!(Command::QueueList.hashes_held().is_empty());
        assert!(Command::QueueClear.check_canonical().is_ok());
        assert!(remove.check_canonical().is_err());
        assert!(Command::GetOutputStatus.hashes_held().is_empty());
    }

    #[test]
    fn get_output_status_roundtrips_without_a_deck() {
        let json = serde_json::to_string(&Command::GetOutputStatus).unwrap();
        assert_eq!(json, r#""get_output_status""#);
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, Command::GetOutputStatus));
    }

    #[test]
    fn seek_carries_a_deck_and_milliseconds() {
        let cmd = Command::Seek {
            deck: Deck::A,
            position_ms: PositionMs::new(90_000),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"seek":{"deck":"A","position_ms":90000}}"#);
        let decoded: Command = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            Command::Seek {
                deck: Deck::A,
                position_ms
            } if position_ms == PositionMs::new(90_000)
        ));
    }

    #[test]
    fn the_old_seek_form_no_longer_parses() {
        let old = r#"{"seek":{"deck":"A","position":441000}}"#;
        assert!(serde_json::from_str::<Command>(old).is_err());
    }

    #[test]
    fn get_transport_requires_a_deck() {
        let json = serde_json::to_string(&Command::GetTransport { deck: Deck::B }).unwrap();
        assert_eq!(json, r#"{"get_transport":{"deck":"B"}}"#);
        assert!(serde_json::from_str::<Command>(r#""get_transport""#).is_err());
        assert!(Command::GetTransport { deck: Deck::A }
            .hashes_held()
            .is_empty());
    }

    #[test]
    fn transport_response_roundtrips_and_tolerates_a_missing_preview() {
        let status = TransportStatus::Loaded {
            transport: Transport::Paused,
            now: ContentHash::new("abc"),
            position_ms: Some(PositionMs::new(1_500)),
            duration_ms: Some(PositionMs::new(240_000)),
            preview: None,
        };
        let json = serde_json::to_string(&ResponseData::Transport(status.clone())).unwrap();
        assert_eq!(
            json,
            r#"{"type":"transport","value":{"state":"loaded","transport":"paused","now":"abc","position_ms":1500,"duration_ms":240000,"preview":null}}"#
        );
        let ResponseData::Transport(back) = serde_json::from_str(&json).unwrap() else {
            panic!("expected Transport");
        };
        assert_eq!(back, status);

        let no_preview = r#"{"type":"transport","value":{"state":"loaded","transport":"playing","now":"abc","position_ms":null,"duration_ms":null}}"#;
        let ResponseData::Transport(loaded) = serde_json::from_str(no_preview).unwrap() else {
            panic!("expected Transport");
        };
        assert!(matches!(
            loaded,
            TransportStatus::Loaded {
                transport: Transport::Playing,
                preview: None,
                ..
            }
        ));
    }

    #[test]
    fn an_idle_transport_has_no_track_to_put_a_position_on() {
        let json = serde_json::to_string(&TransportStatus::Idle).unwrap();
        assert_eq!(json, r#"{"state":"idle"}"#);
        assert_eq!(TransportStatus::Idle.position(), None);
    }

    #[test]
    fn output_status_response_roundtrips() {
        for health in [None, Some(OutputHealth::Stalled)] {
            let json = serde_json::to_string(&ResponseData::OutputStatus(health)).unwrap();
            let decoded: ResponseData = serde_json::from_str(&json).unwrap();
            assert!(matches!(decoded, ResponseData::OutputStatus(h) if h == health));
        }
    }

    #[test]
    fn queue_append_without_source_defaults_to_audio() {
        let json = r#"{"queue_append":{"hash":"abc123def456abc123def456abc123def456abc123def456abc123def456abc1"}}"#;
        let decoded: Command = serde_json::from_str(json).unwrap();
        if let Command::QueueAppend { source, .. } = decoded {
            assert_eq!(source, SourceName::audio());
        } else {
            panic!("expected QueueAppend variant");
        }
    }

    #[test]
    fn queue_next_without_source_defaults_to_audio() {
        let json = r#"{"queue_next":{"hash":"abc123def456abc123def456abc123def456abc123def456abc123def456abc1"}}"#;
        let decoded: Command = serde_json::from_str(json).unwrap();
        if let Command::QueueNext { source, .. } = decoded {
            assert_eq!(source, SourceName::audio());
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
