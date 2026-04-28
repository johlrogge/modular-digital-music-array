//! IPC Protocol types for the ACID (Append-Only Content-Indexed Database) service.
//!
//! Pure types with no network dependencies. Shared between:
//! - mdma-acid (server)
//! - acid-ipc-client (used by services that write/read facts)

use serde::{Deserialize, Serialize};

// ============================================================================
// JSONL line helpers
// ============================================================================

/// Return true if the JSONL line belongs to `entity`.
///
/// Facts are stored in array format `[entity, ...]`. Parse just the first element.
pub fn is_entity_match(line: &str, entity: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    v.get(0)
        .and_then(|e| e.as_str())
        .map(|e| e == entity)
        .unwrap_or(false)
}

// ============================================================================
// Cursor helpers
// ============================================================================

/// Encode a line offset as an opaque cursor token.
///
/// Internal format is `"line:<n>"` but callers must treat this as opaque.
pub fn cursor_from_offset(offset: usize) -> String {
    format!("line:{offset}")
}

/// Decode a line offset from an opaque cursor token.
///
/// Returns `None` if the cursor is unrecognised or malformed.
pub fn offset_from_cursor(cursor: &str) -> Option<usize> {
    cursor.strip_prefix("line:")?.parse().ok()
}

// ============================================================================
// Fact Entry
// ============================================================================

/// A single fact entry with pre-serialized JSON values.
///
/// ACID is domain-agnostic — it accepts arbitrary JSON strings.
/// The caller is responsible for serializing `value` and `source` to JSON
/// before placing them in this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEntry {
    pub value_json: String,
    pub source_json: String,
}

// ============================================================================
// Request Types
// ============================================================================

/// Requests that can be sent to the ACID service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcidRequest {
    /// Ping to check if service is alive.
    Ping,

    /// Write a batch of facts for an entity.
    WriteFacts {
        entity: String,
        facts: Vec<FactEntry>,
    },

    /// Read a chunk of the append-only stream starting after the given cursor.
    ///
    /// `cursor: None` means "start from the beginning".
    /// Pass the `cursor` from the previous `StreamChunk` to page forward.
    ReadStream {
        cursor: Option<String>,
        limit: usize,
    },

    /// Retract a batch of facts for an entity.
    ///
    /// Appends retraction records to the log. Semantically the inverse of
    /// `WriteFacts` (Assert); uses `Operation::Retract` in the stored fact.
    RetractFacts {
        entity: String,
        facts: Vec<FactEntry>,
    },

    /// Read all stored facts for a single entity.
    ReadEntity { entity: String },
}

// ============================================================================
// Response Types
// ============================================================================

/// A chunk of stream data returned by `ReadStream`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub lines: Vec<String>,
    /// Opaque cursor pointing past the last returned line.
    /// Pass this back in the next `ReadStream` request to continue paging.
    pub cursor: String,
}

/// All facts for a single entity, returned by `ReadEntity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityFacts {
    pub lines: Vec<String>,
}

/// Responses from the ACID service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcidResponse {
    /// Pong response to Ping.
    Pong,

    /// Confirmation that facts were written successfully.
    WriteOk { facts_written: usize },

    /// Confirmation that facts were retracted successfully.
    RetractOk { facts_retracted: usize },

    /// A chunk of stream lines.
    StreamChunk(StreamChunk),

    /// All facts for a requested entity.
    EntityFacts(EntityFacts),

    /// Error response.
    Error { message: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ---- cursor helpers -----------------------------------------------------

    #[test]
    fn cursor_roundtrip() {
        let cursor = cursor_from_offset(42);
        assert_eq!(cursor, "line:42");
        assert_eq!(offset_from_cursor(&cursor), Some(42));
    }

    #[test]
    fn cursor_from_zero() {
        let cursor = cursor_from_offset(0);
        assert_eq!(offset_from_cursor(&cursor), Some(0));
    }

    #[test]
    fn offset_from_invalid_cursor_returns_none() {
        assert_eq!(offset_from_cursor("timestamp:1234"), None);
        assert_eq!(offset_from_cursor("line:notanumber"), None);
        assert_eq!(offset_from_cursor("garbage"), None);
    }

    // ---- FactEntry ----------------------------------------------------------

    #[test]
    fn fact_entry_roundtrip() {
        let entry = FactEntry {
            value_json: r#"{"bpm":128}"#.to_string(),
            source_json: r#"{"source":"analyser"}"#.to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: FactEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.value_json, entry.value_json);
        assert_eq!(parsed.source_json, entry.source_json);
    }

    // ---- AcidRequest --------------------------------------------------------

    #[test]
    fn acid_request_ping_roundtrip() {
        let req = AcidRequest::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"Ping\""), "json was: {json}");
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, AcidRequest::Ping));
    }

    #[test]
    fn acid_request_write_facts_roundtrip() {
        let req = AcidRequest::WriteFacts {
            entity: "track:sha256:abc123".to_string(),
            facts: vec![FactEntry {
                value_json: r#""techno""#.to_string(),
                source_json: r#"{"source":"tagger"}"#.to_string(),
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"WriteFacts\""), "json was: {json}");
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidRequest::WriteFacts { entity, facts } => {
                assert_eq!(entity, "track:sha256:abc123");
                assert_eq!(facts.len(), 1);
                assert_eq!(facts[0].value_json, r#""techno""#);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_request_read_stream_with_none_cursor() {
        let req = AcidRequest::ReadStream {
            cursor: None,
            limit: 100,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"ReadStream\""), "json was: {json}");
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidRequest::ReadStream { cursor, limit } => {
                assert_eq!(cursor, None);
                assert_eq!(limit, 100);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_request_read_stream_with_cursor() {
        let req = AcidRequest::ReadStream {
            cursor: Some("line:42".to_string()),
            limit: 10,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidRequest::ReadStream { cursor, limit } => {
                assert_eq!(cursor.as_deref(), Some("line:42"));
                assert_eq!(limit, 10);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ---- AcidResponse -------------------------------------------------------

    #[test]
    fn acid_response_pong_roundtrip() {
        let resp = AcidResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"Pong\""), "json was: {json}");
        let parsed: AcidResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, AcidResponse::Pong));
    }

    #[test]
    fn acid_response_write_ok_roundtrip() {
        let resp = AcidResponse::WriteOk { facts_written: 7 };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"WriteOk\""), "json was: {json}");
        let parsed: AcidResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidResponse::WriteOk { facts_written } => assert_eq!(facts_written, 7),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_response_stream_chunk_roundtrip() {
        let chunk = StreamChunk {
            lines: vec!["line1".to_string(), "line2".to_string()],
            cursor: "line:2".to_string(),
        };
        let resp = AcidResponse::StreamChunk(chunk);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains("\"type\":\"StreamChunk\""),
            "json was: {json}"
        );
        let parsed: AcidResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidResponse::StreamChunk(c) => {
                assert_eq!(c.lines, vec!["line1", "line2"]);
                assert_eq!(c.cursor, "line:2");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_request_retract_facts_roundtrip() {
        let req = AcidRequest::RetractFacts {
            entity: "track:sha256:abc123".to_string(),
            facts: vec![FactEntry {
                value_json: r#""techno""#.to_string(),
                source_json: r#"{"source":"tagger"}"#.to_string(),
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"type\":\"RetractFacts\""),
            "json was: {json}"
        );
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidRequest::RetractFacts { entity, facts } => {
                assert_eq!(entity, "track:sha256:abc123");
                assert_eq!(facts.len(), 1);
                assert_eq!(facts[0].value_json, r#""techno""#);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_request_read_entity_roundtrip() {
        let req = AcidRequest::ReadEntity {
            entity: "track:sha256:abc123".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"ReadEntity\""), "json was: {json}");
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidRequest::ReadEntity { entity } => {
                assert_eq!(entity, "track:sha256:abc123");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_response_retract_ok_roundtrip() {
        let resp = AcidResponse::RetractOk { facts_retracted: 3 };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"RetractOk\""), "json was: {json}");
        let parsed: AcidResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidResponse::RetractOk { facts_retracted } => assert_eq!(facts_retracted, 3),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_response_entity_facts_roundtrip() {
        let resp = AcidResponse::EntityFacts(EntityFacts {
            lines: vec!["line1".to_string(), "line2".to_string()],
        });
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains("\"type\":\"EntityFacts\""),
            "json was: {json}"
        );
        let parsed: AcidResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidResponse::EntityFacts(ef) => {
                assert_eq!(ef.lines, vec!["line1", "line2"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn acid_response_error_roundtrip() {
        let resp = AcidResponse::Error {
            message: "something went wrong".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"Error\""), "json was: {json}");
        let parsed: AcidResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidResponse::Error { message } => {
                assert_eq!(message, "something went wrong");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
