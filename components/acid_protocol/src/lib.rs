//! IPC Protocol types for the ACID (Append-Only Content-Indexed Database) service.
//!
//! Pure types with no network dependencies. Shared between:
//! - mdma-acid (server)
//! - acid-ipc-client (used by services that write/read facts)

use serde::{Deserialize, Serialize};

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

    /// Read a chunk of the append-only stream starting after a given line offset.
    ReadStream { after_line: usize, limit: usize },
}

// ============================================================================
// Response Types
// ============================================================================

/// A chunk of stream data returned by `ReadStream`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub lines: Vec<String>,
    pub next_offset: usize,
}

/// Responses from the ACID service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AcidResponse {
    /// Pong response to Ping.
    Pong,

    /// Confirmation that facts were written successfully.
    WriteOk { facts_written: usize },

    /// A chunk of stream lines.
    StreamChunk(StreamChunk),

    /// Error response.
    Error { message: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
    fn acid_request_read_stream_roundtrip() {
        let req = AcidRequest::ReadStream {
            after_line: 42,
            limit: 100,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"ReadStream\""), "json was: {json}");
        let parsed: AcidRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            AcidRequest::ReadStream { after_line, limit } => {
                assert_eq!(after_line, 42);
                assert_eq!(limit, 100);
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
            next_offset: 2,
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
                assert_eq!(c.next_offset, 2);
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
