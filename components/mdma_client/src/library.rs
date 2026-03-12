//! Library backend abstraction — works in both gateway and direct mode.

use crate::error::map_gw_to_lib_error;
use library_ipc_client::{
    ClientError, ContentHash, FactType, InboxPath, IngestAllItem, IngestResult, IngestSource,
    LibraryClient, LibraryRequest, LibraryResponse, PlaylistName, ProtocolError, ServiceStatus,
    TrackInfo, TrackQuery,
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
            fact_type: FactType::new(fact_type),
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
        self.ingest_file_with_source(path, None)
    }

    pub fn ingest_file_with_source(
        &self,
        path: &InboxPath,
        source: Option<IngestSource>,
    ) -> Result<IngestResult, ClientError> {
        match self.request(&LibraryRequest::IngestFile {
            path: path.clone(),
            source,
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

    // =========================================================================
    // Playlist Methods
    // =========================================================================

    pub fn playlist_list(&self) -> Result<Vec<PlaylistName>, ClientError> {
        match self.request(&LibraryRequest::PlaylistList)? {
            LibraryResponse::PlaylistNames(names) => Ok(names),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistList".to_string(),
            })),
        }
    }

    pub fn playlist_get(&self, name: &PlaylistName) -> Result<Vec<ContentHash>, ClientError> {
        match self.request(&LibraryRequest::PlaylistGet { name: name.clone() })? {
            LibraryResponse::PlaylistContent(content) => Ok(content_to_hashes(&content)),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistGet".to_string(),
            })),
        }
    }

    pub fn playlist_new(
        &self,
        name: &PlaylistName,
        hashes: &[ContentHash],
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: hashes_to_content(hashes),
        })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistNew".to_string(),
            })),
        }
    }

    pub fn playlist_append(
        &self,
        name: &PlaylistName,
        hashes: &[ContentHash],
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistAppend {
            name: name.clone(),
            content: hashes_to_content(hashes),
        })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistAppend".to_string(),
            })),
        }
    }

    pub fn playlist_replace(
        &self,
        name: &PlaylistName,
        hashes: &[ContentHash],
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistReplace {
            name: name.clone(),
            content: hashes_to_content(hashes),
        })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistReplace".to_string(),
            })),
        }
    }

    pub fn playlist_remove(&self, name: &PlaylistName) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistRemove { name: name.clone() })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistRemove".to_string(),
            })),
        }
    }

    pub fn playlist_rename(
        &self,
        from: &PlaylistName,
        to: &PlaylistName,
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistRename {
            from: from.clone(),
            to: to.clone(),
        })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistRename".to_string(),
            })),
        }
    }
}

// =========================================================================
// Private helpers
// =========================================================================

/// Serialize a slice of ContentHash to the one-hash-per-line format.
fn hashes_to_content(hashes: &[ContentHash]) -> String {
    hashes
        .iter()
        .map(|h| h.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse hash-per-line playlist content. Skips empty lines and comment lines.
/// Takes the first whitespace-separated token per line (hash may be followed by display info).
fn content_to_hashes(content: &str) -> Vec<ContentHash> {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_whitespace().next())
        .map(|token| ContentHash::new(token))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_to_content_produces_newline_separated() {
        let hashes = vec![
            ContentHash::new("sha256:aaa"),
            ContentHash::new("sha256:bbb"),
        ];
        assert_eq!(hashes_to_content(&hashes), "sha256:aaa\nsha256:bbb");
    }

    #[test]
    fn hashes_to_content_empty_produces_empty_string() {
        assert_eq!(hashes_to_content(&[]), "");
    }

    #[test]
    fn content_to_hashes_parses_one_per_line() {
        let content = "sha256:aaa\nsha256:bbb\n";
        let result = content_to_hashes(content);
        assert_eq!(
            result,
            vec![
                ContentHash::new("sha256:aaa"),
                ContentHash::new("sha256:bbb")
            ]
        );
    }

    #[test]
    fn content_to_hashes_skips_empty_lines_and_comments() {
        let content = "sha256:aaa\n\n# comment\nsha256:bbb";
        let result = content_to_hashes(content);
        assert_eq!(
            result,
            vec![
                ContentHash::new("sha256:aaa"),
                ContentHash::new("sha256:bbb")
            ]
        );
    }

    #[test]
    fn content_to_hashes_takes_first_whitespace_token() {
        let content = "sha256:abc  Artist - Title  [3:45]";
        let result = content_to_hashes(content);
        assert_eq!(result, vec![ContentHash::new("sha256:abc")]);
    }
}
