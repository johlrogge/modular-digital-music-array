//! Gateway Client
//!
//! NNG client for connecting to the API gateway.
//! Provides typed facades for library, playback, and source services.

pub use acid_protocol::{AcidRequest, AcidResponse};
pub use gateway_protocol::{GatewayRequest, GatewayResponse, SourceName};
pub use library_ipc_protocol::{LibraryRequest, LibraryResponse};
pub use media_protocol::{Command, Response};
pub use source_protocol::{SourceRequest, SourceResponse};

/// Errors that can occur when communicating through the gateway.
///
/// This is an alias for [`nng_transport::NngClientError`]. The `Service` variant
/// carries gateway-level error messages.
pub type ClientError = nng_transport::NngClientError;

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
