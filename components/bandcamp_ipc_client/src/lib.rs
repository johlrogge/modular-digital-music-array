//! IPC Client for mdma-bandcamp
//!
//! NNG client wrapper for connecting to the bandcamp service.
//! Used by mdma-cli and mdma-console.

pub use bandcamp_ipc_protocol::*;

use thiserror::Error;

/// Errors that can occur when communicating with the bandcamp service.
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

/// Client for connecting to the bandcamp service.
pub struct BandcampClient {
    socket: nng::Socket,
}

impl BandcampClient {
    /// Connect to the bandcamp service at the given address.
    ///
    /// Supports both IPC (`ipc:///path/to/socket`) and TCP (`tcp://host:port`).
    /// For TCP addresses, hostnames are resolved to IPv4 addresses since NNG
    /// doesn't handle DNS resolution.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use bandcamp_ipc_client::BandcampClient;
    ///
    /// // IPC connection
    /// let client = BandcampClient::connect("ipc:///run/mdma/bandcamp.sock")?;
    ///
    /// // TCP with hostname (resolved to IPv4)
    /// let client = BandcampClient::connect("tcp://mdma-909.local:5556")?;
    /// # Ok::<(), bandcamp_ipc_client::ClientError>(())
    /// ```
    pub fn connect(address: &str) -> Result<Self, ClientError> {
        let socket = nng_transport::connect(address)?;
        Ok(Self { socket })
    }

    /// Send a request and receive a response.
    pub fn request(&self, request: &BandcampRequest) -> Result<BandcampResponse, ClientError> {
        let data = serde_json::to_vec(request)?;
        let msg = nng::Message::from(&data[..]);
        self.socket
            .send(msg)
            .map_err(|(_, e)| ClientError::Nng(e))?;

        let response_msg = self.socket.recv()?;
        let response: BandcampResponse = serde_json::from_slice(&response_msg)?;
        Ok(response)
    }

    // =========================================================================
    // Convenience Methods
    // =========================================================================

    /// Ping the service to check if it's alive.
    pub fn ping(&self) -> Result<(), ClientError> {
        match self.request(&BandcampRequest::Ping)? {
            BandcampResponse::Pong => Ok(()),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to Ping".to_string(),
            })),
        }
    }

    /// Get service status.
    pub fn status(&self) -> Result<ServiceStatus, ClientError> {
        match self.request(&BandcampRequest::GetStatus)? {
            BandcampResponse::Status(status) => Ok(status),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetStatus".to_string(),
            })),
        }
    }

    /// Reload cookies from disk.
    pub fn reload_cookies(&self) -> Result<(bool, String), ClientError> {
        match self.request(&BandcampRequest::ReloadCookies)? {
            BandcampResponse::CookiesReloaded { valid, message } => Ok((valid, message)),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to ReloadCookies".to_string(),
            })),
        }
    }

    /// Start syncing a user's collection.
    pub fn sync(&self, username: &BandcampUsername) -> Result<(String, usize, usize), ClientError> {
        match self.request(&BandcampRequest::Sync {
            username: username.clone(),
        })? {
            BandcampResponse::SyncStarted {
                username,
                total_items,
                new_items,
            } => Ok((username, total_items, new_items)),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to Sync".to_string(),
            })),
        }
    }

    /// List current downloads.
    pub fn list_downloads(&self) -> Result<Vec<DownloadStatus>, ClientError> {
        match self.request(&BandcampRequest::ListDownloads)? {
            BandcampResponse::Downloads(downloads) => Ok(downloads),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to ListDownloads".to_string(),
            })),
        }
    }

    /// Cancel a download.
    pub fn cancel_download(&self, id: &ItemId) -> Result<(), ClientError> {
        match self.request(&BandcampRequest::CancelDownload { id: id.clone() })? {
            BandcampResponse::Cancelled { .. } => Ok(()),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to CancelDownload".to_string(),
            })),
        }
    }

    /// Pause all downloads.
    pub fn pause(&self) -> Result<(), ClientError> {
        match self.request(&BandcampRequest::PauseAll)? {
            BandcampResponse::Paused => Ok(()),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PauseAll".to_string(),
            })),
        }
    }

    /// Resume downloads.
    pub fn resume(&self) -> Result<(), ClientError> {
        match self.request(&BandcampRequest::ResumeAll)? {
            BandcampResponse::Resumed => Ok(()),
            BandcampResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to ResumeAll".to_string(),
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
