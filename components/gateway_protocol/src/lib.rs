//! Gateway Protocol
//!
//! Envelope types for routing requests through the API gateway.
//! Three routing domains: library (core), playback (core), source (generic).

use acid_protocol::{AcidRequest, AcidResponse};
use library_ipc_protocol::{LibraryRequest, LibraryResponse};
use media_protocol::{Command, Response};
use serde::{Deserialize, Serialize};
use source_protocol::{SourceRequest, SourceResponse};
use std::fmt;

// ============================================================================
// SourceName newtype
// ============================================================================

/// Identifies a named music source (e.g. "bandcamp", "beatport").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceName(String);

impl SourceName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// ============================================================================
// Request Envelope
// ============================================================================

/// A request routed through the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "service")]
#[allow(clippy::large_enum_variant)]
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
        name: SourceName,
        request: SourceRequest,
    },

    /// List available music sources (handled by gateway itself).
    #[serde(rename = "list_sources")]
    ListSources,

    /// Route to the ACID fact store service.
    #[serde(rename = "acid")]
    Acid { request: AcidRequest },
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
        name: SourceName,
        response: SourceResponse,
    },

    /// List of available source names.
    #[serde(rename = "sources")]
    Sources { names: Vec<SourceName> },

    /// Response from the ACID fact store service.
    #[serde(rename = "acid")]
    Acid { response: AcidResponse },

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
    fn source_name_newtype_roundtrip() {
        let name = SourceName::new("bandcamp");
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"bandcamp\"");
        let parsed: SourceName = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, name);
        assert_eq!(parsed.as_str(), "bandcamp");
        assert_eq!(parsed.to_string(), "bandcamp");
    }

    #[test]
    fn source_request_roundtrip() {
        let req = GatewayRequest::Source {
            name: SourceName::new("bandcamp"),
            request: SourceRequest::Sync,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"service\":\"source\""));
        assert!(json.contains("\"name\":\"bandcamp\""));
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            GatewayRequest::Source { name, request } => {
                assert_eq!(name, SourceName::new("bandcamp"));
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
    fn acid_request_roundtrip() {
        let req = GatewayRequest::Acid {
            request: AcidRequest::Ping,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"service\":\"acid\""));
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayRequest::Acid {
                request: AcidRequest::Ping
            }
        ));
    }

    #[test]
    fn acid_response_roundtrip() {
        let resp = GatewayResponse::Acid {
            response: AcidResponse::Pong,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"service\":\"acid\""));
        let parsed: GatewayResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayResponse::Acid {
                response: AcidResponse::Pong
            }
        ));
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
            names: vec![SourceName::new("bandcamp"), SourceName::new("beatport")],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GatewayResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            GatewayResponse::Sources { names } => {
                assert_eq!(
                    names,
                    vec![SourceName::new("bandcamp"), SourceName::new("beatport")]
                );
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
