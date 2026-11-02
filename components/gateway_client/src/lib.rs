//! Gateway Client
//!
//! NNG client for connecting to the API gateway.
//! Provides typed facades for library, playback, and source services.

pub use acid_protocol::{AcidRequest, AcidResponse};
pub use admin_ipc_protocol::{AdminRequest, AdminResponse};
pub use gateway_protocol::{
    is_unsupported_request, GatewayRequest, GatewayResponse, NodeIdentity, SourceName,
};
pub use library_ipc_protocol::{LibraryRequest, LibraryResponse};
pub use media_protocol::{Command, Response};
pub use source_protocol::{SourceRequest, SourceResponse};

/// Errors that can occur when communicating through the gateway.
///
/// This is an alias for [`nng_transport::NngClientError`]. The `Service` variant
/// carries gateway-level error messages.
pub type ClientError = nng_transport::NngClientError;

/// Errors from [`GatewayClient::identify`].
#[derive(Debug, thiserror::Error)]
pub enum IdentifyError {
    /// The gateway is too old to understand the identify request.
    #[error("gateway does not support identify: {0}")]
    Unsupported(String),
    /// Transport failure or any other error.
    #[error(transparent)]
    Client(#[from] ClientError),
}

/// Map a gateway response to an identity result.
fn identity_from_response(response: GatewayResponse) -> Result<NodeIdentity, IdentifyError> {
    match response {
        GatewayResponse::Identity { identity } => Ok(identity),
        GatewayResponse::Error { message } if is_unsupported_request(&message) => {
            Err(IdentifyError::Unsupported(message))
        }
        GatewayResponse::Error { message } => Err(ClientError::Service(message).into()),
        _ => Err(ClientError::Service("Unexpected response type for identify".to_string()).into()),
    }
}

/// Client for connecting to the API gateway.
pub struct GatewayClient {
    socket: nng::Socket,
}

impl GatewayClient {
    /// Connect to the gateway at the given address.
    ///
    /// Supports both IPC and TCP addresses.
    pub fn connect(address: &str) -> Result<Self, ClientError> {
        let socket = nng_transport::connect(address)?;
        Ok(Self { socket })
    }

    /// Connect without waiting, with every request bounded by `timeout`.
    ///
    /// For probing whether a gateway answers: an unreachable address surfaces
    /// as a timeout on the first request rather than blocking here.
    pub fn connect_with_timeout(
        address: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, ClientError> {
        let socket = nng_transport::connect_with_timeout(address, timeout)?;
        Ok(Self { socket })
    }

    /// Send a raw gateway request and receive a response.
    fn request(&self, request: &GatewayRequest) -> Result<GatewayResponse, ClientError> {
        nng_transport::request_response(&self.socket, request)
    }

    // =========================================================================
    // Library facade
    // =========================================================================

    /// Send a library request through the gateway.
    pub fn library_request(&self, req: &LibraryRequest) -> Result<LibraryResponse, ClientError> {
        let envelope = GatewayRequest::Library {
            request: req.clone(),
        };
        match self.request(&envelope)? {
            GatewayResponse::Library { response } => Ok(response),
            GatewayResponse::Error { message } => Err(ClientError::Service(message)),
            _ => Err(ClientError::Service(
                "Unexpected response type for library request".to_string(),
            )),
        }
    }

    // =========================================================================
    // Playback facade
    // =========================================================================

    /// Send a playback command through the gateway.
    pub fn playback_command(&self, cmd: &Command) -> Result<Response, ClientError> {
        let envelope = GatewayRequest::Playback {
            request: cmd.clone(),
        };
        match self.request(&envelope)? {
            GatewayResponse::Playback { response } => Ok(response),
            GatewayResponse::Error { message } => Err(ClientError::Service(message)),
            _ => Err(ClientError::Service(
                "Unexpected response type for playback request".to_string(),
            )),
        }
    }

    // =========================================================================
    // Source facade
    // =========================================================================

    /// Send a source request through the gateway.
    pub fn source_request(
        &self,
        name: &str,
        req: &SourceRequest,
    ) -> Result<SourceResponse, ClientError> {
        let envelope = GatewayRequest::Source {
            name: SourceName::new(name),
            request: req.clone(),
        };
        match self.request(&envelope)? {
            GatewayResponse::Source { response, .. } => Ok(response),
            GatewayResponse::Error { message } => Err(ClientError::Service(message)),
            _ => Err(ClientError::Service(
                "Unexpected response type for source request".to_string(),
            )),
        }
    }

    // =========================================================================
    // ACID facade
    // =========================================================================

    /// Send an ACID request through the gateway.
    pub fn acid_request(&self, req: &AcidRequest) -> Result<AcidResponse, ClientError> {
        let envelope = GatewayRequest::Acid {
            request: req.clone(),
        };
        match self.request(&envelope)? {
            GatewayResponse::Acid { response } => Ok(response),
            GatewayResponse::Error { message } => Err(ClientError::Service(message)),
            _ => Err(ClientError::Service(
                "Unexpected response type for acid request".to_string(),
            )),
        }
    }

    // =========================================================================
    // Admin facade
    // =========================================================================

    /// Send an admin request through the gateway.
    pub fn admin_request(&self, req: &AdminRequest) -> Result<AdminResponse, ClientError> {
        let envelope = GatewayRequest::Admin {
            request: req.clone(),
        };
        match self.request(&envelope)? {
            GatewayResponse::Admin { response } => Ok(response),
            GatewayResponse::Error { message } => Err(ClientError::Service(message)),
            _ => Err(ClientError::Service(
                "Unexpected response type for admin request".to_string(),
            )),
        }
    }

    // =========================================================================
    // Identity
    // =========================================================================

    /// Ask the gateway which node it runs on.
    ///
    /// Returns [`IdentifyError::Unsupported`] for gateways that predate the
    /// identify request, and [`IdentifyError::Client`] for transport and other
    /// errors.
    pub fn identify(&self) -> Result<NodeIdentity, IdentifyError> {
        let response = self.request(&GatewayRequest::Identify)?;
        identity_from_response(response)
    }

    /// List available music sources.
    pub fn list_sources(&self) -> Result<Vec<SourceName>, ClientError> {
        match self.request(&GatewayRequest::ListSources)? {
            GatewayResponse::Sources { names } => Ok(names),
            GatewayResponse::Error { message } => Err(ClientError::Service(message)),
            _ => Err(ClientError::Service(
                "Unexpected response type for list_sources".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> NodeIdentity {
        NodeIdentity {
            id: Some("mdma-stage".to_string()),
            name: Some("mdma-stage".to_string()),
            version: "0.29.0".to_string(),
            proto: 1,
        }
    }

    #[test]
    fn identity_response_is_returned() {
        let got = identity_from_response(GatewayResponse::Identity {
            identity: identity(),
        })
        .unwrap();
        assert_eq!(got, identity());
    }

    #[test]
    fn old_gateway_parse_error_is_unsupported() {
        let response = GatewayResponse::Error {
            message: "Invalid request: unknown variant `identify`".to_string(),
        };
        assert!(matches!(
            identity_from_response(response),
            Err(IdentifyError::Unsupported(_))
        ));
    }

    #[test]
    fn other_gateway_error_is_client_error() {
        let response = GatewayResponse::Error {
            message: "library service unreachable".to_string(),
        };
        assert!(matches!(
            identity_from_response(response),
            Err(IdentifyError::Client(ClientError::Service(_)))
        ));
    }

    #[test]
    fn unexpected_response_is_client_error() {
        let response = GatewayResponse::Sources { names: vec![] };
        assert!(matches!(
            identity_from_response(response),
            Err(IdentifyError::Client(_))
        ));
    }
}
