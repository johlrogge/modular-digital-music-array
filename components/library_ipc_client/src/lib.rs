//! IPC Client for mdma-library
//!
//! NNG client wrapper for connecting to the library service.
//! Used by mdma-cli and mdma-console.

pub use library_ipc_protocol::*;

use thiserror::Error;

/// Errors that can occur when communicating with the library.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Connection error: {0}")]
    Connection(#[from] nng_transport::ConnectionError),

    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Protocol error: {0}")]
    Protocol(ProtocolError),
}

/// Client for connecting to the library service.
pub struct LibraryClient {
    socket: nng::Socket,
}

impl LibraryClient {
    /// Connect to the library service at the given address.
    ///
    /// Supports both IPC (`ipc:///path/to/socket`) and TCP (`tcp://host:port`).
    /// For TCP addresses, hostnames are resolved to IPv4 addresses.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use library_ipc_client::LibraryClient;
    ///
    /// let client = LibraryClient::connect("ipc:///run/mdma/library.sock")?;
    /// let client = LibraryClient::connect("tcp://mdma-909.local:5555")?;
    /// # Ok::<(), library_ipc_client::ClientError>(())
    /// ```
    pub fn connect(address: &str) -> Result<Self, ClientError> {
        let socket = nng_transport::connect(address)?;
        Ok(Self { socket })
    }

    /// Send a request and receive a response.
    ///
    /// This is the low-level method that sends any request type.
    /// For convenience, use the typed methods like `ping()`, `status()`, etc.
    pub fn request(&self, request: &LibraryRequest) -> Result<LibraryResponse, ClientError> {
        let data = serde_json::to_vec(request)?;
        let msg = nng::Message::from(&data[..]);
        self.socket
            .send(msg)
            .map_err(|(_, e)| ClientError::Nng(e))?;

        let response_msg = self.socket.recv()?;
        let response: LibraryResponse = serde_json::from_slice(&response_msg)?;
        Ok(response)
    }

    // =========================================================================
    // Convenience Methods
    // =========================================================================

    /// Ping the service to check if it's alive.
    pub fn ping(&self) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::Ping)? {
            LibraryResponse::Pong => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to Ping".to_string(),
            })),
        }
    }

    /// Get service status.
    pub fn status(&self) -> Result<ServiceStatus, ClientError> {
        match self.request(&LibraryRequest::GetStatus)? {
            LibraryResponse::Status(status) => Ok(status),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetStatus".to_string(),
            })),
        }
    }

    /// List tracks in the library.
    pub fn list_tracks(&self, limit: Option<usize>) -> Result<Vec<TrackInfo>, ClientError> {
        match self.request(&LibraryRequest::ListTracks { limit })? {
            LibraryResponse::Tracks(tracks) => Ok(tracks),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to ListTracks".to_string(),
            })),
        }
    }

    /// Get a specific track by content hash.
    pub fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ClientError> {
        match self.request(&LibraryRequest::GetTrack { hash: hash.clone() })? {
            LibraryResponse::Track(track) => Ok(track),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetTrack".to_string(),
            })),
        }
    }

    /// Get all facts for a track.
    pub fn get_facts(
        &self,
        hash: &ContentHash,
    ) -> Result<(ContentHash, Vec<(String, String)>), ClientError> {
        match self.request(&LibraryRequest::GetFacts { hash: hash.clone() })? {
            LibraryResponse::Facts { hash, facts } => Ok((hash, facts)),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetFacts".to_string(),
            })),
        }
    }

    /// Search for tracks.
    pub fn search(&self, query: &str) -> Result<Vec<TrackInfo>, ClientError> {
        match self.request(&LibraryRequest::Search {
            query: query.to_string(),
        })? {
            LibraryResponse::SearchResults(tracks) => Ok(tracks),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to Search".to_string(),
            })),
        }
    }

    /// Get the inbox queue.
    pub fn inbox_queue(&self) -> Result<Vec<InboxPath>, ClientError> {
        match self.request(&LibraryRequest::GetInboxQueue)? {
            LibraryResponse::InboxQueue(paths) => Ok(paths),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetInboxQueue".to_string(),
            })),
        }
    }

    /// Ingest a file from the inbox.
    pub fn ingest_file(&self, path: &InboxPath) -> Result<IngestResult, ClientError> {
        match self.request(&LibraryRequest::IngestFile { path: path.clone() })? {
            LibraryResponse::IngestResult(result) => Ok(result),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to IngestFile".to_string(),
            })),
        }
    }

    /// Delete a file from the inbox.
    pub fn delete_inbox_file(&self, path: &InboxPath) -> Result<IngestResult, ClientError> {
        match self.request(&LibraryRequest::DeleteInboxFile { path: path.clone() })? {
            LibraryResponse::IngestResult(result) => Ok(result),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to DeleteInboxFile".to_string(),
            })),
        }
    }

    /// Ingest all files in the inbox.
    pub fn ingest_all(&self) -> Result<Vec<IngestAllItem>, ClientError> {
        match self.request(&LibraryRequest::IngestAll)? {
            LibraryResponse::IngestAllResult(results) => Ok(results),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to IngestAll".to_string(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_error_display() {
        let err = ClientError::Protocol(ProtocolError::Internal {
            message: "test".to_string(),
        });
        assert!(err.to_string().contains("test"));
    }
}
