//! IPC server for mdma-bandcamp using nng
//!
//! Uses request/reply pattern over IPC socket.
//! Protocol types are defined in source-protocol.

pub use source_protocol::*;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Connection error: {0}")]
    Connection(String),
}

/// IPC Server for the bandcamp service
pub struct IpcServer {
    socket: nng::Socket,
}

impl IpcServer {
    /// Create and bind an IPC server
    pub fn bind(address: &str) -> Result<Self, IpcError> {
        let socket = nng::Socket::new(nng::Protocol::Rep0)?;
        socket.listen(address)?;
        tracing::info!(address = %address, "IPC server listening");
        Ok(Self { socket })
    }

    /// Add another listener address (nng supports multiple listeners on one socket)
    pub fn listen_also(&self, address: &str) -> Result<(), IpcError> {
        self.socket.listen(address)?;
        tracing::info!(address = %address, "IPC server also listening");
        Ok(())
    }

    /// Receive a request (blocking)
    pub fn recv(&self) -> Result<SourceRequest, IpcError> {
        let msg = self.socket.recv()?;
        let request: SourceRequest = serde_json::from_slice(&msg)?;
        Ok(request)
    }

    /// Send a response
    pub fn send(&self, response: &SourceResponse) -> Result<(), IpcError> {
        let data = serde_json::to_vec(response)?;
        let msg = nng::Message::from(&data[..]);
        self.socket.send(msg).map_err(|(_, e)| IpcError::Nng(e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_request() {
        let req = SourceRequest::GetStatus;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("GetStatus"));
    }

    #[test]
    fn serialize_response() {
        let resp = SourceResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Pong"));
    }
}
