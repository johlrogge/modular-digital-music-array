//! IPC Client for mdma-library
//!
//! NNG client wrapper for connecting to the library service.
//! Used by mdma-cli and mdma-console.

pub use library_ipc_protocol::*;

use thiserror::Error;

/// Errors that can occur when communicating with the library.
///
/// Extends [`nng_transport::NngClientError`] with a `Protocol` variant for
/// library-level protocol errors.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("{0}")]
    Transport(#[from] nng_transport::NngClientError),

    #[error("Protocol error: {0}")]
    Protocol(ProtocolError),
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Transport(nng_transport::NngClientError::Serialization(e))
    }
}

impl From<nng::Error> for ClientError {
    fn from(e: nng::Error) -> Self {
        ClientError::Transport(nng_transport::NngClientError::Nng(e))
    }
}

impl From<nng_transport::ConnectionError> for ClientError {
    fn from(e: nng_transport::ConnectionError) -> Self {
        ClientError::Transport(nng_transport::NngClientError::Connection(e))
    }
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
        Ok(nng_transport::request_response(&self.socket, request)?)
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

    /// Search for tracks using a structured query.
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

    /// Get all distinct values stored for a given fact type (sorted).
    ///
    /// Useful for discovery — e.g. list all genres, labels, or keys in the library.
    ///
    /// ```bash
    /// mdma search fact-values-for genre | dmenu | xargs -I{} mdma search --genre {}
    /// ```
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
        self.ingest_file_with_source(path, None)
    }

    /// Ingest a file from the inbox with source metadata.
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

    /// Check if a fact with the given type and value exists.
    pub fn has_fact(&self, fact_type: &str, value: &str) -> Result<bool, ClientError> {
        match self.request(&LibraryRequest::HasFact {
            fact_type: FactType::new(fact_type),
            value: value.to_string(),
        })? {
            LibraryResponse::FactExists { exists, .. } => Ok(exists),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to HasFact".to_string(),
            })),
        }
    }

    /// Batch check: which of these values exist for a given fact type?
    /// Returns only the values that exist.
    pub fn has_facts(
        &self,
        fact_type: &str,
        values: Vec<String>,
    ) -> Result<Vec<String>, ClientError> {
        match self.request(&LibraryRequest::HasFacts {
            fact_type: FactType::new(fact_type),
            values,
        })? {
            LibraryResponse::FactsExist { existing, .. } => Ok(existing),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to HasFacts".to_string(),
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

    // =========================================================================
    // Playlist Methods
    // =========================================================================

    /// List all playlists.
    pub fn playlist_list(&self) -> Result<Vec<PlaylistName>, ClientError> {
        match self.request(&LibraryRequest::PlaylistList)? {
            LibraryResponse::PlaylistNames(names) => Ok(names),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistList".to_string(),
            })),
        }
    }

    /// Get the contents of a playlist as a list of content hashes.
    pub fn playlist_get(&self, name: &PlaylistName) -> Result<Vec<ContentHash>, ClientError> {
        match self.request(&LibraryRequest::PlaylistGet { name: name.clone() })? {
            LibraryResponse::PlaylistContent(content) => Ok(content_to_hashes(&content)),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistGet".to_string(),
            })),
        }
    }

    /// Create a new playlist with the given hashes.
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

    /// Append hashes to an existing playlist.
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

    /// Replace all hashes in an existing playlist.
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

    /// Remove a playlist.
    pub fn playlist_remove(&self, name: &PlaylistName) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistRemove { name: name.clone() })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistRemove".to_string(),
            })),
        }
    }

    /// Rename a playlist.
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

    /// Write a single fact for a track. Used for importing metadata from external sources.
    pub fn write_fact(&self, hash: &ContentHash, fact: MusicValue) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::WriteFact {
            hash: hash.clone(),
            fact,
        })? {
            LibraryResponse::FactWritten => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to WriteFact".to_string(),
            })),
        }
    }

    /// Retract all facts whose `FactSource.tool` matches `source_name` for every
    /// content hash that has an `ItemId` fact equal to `item_id`.
    ///
    /// Retracted attributes: Album, Title, Artist, TrackNumber, Year.
    /// ItemId itself is intentionally NOT retracted.
    pub fn retract_source_facts(
        &self,
        item_id: &str,
        source_name: &str,
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::RetractSourceFacts {
            item_id: item_id.to_string(),
            source_name: source_name.to_string(),
        })? {
            LibraryResponse::SourceFactsRetracted => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to RetractSourceFacts".to_string(),
            })),
        }
    }

    /// Look up the album title for any track tagged with `item_id`.
    ///
    /// Returns `Some(title)` if any track for that ItemId has an Album fact,
    /// otherwise `None`. If multiple tracks share the same ItemId but have
    /// different album titles (rare, should only occur mid-rename), any one
    /// value may be returned.
    pub fn get_album_title_by_item_id(&self, item_id: &str) -> Result<Option<String>, ClientError> {
        match self.request(&LibraryRequest::GetAlbumTitleByItemId {
            item_id: item_id.to_string(),
        })? {
            LibraryResponse::AlbumTitleByItemId(title) => Ok(title),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetAlbumTitleByItemId".to_string(),
            })),
        }
    }

    /// Count the number of tracks in the library whose facts include `ItemId = item_id`.
    ///
    /// Returns `0` for an unknown ItemId.
    pub fn get_track_count_for_item_id(&self, item_id: &str) -> Result<usize, ClientError> {
        match self.request(&LibraryRequest::GetTrackCountForItemId {
            item_id: item_id.to_string(),
        })? {
            LibraryResponse::TrackCountForItemId(count) => Ok(count),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetTrackCountForItemId".to_string(),
            })),
        }
    }

    /// Batch lookup: for many ItemIds at once, return a map of ItemId → album title.
    ///
    /// Absent key in the result means no album title was found for that ItemId
    /// (either unknown ID or its tracks have no Album fact yet).
    ///
    /// Prefer this over calling `get_album_title_by_item_id` in a loop — it
    /// replaces N sequential IPC round-trips with a single batched query.
    pub fn get_album_titles_by_item_ids(
        &self,
        item_ids: &[String],
    ) -> Result<std::collections::HashMap<String, String>, ClientError> {
        match self.request(&LibraryRequest::GetAlbumTitlesByItemIds {
            item_ids: item_ids.to_vec(),
        })? {
            LibraryResponse::AlbumTitlesByItemIds(map) => Ok(map),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to GetAlbumTitlesByItemIds".to_string(),
            })),
        }
    }
}

// =========================================================================
// Playlist content helpers
// =========================================================================

/// Serialize a slice of ContentHash to the one-hash-per-line format.
pub fn hashes_to_content(hashes: &[ContentHash]) -> String {
    hashes
        .iter()
        .map(|h| h.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse hash-per-line playlist content. Skips empty lines and comment lines.
/// Takes the first whitespace-separated token per line (hash may be followed by display info).
pub fn content_to_hashes(content: &str) -> Vec<ContentHash> {
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
    fn client_error_display() {
        let err = ClientError::Protocol(ProtocolError::Internal {
            message: "test".to_string(),
        });
        assert!(err.to_string().contains("test"));
    }
}
