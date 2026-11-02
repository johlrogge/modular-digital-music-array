//! IPC server for mdma-bandcamp using nng
//!
//! Uses request/reply pattern over IPC socket.
//! Protocol types are defined in source-protocol.

pub use source_protocol::*;

use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum IpcError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Connection error: {0}")]
    Connection(String),
}

/// Parse a raw message into a request.
pub fn parse_request(bytes: &[u8]) -> Result<SourceRequest, IpcError> {
    Ok(serde_json::from_slice(bytes)?)
}

/// IPC Server for the bandcamp service
pub struct IpcServer {
    socket: nng::Socket,
}

impl IpcServer {
    /// Create an IPC server from a pre-built socket (obtained via `service::create_sockets`).
    pub fn new(socket: nng::Socket) -> Self {
        Self { socket }
    }

    /// Add another listener address (nng supports multiple listeners on one socket)
    #[allow(dead_code)]
    pub fn listen_also(&self, address: &str) -> Result<(), IpcError> {
        self.socket.listen(address)?;
        tracing::info!(address = %address, "IPC server also listening");
        Ok(())
    }

    /// Receive the raw bytes of a request (blocking).
    ///
    /// Parsing is separate ([`parse_request`]) so a malformed request can
    /// still be answered: on a REP socket every received message needs a reply.
    pub fn recv_raw(&self) -> Result<Vec<u8>, IpcError> {
        let msg = self.socket.recv()?;
        Ok(msg.as_slice().to_vec())
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
    fn parse_request_roundtrip_and_garbage() {
        let bytes = serde_json::to_vec(&SourceRequest::GetStatus).unwrap();
        assert!(matches!(
            parse_request(&bytes),
            Ok(SourceRequest::GetStatus)
        ));
        assert!(parse_request(b"{\"NoSuchRequest\":1}").is_err());
        assert!(parse_request(b"not json").is_err());
    }

    #[test]
    fn serialize_response() {
        let resp = SourceResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Pong"));
    }
}
