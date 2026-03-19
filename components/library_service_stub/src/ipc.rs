//! IPC server for library-service-stub using nng.
//!
//! Uses request/reply pattern over IPC socket.
//! Protocol types are defined in library-ipc-protocol.

pub use library_ipc_protocol::*;

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

/// IPC Server for the library service stub
pub struct IpcServer {
    socket: nng::Socket,
}

impl IpcServer {
    /// Create and bind an IPC server
    pub fn bind(address: &str) -> Result<Self, IpcError> {
        let socket = nng::Socket::new(nng::Protocol::Rep0)?;
        socket.listen(address)?;
        Ok(Self { socket })
    }

    /// Receive a request (blocking)
    pub fn recv(&self) -> Result<LibraryRequest, IpcError> {
        let msg = self.socket.recv()?;
        let request: LibraryRequest = serde_json::from_slice(&msg)?;
        Ok(request)
    }

    /// Send a response
    pub fn send(&self, response: &LibraryResponse) -> Result<(), IpcError> {
        let data = serde_json::to_vec(response)?;
        let msg = nng::Message::from(&data[..]);
        self.socket.send(msg).map_err(|(_, e)| IpcError::Nng(e))?;
        Ok(())
    }
}
