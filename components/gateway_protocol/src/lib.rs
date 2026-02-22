//! Gateway Protocol
//!
//! Envelope types for routing requests through the API gateway.
//! Three routing domains: library (core), playback (core), source (generic).

use library_ipc_protocol::{LibraryRequest, LibraryResponse};
use media_protocol::{Command, Response};
use serde::{Deserialize, Serialize};
use source_protocol::{SourceRequest, SourceResponse};

// ============================================================================
// Request Envelope
// ============================================================================

/// A request routed through the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "service")]
pub enum GatewayRequest {
    /// Route to the library service.
    #[serde(rename = "library")]
    Library { request: LibraryRequest },

    /// Route to the playback service.
    #[serde(rename = "playback")]
    Playback { request: Command },

    /// Route to a named music source.
    #[serde(rename = "source")]
    Source {
        name: String,
        request: SourceRequest,
    },

    /// List available music sources (handled by gateway itself).
    #[serde(rename = "list_sources")]
    ListSources,
}

// ============================================================================
// Response Envelope
// ============================================================================

/// A response from the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "service")]
pub enum GatewayResponse {
    /// Response from the library service.
    #[serde(rename = "library")]
    Library { response: LibraryResponse },

    /// Response from the playback service.
    #[serde(rename = "playback")]
    Playback { response: Response },

    /// Response from a music source.
    #[serde(rename = "source")]
    Source {
        name: String,
        response: SourceResponse,
    },

    /// List of available source names.
    #[serde(rename = "sources")]
    Sources { names: Vec<String> },

    /// Gateway-level error (routing failure, service unreachable, etc.).
    #[serde(rename = "error")]
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_request_roundtrip() {
        let req = GatewayRequest::Library {
            request: LibraryRequest::Ping,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"service\":\"library\""));
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayRequest::Library {
                request: LibraryRequest::Ping
            }
        ));
    }

    #[test]
    fn playback_request_roundtrip() {
        let req = GatewayRequest::Playback {
            request: Command::QueueList,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"service\":\"playback\""));
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayRequest::Playback {
                request: Command::QueueList
            }
        ));
    }

    #[test]
    fn source_request_roundtrip() {
        let req = GatewayRequest::Source {
            name: "bandcamp".to_string(),
            request: SourceRequest::Sync,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"service\":\"source\""));
        assert!(json.contains("\"name\":\"bandcamp\""));
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            GatewayRequest::Source { name, request } => {
                assert_eq!(name, "bandcamp");
                assert!(matches!(request, SourceRequest::Sync));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn list_sources_roundtrip() {
        let req = GatewayRequest::ListSources;
        let json = serde_json::to_string(&req).unwrap();
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, GatewayRequest::ListSources));
    }

    #[test]
    fn library_response_roundtrip() {
        let resp = GatewayResponse::Library {
            response: LibraryResponse::Pong,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"service\":\"library\""));
        let parsed: GatewayResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayResponse::Library {
                response: LibraryResponse::Pong
            }
        ));
    }

    #[test]
    fn sources_response_roundtrip() {
        let resp = GatewayResponse::Sources {
            names: vec!["bandcamp".to_string(), "beatport".to_string()],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GatewayResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            GatewayResponse::Sources { names } => {
                assert_eq!(names, vec!["bandcamp", "beatport"]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn error_response_roundtrip() {
        let resp = GatewayResponse::Error {
            message: "service unreachable".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GatewayResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            GatewayResponse::Error { message } => {
                assert_eq!(message, "service unreachable");
            }
            _ => panic!("wrong variant"),
        }
    }
}
