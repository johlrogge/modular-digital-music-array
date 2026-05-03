use playback_primitives::{ContentHash, SessionId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;

/// Topic prefix for all playback events.
pub const TOPIC_PLAYBACK: &str = "playback/";

/// Topic prefix for all ACID events.
pub const TOPIC_ACID: &str = "acid/";

/// Topic for facts-asserted notifications.
pub const TOPIC_ACID_FACTS_ASSERTED: &str = "acid/facts/asserted";

/// Topic for facts-retracted notifications.
pub const TOPIC_ACID_FACTS_RETRACTED: &str = "acid/facts/retracted";

/// Topic for track started events.
pub const TOPIC_TRACK_STARTED: &str = "playback/track_started";

/// Topic for track ended events.
pub const TOPIC_TRACK_ENDED: &str = "playback/track_ended";

/// Topic for track stopped events.
pub const TOPIC_TRACK_STOPPED: &str = "playback/track_stopped";

/// Topic for track paused events.
pub const TOPIC_TRACK_PAUSED: &str = "playback/track_paused";

/// Topic for track resumed events.
pub const TOPIC_TRACK_RESUMED: &str = "playback/track_resumed";

/// Topic for queue changed events.
pub const TOPIC_QUEUE_CHANGED: &str = "playback/queue_changed";

/// Topic for position update events.
pub const TOPIC_POSITION_UPDATE: &str = "playback/position";

/// Topic for session started events.
pub const TOPIC_SESSION_STARTED: &str = "playback/session_started";

/// Topic for session ended events.
pub const TOPIC_SESSION_ENDED: &str = "playback/session_ended";

/// Events emitted by the playback service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum PlaybackEvent {
    TrackStarted {
        hash: ContentHash,
    },
    TrackEnded {
        hash: ContentHash,
    },
    TrackStopped {
        hash: ContentHash,
    },
    TrackPaused {
        hash: ContentHash,
    },
    TrackResumed {
        hash: ContentHash,
    },
    QueueChanged {
        length: usize,
    },
    PositionUpdate {
        hash: ContentHash,
        position_ms: u64,
        duration_ms: u64,
    },
    SessionStarted {
        id: SessionId,
    },
    SessionEnded {
        id: SessionId,
    },
}

impl PlaybackEvent {
    /// Returns the topic string for this event.
    fn topic(&self) -> &'static str {
        match self {
            PlaybackEvent::TrackStarted { .. } => TOPIC_TRACK_STARTED,
            PlaybackEvent::TrackEnded { .. } => TOPIC_TRACK_ENDED,
            PlaybackEvent::TrackStopped { .. } => TOPIC_TRACK_STOPPED,
            PlaybackEvent::TrackPaused { .. } => TOPIC_TRACK_PAUSED,
            PlaybackEvent::TrackResumed { .. } => TOPIC_TRACK_RESUMED,
            PlaybackEvent::QueueChanged { .. } => TOPIC_QUEUE_CHANGED,
            PlaybackEvent::PositionUpdate { .. } => TOPIC_POSITION_UPDATE,
            PlaybackEvent::SessionStarted { .. } => TOPIC_SESSION_STARTED,
            PlaybackEvent::SessionEnded { .. } => TOPIC_SESSION_ENDED,
        }
    }
}

// ============================================================================
// ACID Events
// ============================================================================

/// Events emitted by the ACID service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum AcidEvent {
    /// New facts have been asserted for an entity (via `WriteFacts`).
    ///
    /// This is a lightweight notification — it does NOT contain the fact data.
    /// Subscribers should issue a `ReadStream` request with the provided cursor
    /// to fetch the new facts.
    FactsAsserted {
        entity: String,
        count: usize,
        cursor: String,
    },
    /// Facts have been retracted for an entity (via `RetractFacts`).
    ///
    /// Same lightweight notification pattern as `FactsAsserted`.
    /// Subscribers should issue a `ReadStream` request with the provided cursor
    /// to fetch the retraction records.
    FactsRetracted {
        entity: String,
        count: usize,
        cursor: String,
    },
}

impl AcidEvent {
    /// Returns the topic string for this event.
    fn topic(&self) -> &'static str {
        match self {
            AcidEvent::FactsAsserted { .. } => TOPIC_ACID_FACTS_ASSERTED,
            AcidEvent::FactsRetracted { .. } => TOPIC_ACID_FACTS_RETRACTED,
        }
    }
}

// ============================================================================
// Generic topic-message encoding/decoding
// ============================================================================

/// Trait for events that carry a topic string used in nng Pub/Sub routing.
pub trait TopicEvent {
    /// Returns the nng topic string for this event variant.
    fn topic(&self) -> &'static str;
}

impl TopicEvent for PlaybackEvent {
    fn topic(&self) -> &'static str {
        PlaybackEvent::topic(self)
    }
}

impl TopicEvent for AcidEvent {
    fn topic(&self) -> &'static str {
        AcidEvent::topic(self)
    }
}

/// Encode any `TopicEvent + Serialize` as a topic-prefixed message for nng Pub/Sub.
///
/// Wire format: `{topic}\0{json}`
///
/// The null byte separates the topic from the JSON body so that nng's
/// topic-based subscription filtering works (subscribers match on prefix).
pub fn encode_topic_message<T: TopicEvent + Serialize>(event: &T) -> Vec<u8> {
    let topic = event.topic();
    let json = serde_json::to_string(event).expect("TopicEvent serialization is infallible");
    let mut buf = Vec::with_capacity(topic.len() + 1 + json.len());
    buf.extend_from_slice(topic.as_bytes());
    buf.push(0);
    buf.extend_from_slice(json.as_bytes());
    buf
}

/// Decode a topic-prefixed message back into any `TopicEvent + DeserializeOwned`.
///
/// Returns `(topic, event)` on success.
pub fn decode_topic_message<T: DeserializeOwned>(bytes: &[u8]) -> Result<(&str, T), ParseError> {
    let sep = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or(ParseError::MissingSeparator)?;
    let topic = std::str::from_utf8(&bytes[..sep]).map_err(|_| ParseError::InvalidTopic)?;
    let event: T = serde_json::from_slice(&bytes[sep + 1..]).map_err(ParseError::Json)?;
    Ok((topic, event))
}

// ============================================================================
// Backward-compatible thin wrappers
// ============================================================================

/// Encode a `PlaybackEvent` as a topic-prefixed message for nng Pub/Sub.
#[inline]
pub fn to_topic_message(event: &PlaybackEvent) -> Vec<u8> {
    encode_topic_message(event)
}

/// Decode a topic-prefixed message back into a `PlaybackEvent`.
#[inline]
pub fn from_topic_message(bytes: &[u8]) -> Result<(&str, PlaybackEvent), ParseError> {
    decode_topic_message(bytes)
}

/// Encode an `AcidEvent` as a topic-prefixed message for nng Pub/Sub.
#[inline]
pub fn acid_event_to_topic_message(event: &AcidEvent) -> Vec<u8> {
    encode_topic_message(event)
}

/// Decode a topic-prefixed message back into an `AcidEvent`.
#[inline]
pub fn acid_event_from_topic_message(bytes: &[u8]) -> Result<(&str, AcidEvent), ParseError> {
    decode_topic_message(bytes)
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("no null separator in topic message")]
    MissingSeparator,
    #[error("topic is not valid UTF-8")]
    InvalidTopic,
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[test]
    fn event_uses_content_hash_type() {
        let hash = playback_primitives::ContentHash::new("sha256:abc123");
        let event = PlaybackEvent::TrackStarted { hash: hash.clone() };
        let bytes = to_topic_message(&event);
        let (_, decoded) = from_topic_message(&bytes).unwrap();
        match decoded {
            PlaybackEvent::TrackStarted { hash: decoded_hash } => assert_eq!(decoded_hash, hash),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_uses_session_id_type() {
        let id = playback_primitives::SessionId::new("2026-02-24T12:00:00+00:00");
        let event = PlaybackEvent::SessionStarted { id: id.clone() };
        let bytes = to_topic_message(&event);
        let (_, decoded) = from_topic_message(&bytes).unwrap();
        match decoded {
            PlaybackEvent::SessionStarted { id: decoded_id } => assert_eq!(decoded_id, id),
            _ => panic!("wrong variant"),
        }
    }

    #[rstest]
    #[case(
        PlaybackEvent::TrackStarted { hash: ContentHash::new("sha256:abc123") },
        TOPIC_TRACK_STARTED
    )]
    #[case(
        PlaybackEvent::TrackEnded { hash: ContentHash::new("sha256:def456") },
        TOPIC_TRACK_ENDED
    )]
    #[case(
        PlaybackEvent::TrackStopped { hash: ContentHash::new("sha256:789ghi") },
        TOPIC_TRACK_STOPPED
    )]
    #[case(
        PlaybackEvent::TrackPaused { hash: ContentHash::new("sha256:abc123") },
        TOPIC_TRACK_PAUSED
    )]
    #[case(
        PlaybackEvent::TrackResumed { hash: ContentHash::new("sha256:abc123") },
        TOPIC_TRACK_RESUMED
    )]
    #[case(
        PlaybackEvent::QueueChanged { length: 42 },
        TOPIC_QUEUE_CHANGED
    )]
    #[case(
        PlaybackEvent::PositionUpdate { hash: ContentHash::new("sha256:abc123"), position_ms: 12_345, duration_ms: 240_000 },
        TOPIC_POSITION_UPDATE
    )]
    #[case(
        PlaybackEvent::SessionStarted { id: SessionId::new("2026-02-24T12:00:00+00:00") },
        TOPIC_SESSION_STARTED
    )]
    #[case(
        PlaybackEvent::SessionEnded { id: SessionId::new("2026-02-24T12:00:00+00:00") },
        TOPIC_SESSION_ENDED
    )]
    fn roundtrip(#[case] event: PlaybackEvent, #[case] expected_topic: &str) {
        let bytes = to_topic_message(&event);
        let (topic, decoded) = from_topic_message(&bytes).unwrap();
        assert_eq!(topic, expected_topic);
        assert_eq!(decoded, event);
    }

    #[test]
    fn wire_format_has_null_separator() {
        let event = PlaybackEvent::TrackStarted {
            hash: ContentHash::new("test"),
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

    // ---- AcidEvent ----------------------------------------------------------

    #[rstest]
    #[case(
        AcidEvent::FactsAsserted {
            entity: "sha256:abc123".to_string(),
            count: 5,
            cursor: "line:42".to_string(),
        },
        TOPIC_ACID_FACTS_ASSERTED
    )]
    #[case(
        AcidEvent::FactsRetracted {
            entity: "sha256:def456".to_string(),
            count: 2,
            cursor: "line:44".to_string(),
        },
        TOPIC_ACID_FACTS_RETRACTED
    )]
    fn acid_event_roundtrip(#[case] event: AcidEvent, #[case] expected_topic: &str) {
        let bytes = acid_event_to_topic_message(&event);
        let (topic, decoded) = acid_event_from_topic_message(&bytes).unwrap();
        assert_eq!(topic, expected_topic);
        assert_eq!(decoded, event);
    }

    #[rstest]
    #[case(
        AcidEvent::FactsAsserted {
            entity: "sha256:abc".to_string(),
            count: 1,
            cursor: "line:0".to_string(),
        },
        TOPIC_ACID_FACTS_ASSERTED
    )]
    #[case(
        AcidEvent::FactsRetracted {
            entity: "sha256:abc".to_string(),
            count: 1,
            cursor: "line:0".to_string(),
        },
        TOPIC_ACID_FACTS_RETRACTED
    )]
    fn acid_event_wire_format_has_null_separator(#[case] event: AcidEvent, #[case] topic: &str) {
        let bytes = acid_event_to_topic_message(&event);
        assert!(bytes.starts_with(topic.as_bytes()));
        assert_eq!(bytes[topic.len()], 0);
    }

    #[test]
    fn acid_prefix_matches_both_variants() {
        let asserted_topic = TOPIC_ACID_FACTS_ASSERTED;
        let retracted_topic = TOPIC_ACID_FACTS_RETRACTED;
        assert!(
            asserted_topic.starts_with(TOPIC_ACID),
            "FactsAsserted topic must start with TOPIC_ACID prefix"
        );
        assert!(
            retracted_topic.starts_with(TOPIC_ACID),
            "FactsRetracted topic must start with TOPIC_ACID prefix"
        );
    }
}
