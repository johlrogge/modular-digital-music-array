use serde::{Deserialize, Serialize};

/// Topic prefix for all playback events.
pub const TOPIC_PLAYBACK: &str = "playback/";

/// Topic for track started events.
pub const TOPIC_TRACK_STARTED: &str = "playback/track_started";

/// Topic for track ended events.
pub const TOPIC_TRACK_ENDED: &str = "playback/track_ended";

/// Topic for track stopped events.
pub const TOPIC_TRACK_STOPPED: &str = "playback/track_stopped";

/// Topic for queue changed events.
pub const TOPIC_QUEUE_CHANGED: &str = "playback/queue_changed";

/// Events emitted by the playback service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum PlaybackEvent {
    TrackStarted { hash: String },
    TrackEnded { hash: String },
    TrackStopped { hash: String },
    QueueChanged { length: usize },
}

impl PlaybackEvent {
    /// Returns the topic string for this event.
    fn topic(&self) -> &'static str {
        match self {
            PlaybackEvent::TrackStarted { .. } => TOPIC_TRACK_STARTED,
            PlaybackEvent::TrackEnded { .. } => TOPIC_TRACK_ENDED,
            PlaybackEvent::TrackStopped { .. } => TOPIC_TRACK_STOPPED,
            PlaybackEvent::QueueChanged { .. } => TOPIC_QUEUE_CHANGED,
        }
    }
}

/// Encode an event as a topic-prefixed message for nng Pub/Sub.
///
/// Wire format: `{topic}\0{json}`
///
/// The null byte separates the topic from the JSON body so that nng's
/// topic-based subscription filtering works (subscribers match on prefix).
pub fn to_topic_message(event: &PlaybackEvent) -> Vec<u8> {
    let topic = event.topic();
    let json = serde_json::to_string(event).expect("PlaybackEvent serialization is infallible");
    let mut buf = Vec::with_capacity(topic.len() + 1 + json.len());
    buf.extend_from_slice(topic.as_bytes());
    buf.push(0);
    buf.extend_from_slice(json.as_bytes());
    buf
}

/// Decode a topic-prefixed message back into an event.
///
/// Returns `(topic, event)` on success.
pub fn from_topic_message(bytes: &[u8]) -> Result<(&str, PlaybackEvent), ParseError> {
    let sep = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or(ParseError::MissingSeparator)?;

    let topic = std::str::from_utf8(&bytes[..sep]).map_err(|_| ParseError::InvalidTopic)?;
    let event: PlaybackEvent =
        serde_json::from_slice(&bytes[sep + 1..]).map_err(ParseError::Json)?;
    Ok((topic, event))
}

#[derive(Debug)]
pub enum ParseError {
    MissingSeparator,
    InvalidTopic,
    Json(serde_json::Error),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingSeparator => write!(f, "no null separator in topic message"),
            ParseError::InvalidTopic => write!(f, "topic is not valid UTF-8"),
            ParseError::Json(e) => write!(f, "JSON parse error: {}", e),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_track_started() {
        let event = PlaybackEvent::TrackStarted {
            hash: "sha256:abc123".into(),
        };
        let bytes = to_topic_message(&event);
        let (topic, decoded) = from_topic_message(&bytes).unwrap();
        assert_eq!(topic, TOPIC_TRACK_STARTED);
        assert_eq!(decoded, event);
    }

    #[test]
    fn roundtrip_track_ended() {
        let event = PlaybackEvent::TrackEnded {
            hash: "sha256:def456".into(),
        };
        let bytes = to_topic_message(&event);
        let (topic, decoded) = from_topic_message(&bytes).unwrap();
        assert_eq!(topic, TOPIC_TRACK_ENDED);
        assert_eq!(decoded, event);
    }

    #[test]
    fn roundtrip_track_stopped() {
        let event = PlaybackEvent::TrackStopped {
            hash: "sha256:789ghi".into(),
        };
        let bytes = to_topic_message(&event);
        let (topic, decoded) = from_topic_message(&bytes).unwrap();
        assert_eq!(topic, TOPIC_TRACK_STOPPED);
        assert_eq!(decoded, event);
    }

    #[test]
    fn roundtrip_queue_changed() {
        let event = PlaybackEvent::QueueChanged { length: 42 };
        let bytes = to_topic_message(&event);
        let (topic, decoded) = from_topic_message(&bytes).unwrap();
        assert_eq!(topic, TOPIC_QUEUE_CHANGED);
        assert_eq!(decoded, event);
    }

    #[test]
    fn wire_format_has_null_separator() {
        let event = PlaybackEvent::TrackStarted {
            hash: "test".into(),
        };
        let bytes = to_topic_message(&event);
        // Should start with topic, then null, then JSON
        assert!(bytes.starts_with(TOPIC_TRACK_STARTED.as_bytes()));
        assert_eq!(bytes[TOPIC_TRACK_STARTED.len()], 0);
    }

    #[test]
    fn parse_error_on_missing_separator() {
        let bytes = b"no separator here";
        assert!(from_topic_message(bytes).is_err());
    }

    #[test]
    fn parse_error_on_invalid_json() {
        let mut bytes = Vec::from(TOPIC_TRACK_STARTED.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(b"not json");
        assert!(from_topic_message(&bytes).is_err());
    }
}
