//! Gateway Client
//!
//! NNG client for connecting to the API gateway.
//! Provides typed facades for library, playback, and source services.

pub use acid_protocol::{AcidRequest, AcidResponse};
pub use gateway_protocol::{GatewayRequest, GatewayResponse};
pub use library_ipc_protocol::{LibraryRequest, LibraryResponse};
pub use media_protocol::{Command, Response};
pub use source_protocol::{SourceRequest, SourceResponse};

use thiserror::Error;

/// Errors that can occur when communicating through the gateway.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Connection error: {0}")]
    Connection(#[from] nng_transport::ConnectionError),

    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Gateway error: {0}")]
    Gateway(String),
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

    /// Send a raw gateway request and receive a response.
    fn request(&self, request: &GatewayRequest) -> Result<GatewayResponse, ClientError> {
        let data = serde_json::to_vec(request)?;
        let msg = nng::Message::from(&data[..]);
        self.socket
            .send(msg)
            .map_err(|(_, e)| ClientError::Nng(e))?;

        let response_msg = self.socket.recv()?;
        let response: GatewayResponse = serde_json::from_slice(&response_msg)?;
        Ok(response)
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
            GatewayResponse::Error { message } => Err(ClientError::Gateway(message)),
            _ => Err(ClientError::Gateway(
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
            GatewayResponse::Error { message } => Err(ClientError::Gateway(message)),
            _ => Err(ClientError::Gateway(
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
            name: name.to_string(),
            request: req.clone(),
        };
        match self.request(&envelope)? {
            GatewayResponse::Source { response, .. } => Ok(response),
            GatewayResponse::Error { message } => Err(ClientError::Gateway(message)),
            _ => Err(ClientError::Gateway(
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
            GatewayResponse::Error { message } => Err(ClientError::Gateway(message)),
            _ => Err(ClientError::Gateway(
                "Unexpected response type for acid request".to_string(),
            )),
        }
    }

    /// List available music sources.
    pub fn list_sources(&self) -> Result<Vec<String>, ClientError> {
        match self.request(&GatewayRequest::ListSources)? {
            GatewayResponse::Sources { names } => Ok(names),
            GatewayResponse::Error { message } => Err(ClientError::Gateway(message)),
            _ => Err(ClientError::Gateway(
                "Unexpected response type for list_sources".to_string(),
            )),
        }
    }
}
