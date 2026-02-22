//! Library backend abstraction — works in both gateway and direct mode.

use crate::error::map_gw_to_lib_error;
use library_ipc_client::{
    ClientError, ContentHash, InboxPath, IngestAllItem, IngestResult, LibraryClient,
    LibraryRequest, LibraryResponse, ProtocolError, ServiceStatus, TrackInfo, TrackQuery,
};

/// Abstraction for library requests, works in both gateway and direct mode.
pub enum LibraryBackend {
    Direct(LibraryClient),
    Gateway(gateway_client::GatewayClient),
}

impl LibraryBackend {
    /// Connect directly to the library IPC socket.
    pub fn connect_direct(socket: &str) -> Result<Self, ClientError> {
        let client = LibraryClient::connect(socket)?;
        Ok(LibraryBackend::Direct(client))
    }

    /// Connect to the library via gateway.
    pub fn connect_gateway(gateway: &str) -> Result<Self, ClientError> {
        let gw = gateway_client::GatewayClient::connect(gateway).map_err(map_gw_to_lib_error)?;
        Ok(LibraryBackend::Gateway(gw))
    }

    /// Connect using gateway if provided, otherwise direct.
    pub fn connect(gateway: Option<&str>, socket: &str) -> Result<Self, ClientError> {
        match gateway {
            Some(gw) => Self::connect_gateway(gw),
            None => Self::connect_direct(socket),
        }
    }

    pub fn request(&self, req: &LibraryRequest) -> Result<LibraryResponse, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.request(req),
            LibraryBackend::Gateway(gw) => gw.library_request(req).map_err(map_gw_to_lib_error),
        }
    }

    pub fn ping(&self) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::Ping)? {
            LibraryResponse::Pong => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to Ping".to_string(),
            })),
        }
    }

    pub fn status(&self) -> Result<ServiceStatus, ClientError> {
        match self.request(&LibraryRequest::GetStatus)? {
            LibraryResponse::Status(status) => Ok(status),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetStatus".to_string(),
            })),
        }
    }

    pub fn list_tracks(&self, limit: Option<usize>) -> Result<Vec<TrackInfo>, ClientError> {
        match self.request(&LibraryRequest::ListTracks { limit })? {
            LibraryResponse::Tracks(tracks) => Ok(tracks),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to ListTracks".to_string(),
            })),
        }
    }

    pub fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ClientError> {
        match self.request(&LibraryRequest::GetTrack { hash: hash.clone() })? {
            LibraryResponse::Track(track) => Ok(track),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetTrack".to_string(),
            })),
        }
    }

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

    pub fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, ClientError> {
        match self.request(&LibraryRequest::Search {
            query: query.clone(),
        })? {
            LibraryResponse::SearchResults(tracks) => Ok(tracks),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to Search".to_string(),
            })),
        }
    }

    pub fn get_fact_values(&self, fact_type: &str) -> Result<Vec<String>, ClientError> {
        match self.request(&LibraryRequest::GetFactValues {
            fact_type: fact_type.to_string(),
        })? {
            LibraryResponse::FactValues(values) => Ok(values),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetFactValues".to_string(),
            })),
        }
    }

    pub fn inbox_queue(&self) -> Result<Vec<InboxPath>, ClientError> {
        match self.request(&LibraryRequest::GetInboxQueue)? {
            LibraryResponse::InboxQueue(paths) => Ok(paths),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetInboxQueue".to_string(),
            })),
        }
    }

    pub fn ingest_file(&self, path: &InboxPath) -> Result<IngestResult, ClientError> {
        match self.request(&LibraryRequest::IngestFile {
            path: path.clone(),
            source: None,
        })? {
            LibraryResponse::IngestResult(result) => Ok(result),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to IngestFile".to_string(),
            })),
        }
    }

    pub fn delete_inbox_file(&self, path: &InboxPath) -> Result<IngestResult, ClientError> {
        match self.request(&LibraryRequest::DeleteInboxFile { path: path.clone() })? {
            LibraryResponse::IngestResult(result) => Ok(result),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to DeleteInboxFile".to_string(),
            })),
        }
    }

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
