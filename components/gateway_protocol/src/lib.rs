//! Gateway Protocol
//!
//! Envelope types for routing requests through the API gateway.
//! Three routing domains: library (core), playback (core), source (generic).

use acid_protocol::{AcidRequest, AcidResponse};
use admin_ipc_protocol::{AdminRequest, AdminResponse};
use library_ipc_protocol::{LibraryRequest, LibraryResponse};
use media_protocol::{Command, Response};
use serde::{Deserialize, Serialize};
use source_protocol::{SourceRequest, SourceResponse};

// ============================================================================
// SourceName re-export
// ============================================================================

/// Re-exported from `playback_primitives` — the canonical source name type.
pub use playback_primitives::SourceName;

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

    /// Route to the admin service.
    #[serde(rename = "admin")]
    Admin { request: AdminRequest },

    /// Ask the gateway which node it belongs to (handled by gateway itself).
    ///
    /// Old gateways do not know this variant. Their envelope parse fails and
    /// they answer `GatewayResponse::Error { message: "Invalid request: unknown
    /// variant `identify`, ..." }` (the message starts with
    /// [`INVALID_REQUEST_PREFIX`]). Clients use [`is_unsupported_request`] to
    /// tell that apart from transport errors.
    #[serde(rename = "identify")]
    Identify,
}

/// Prefix of the error message a gateway sends when it cannot parse a request envelope.
/// Frozen: gateways up to 0.29.0 emit it literally, and clients match on it.
pub const INVALID_REQUEST_PREFIX: &str = "Invalid request:";

impl GatewayResponse {
    /// The error a gateway answers when it cannot parse a request envelope.
    pub fn invalid_request(e: impl std::fmt::Display) -> Self {
        GatewayResponse::Error {
            message: format!("{} {}", INVALID_REQUEST_PREFIX, e),
        }
    }
}

/// True if a gateway-level error message means "this gateway does not
/// understand the request" (e.g. an old gateway receiving `Identify`).
pub fn is_unsupported_request(message: &str) -> bool {
    message.starts_with(INVALID_REQUEST_PREFIX)
}

/// Current version of the identity payload.
pub const IDENTITY_PROTO: u32 = 1;

/// Identity of the node a gateway runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// Stable node id: the hostname set at provisioning (not the mDNS name).
    /// `None` means the gateway was not configured with one (unverifiable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Human-readable node name. `None` means not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Gateway version.
    pub version: String,
    /// Identity payload version.
    pub proto: u32,
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

    /// Response from the admin service.
    #[serde(rename = "admin")]
    Admin { response: AdminResponse },

    /// Identity of the node this gateway runs on.
    #[serde(rename = "identity")]
    Identity { identity: NodeIdentity },

    /// Gateway-level error (routing failure, service unreachable, etc.).
    #[serde(rename = "error")]
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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
    fn admin_request_roundtrip() {
        let req = GatewayRequest::Admin {
            request: AdminRequest::Ping,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"service\":\"admin\""));
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayRequest::Admin {
                request: AdminRequest::Ping
            }
        ));
    }

    #[test]
    fn admin_response_roundtrip() {
        let resp = GatewayResponse::Admin {
            response: AdminResponse::Pong,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"service\":\"admin\""));
        let parsed: GatewayResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            GatewayResponse::Admin {
                response: AdminResponse::Pong
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
    fn identify_request_roundtrip() {
        let json = serde_json::to_string(&GatewayRequest::Identify).unwrap();
        assert_eq!(json, "{\"service\":\"identify\"}");
        let parsed: GatewayRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, GatewayRequest::Identify));
    }

    #[test]
    fn identity_response_roundtrip() {
        let identity = NodeIdentity {
            id: Some("mdma-stage".to_string()),
            name: Some("mdma-stage".to_string()),
            version: "0.29.0".to_string(),
            proto: IDENTITY_PROTO,
        };
        let resp = GatewayResponse::Identity {
            identity: identity.clone(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"service\":\"identity\""));
        assert!(json.contains("\"id\":\"mdma-stage\""));
        assert!(json.contains("\"proto\":1"));
        match serde_json::from_str::<GatewayResponse>(&json).unwrap() {
            GatewayResponse::Identity { identity: parsed } => assert_eq!(parsed, identity),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn unconfigured_identity_omits_id_and_name() {
        let resp = GatewayResponse::Identity {
            identity: NodeIdentity {
                id: None,
                name: None,
                version: "0.29.0".to_string(),
                proto: IDENTITY_PROTO,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("\"id\""));
        assert!(!json.contains("\"name\""));
        match serde_json::from_str::<GatewayResponse>(&json).unwrap() {
            GatewayResponse::Identity { identity } => {
                assert_eq!(identity.id, None);
                assert_eq!(identity.name, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn invalid_request_prefix_is_frozen() {
        // Gateways up to 0.29.0 emit this literally; clients match on it.
        assert_eq!(INVALID_REQUEST_PREFIX, "Invalid request:");
    }

    #[test]
    fn is_unsupported_request_matches_invalid_request_message() {
        let err = serde_json::from_str::<GatewayRequest>("{\"service\":\"bogus\"}").unwrap_err();
        match GatewayResponse::invalid_request(err) {
            GatewayResponse::Error { message } => {
                assert!(is_unsupported_request(&message));
            }
            _ => panic!("wrong variant"),
        }
        assert!(!is_unsupported_request("library service unreachable: x"));
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
