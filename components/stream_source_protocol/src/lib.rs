//! Stream source protocol — spoken between the queue manager and all playback sources.
//!
//! **Framing:** All messages are JSON objects. Commands are tagged with `"cmd"` and
//! responses with `"status"`, using internally-tagged serde representation. Every
//! message is self-describing, which works well with NNG's message-based framing.
//!
//! **Polling:** The queue manager polls `StreamCommand::Loaded` every 200ms to detect
//! track completion. Upon receiving `StreamPlaybackState::Finished`, the queue manager
//! advances to the next entry.

use music_primitives::ContentHash;
pub use playback_primitives::{AudioOutputConfig, AudioSinkInfo};

/// Commands sent FROM the queue manager TO a stream source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum StreamCommand {
    /// Load content by content hash. Used by file-backed sources (mdma-audio).
    /// Streaming sources (bluetooth, radio) do not use Load — they play whatever
    /// their source currently has active.
    Load { content_hash: ContentHash },
    /// Start or resume playback.
    Play,
    /// Pause playback without unloading.
    Pause,
    /// Stop and unload current content.
    Stop,
    /// Query what is currently loaded and its playback state.
    Loaded,
    /// List available audio output devices. File playback sources only.
    ListOutputs,
    /// Set the active audio output device. File playback sources only.
    SetOutput { device_name: String },
    /// Get the currently active audio output configuration. File playback sources only.
    GetOutput,
    /// Connectivity check.
    Ping,
}

/// Responses from a stream source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StreamResponse {
    /// Command accepted.
    Ok,
    /// Response to Loaded query.
    Loaded { info: Option<StreamTrackInfo> },
    /// Response to ListOutputs.
    Outputs { sinks: Vec<AudioSinkInfo> },
    /// Response to GetOutput / SetOutput.
    Output { config: AudioOutputConfig },
    /// Pong.
    Pong,
    /// Command failed.
    Error { message: String },
}

/// What a stream source currently has loaded.
/// None = source is idle/stopped. Some = source has content active.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StreamTrackInfo {
    /// Playback state of the loaded content.
    pub state: StreamPlaybackState,
    /// Content hash (for file-backed sources; None for streaming sources).
    pub content_hash: Option<ContentHash>,
    /// Human-readable track title.
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Playback position in milliseconds.
    pub position_ms: Option<u64>,
    /// Total duration in milliseconds.
    pub duration_ms: Option<u64>,
}

/// The playback state of what is currently loaded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPlaybackState {
    Playing,
    Paused,
    /// Track or content has naturally ended.
    /// The queue manager polls `Loaded` and advances when it sees this state.
    Finished,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // StreamCommand serialization tests

    #[test]
    fn stream_command_play_serialize() {
        let cmd = StreamCommand::Play;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"cmd":"play"}"#);
    }

    #[test]
    fn stream_command_play_deserialize() {
        let parsed: StreamCommand = serde_json::from_str(r#"{"cmd":"play"}"#).unwrap();
        assert!(matches!(parsed, StreamCommand::Play));
    }

    #[test]
    fn stream_command_pause_serialize() {
        let cmd = StreamCommand::Pause;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"cmd":"pause"}"#);
    }

    #[test]
    fn stream_command_pause_deserialize() {
        let parsed: StreamCommand = serde_json::from_str(r#"{"cmd":"pause"}"#).unwrap();
        assert!(matches!(parsed, StreamCommand::Pause));
    }

    #[test]
    fn stream_command_stop_serialize() {
        let cmd = StreamCommand::Stop;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"cmd":"stop"}"#);
    }

    #[test]
    fn stream_command_stop_deserialize() {
        let parsed: StreamCommand = serde_json::from_str(r#"{"cmd":"stop"}"#).unwrap();
        assert!(matches!(parsed, StreamCommand::Stop));
    }

    #[test]
    fn stream_command_list_outputs_serialize() {
        let cmd = StreamCommand::ListOutputs;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"cmd":"list_outputs"}"#);
    }

    #[test]
    fn stream_command_list_outputs_deserialize() {
        let parsed: StreamCommand = serde_json::from_str(r#"{"cmd":"list_outputs"}"#).unwrap();
        assert!(matches!(parsed, StreamCommand::ListOutputs));
    }

    #[test]
    fn stream_command_set_output_serialize() {
        let cmd = StreamCommand::SetOutput {
            device_name: "alsa_output.usb-1".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"set_output","device_name":"alsa_output.usb-1"}"#
        );
    }

    #[test]
    fn stream_command_set_output_deserialize() {
        let parsed: StreamCommand =
            serde_json::from_str(r#"{"cmd":"set_output","device_name":"alsa_output.usb-1"}"#)
                .unwrap();
        assert!(matches!(parsed, StreamCommand::SetOutput { .. }));
        if let StreamCommand::SetOutput { device_name } = parsed {
            assert_eq!(device_name, "alsa_output.usb-1");
        }
    }

    #[test]
    fn stream_command_get_output_serialize() {
        let cmd = StreamCommand::GetOutput;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"cmd":"get_output"}"#);
    }

    #[test]
    fn stream_command_get_output_deserialize() {
        let parsed: StreamCommand = serde_json::from_str(r#"{"cmd":"get_output"}"#).unwrap();
        assert!(matches!(parsed, StreamCommand::GetOutput));
    }

    #[test]
    fn stream_command_load_serialize() {
        let cmd = StreamCommand::Load {
            content_hash: ContentHash::new("abc123"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"cmd":"load","content_hash":"abc123"}"#);
    }

    #[test]
    fn stream_command_load_deserialize() {
        let parsed: StreamCommand =
            serde_json::from_str(r#"{"cmd":"load","content_hash":"abc123"}"#).unwrap();
        assert!(matches!(parsed, StreamCommand::Load { .. }));
        if let StreamCommand::Load { content_hash } = parsed {
            assert_eq!(content_hash, ContentHash::new("abc123"));
        }
    }

    // StreamResponse serialization tests

    #[test]
    fn stream_response_loaded_none_serialize() {
        let resp = StreamResponse::Loaded { info: None };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"status":"loaded","info":null}"#);
    }

    #[test]
    fn stream_response_loaded_none_deserialize() {
        let parsed: StreamResponse =
            serde_json::from_str(r#"{"status":"loaded","info":null}"#).unwrap();
        assert!(matches!(parsed, StreamResponse::Loaded { info: None }));
    }

    #[test]
    fn stream_response_pong_serialize() {
        let resp = StreamResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"status":"pong"}"#);
    }

    #[test]
    fn stream_response_pong_deserialize() {
        let parsed: StreamResponse = serde_json::from_str(r#"{"status":"pong"}"#).unwrap();
        assert!(matches!(parsed, StreamResponse::Pong));
    }

    #[test]
    fn stream_response_error_serialize() {
        let resp = StreamResponse::Error {
            message: "something broke".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"status":"error","message":"something broke"}"#);
    }

    #[test]
    fn stream_response_error_deserialize() {
        let parsed: StreamResponse =
            serde_json::from_str(r#"{"status":"error","message":"something broke"}"#).unwrap();
        assert!(matches!(parsed, StreamResponse::Error { .. }));
        if let StreamResponse::Error { message } = parsed {
            assert_eq!(message, "something broke");
        }
    }

    #[test]
    fn stream_response_outputs_serialize() {
        let resp = StreamResponse::Outputs {
            sinks: vec![AudioSinkInfo {
                name: "alsa_output.usb-1".to_string(),
                description: Some("USB Audio".to_string()),
                max_sample_rate: Some(192_000),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"status":"outputs","sinks":[{"name":"alsa_output.usb-1","description":"USB Audio","max_sample_rate":192000}]}"#
        );
    }

    #[test]
    fn stream_response_outputs_deserialize() {
        let parsed: StreamResponse = serde_json::from_str(
            r#"{"status":"outputs","sinks":[{"name":"alsa_output.usb-1","description":"USB Audio","max_sample_rate":192000}]}"#,
        )
        .unwrap();
        assert!(matches!(parsed, StreamResponse::Outputs { .. }));
        if let StreamResponse::Outputs { sinks } = parsed {
            assert_eq!(sinks.len(), 1);
            assert_eq!(sinks[0].name, "alsa_output.usb-1");
            assert_eq!(sinks[0].description.as_deref(), Some("USB Audio"));
            assert_eq!(sinks[0].max_sample_rate, Some(192_000));
        }
    }

    #[test]
    fn stream_response_output_serialize() {
        let resp = StreamResponse::Output {
            config: AudioOutputConfig {
                device_name: Some("alsa_output.usb-1".to_string()),
                sample_rate: Some(44100),
                channels: Some(2),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"status":"output","config":{"device_name":"alsa_output.usb-1","sample_rate":44100,"channels":2}}"#
        );
    }

    #[test]
    fn stream_response_output_deserialize() {
        let parsed: StreamResponse = serde_json::from_str(
            r#"{"status":"output","config":{"device_name":"alsa_output.usb-1","sample_rate":44100,"channels":2}}"#,
        )
        .unwrap();
        assert!(matches!(parsed, StreamResponse::Output { .. }));
        if let StreamResponse::Output { config } = parsed {
            assert_eq!(config.device_name.as_deref(), Some("alsa_output.usb-1"));
            assert_eq!(config.sample_rate, Some(44100));
            assert_eq!(config.channels, Some(2));
        }
    }

    #[test]
    fn stream_response_loaded_some_finished_uses_partial_eq() {
        let track = StreamTrackInfo {
            state: StreamPlaybackState::Finished,
            content_hash: Some(ContentHash::new("abc123")),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            position_ms: Some(30_000),
            duration_ms: Some(180_000),
        };
        let expected = StreamTrackInfo {
            state: StreamPlaybackState::Finished,
            content_hash: Some(ContentHash::new("abc123")),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            position_ms: Some(30_000),
            duration_ms: Some(180_000),
        };
        assert_eq!(track, expected);
    }
}
