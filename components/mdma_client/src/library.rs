//! Library backend abstraction — works in both gateway and direct mode.

use crate::error::map_gw_to_lib_error;
use library_ipc_client::{
    content_to_hashes, ClientError, ContentHash, FactType, InboxPath, IngestAllItem, IngestResult,
    IngestSource, LibraryClient, LibraryRequest, LibraryResponse, MusicValue, OrphanInfo,
    PlaylistName, ProtocolError, ServiceStatus, TrackInfo, TrackQuery,
};
use track_formatter::format_playlist_content;

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
        match self {
            LibraryBackend::Direct(c) => c.ping(),
            LibraryBackend::Gateway(_) => interpret_ping(self.request(&LibraryRequest::Ping)?),
        }
    }

    pub fn status(&self) -> Result<ServiceStatus, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.status(),
            LibraryBackend::Gateway(_) => {
                interpret_status(self.request(&LibraryRequest::GetStatus)?)
            }
        }
    }

    pub fn list_tracks(&self, limit: Option<usize>) -> Result<Vec<TrackInfo>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.list_tracks(limit),
            LibraryBackend::Gateway(_) => {
                interpret_tracks(self.request(&LibraryRequest::ListTracks { limit })?)
            }
        }
    }

    pub fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.get_track(hash),
            LibraryBackend::Gateway(_) => {
                interpret_track(self.request(&LibraryRequest::GetTrack { hash: hash.clone() })?)
            }
        }
    }

    pub fn get_facts(
        &self,
        hash: &ContentHash,
    ) -> Result<(ContentHash, Vec<(String, String)>), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.get_facts(hash),
            LibraryBackend::Gateway(_) => {
                interpret_facts(self.request(&LibraryRequest::GetFacts { hash: hash.clone() })?)
            }
        }
    }

    pub fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.search(query),
            LibraryBackend::Gateway(_) => {
                interpret_search_results(self.request(&LibraryRequest::Search {
                    query: query.clone(),
                })?)
            }
        }
    }

    pub fn get_fact_values(&self, fact_type: &str) -> Result<Vec<String>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.get_fact_values(fact_type),
            LibraryBackend::Gateway(_) => {
                interpret_fact_values(self.request(&LibraryRequest::GetFactValues {
                    fact_type: FactType::new(fact_type),
                })?)
            }
        }
    }

    pub fn inbox_queue(&self) -> Result<Vec<InboxPath>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.inbox_queue(),
            LibraryBackend::Gateway(_) => {
                interpret_inbox_queue(self.request(&LibraryRequest::GetInboxQueue)?)
            }
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
        match self {
            LibraryBackend::Direct(c) => c.ingest_file_with_source(path, source),
            LibraryBackend::Gateway(_) => {
                interpret_ingest_result(self.request(&LibraryRequest::IngestFile {
                    path: path.clone(),
                    source,
                })?)
            }
        }
    }

    pub fn delete_inbox_file(&self, path: &InboxPath) -> Result<IngestResult, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.delete_inbox_file(path),
            LibraryBackend::Gateway(_) => interpret_ingest_result(
                self.request(&LibraryRequest::DeleteInboxFile { path: path.clone() })?,
            ),
        }
    }

    pub fn ingest_all(&self) -> Result<Vec<IngestAllItem>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.ingest_all(),
            LibraryBackend::Gateway(_) => {
                interpret_ingest_all(self.request(&LibraryRequest::IngestAll)?)
            }
        }
    }

    // =========================================================================
    // Playlist Methods
    // =========================================================================

    pub fn playlist_list(&self) -> Result<Vec<PlaylistName>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_list(),
            LibraryBackend::Gateway(_) => {
                interpret_playlist_names(self.request(&LibraryRequest::PlaylistList)?)
            }
        }
    }

    pub fn playlist_get(&self, name: &PlaylistName) -> Result<Vec<ContentHash>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_get(name),
            LibraryBackend::Gateway(_) => interpret_playlist_content(
                self.request(&LibraryRequest::PlaylistGet { name: name.clone() })?,
            ),
        }
    }

    /// Create a new playlist with pre-formatted content.
    ///
    /// `content` must be in the canonical `{8hash}  {Artist} - {Title}  [{duration}]`
    /// format produced by `track_formatter::format_playlist_content`.
    pub fn playlist_new(&self, name: &PlaylistName, content: String) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_new(name, content.clone()),
            LibraryBackend::Gateway(_) => {
                interpret_playlist_ok(self.request(&LibraryRequest::PlaylistNew {
                    name: name.clone(),
                    content,
                })?)
            }
        }
    }

    /// Append pre-formatted content lines to an existing playlist.
    ///
    /// `content` must be in the canonical `{8hash}  {Artist} - {Title}  [{duration}]`
    /// format produced by `track_formatter::format_playlist_content`.
    pub fn playlist_append(&self, name: &PlaylistName, content: String) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_append(name, content),
            LibraryBackend::Gateway(_) => {
                interpret_playlist_ok(self.request(&LibraryRequest::PlaylistAppend {
                    name: name.clone(),
                    content,
                })?)
            }
        }
    }

    /// Replace a playlist with pre-formatted content.
    ///
    /// `content` must be in the canonical `{8hash}  {Artist} - {Title}  [{duration}]`
    /// format produced by `track_formatter::format_playlist_content`.
    pub fn playlist_replace(
        &self,
        name: &PlaylistName,
        content: String,
    ) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_replace(name, content),
            LibraryBackend::Gateway(_) => {
                interpret_playlist_ok(self.request(&LibraryRequest::PlaylistReplace {
                    name: name.clone(),
                    content,
                })?)
            }
        }
    }

    /// Resolve a slice of content hashes to `TrackInfo` records and format them
    /// as canonical playlist content.
    ///
    /// Hashes that fail to resolve are skipped with a warning. Returns the
    /// formatted string ready to pass to `playlist_replace` or `playlist_append`.
    pub fn resolve_and_format_playlist(&self, hashes: &[ContentHash]) -> String {
        let tracks: Vec<TrackInfo> = hashes
            .iter()
            .filter_map(|h| match self.get_track(h) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!("Failed to resolve hash {}: {}", h.as_str(), e);
                    None
                }
            })
            .collect();
        format_playlist_content(&tracks)
    }

    pub fn playlist_remove(&self, name: &PlaylistName) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_remove(name),
            LibraryBackend::Gateway(_) => interpret_playlist_ok(
                self.request(&LibraryRequest::PlaylistRemove { name: name.clone() })?,
            ),
        }
    }

    pub fn playlist_rename(
        &self,
        from: &PlaylistName,
        to: &PlaylistName,
    ) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.playlist_rename(from, to),
            LibraryBackend::Gateway(_) => {
                interpret_playlist_ok(self.request(&LibraryRequest::PlaylistRename {
                    from: from.clone(),
                    to: to.clone(),
                })?)
            }
        }
    }

    /// Write a bookmark fact for a track.
    pub fn write_bookmark(
        &self,
        hash: &ContentHash,
        scope: Option<String>,
    ) -> Result<(), ClientError> {
        interpret_bookmark_written(self.request(&LibraryRequest::WriteBookmark {
            hash: hash.clone(),
            scope,
        })?)
    }

    /// Write a single fact for a track. Used for importing metadata from external sources.
    pub fn write_fact(&self, hash: &ContentHash, fact: MusicValue) -> Result<(), ClientError> {
        interpret_fact_written(self.request(&LibraryRequest::WriteFact {
            hash: hash.clone(),
            fact,
        })?)
    }

    /// Retract a single fact for a track. Used for removing manually-added metadata.
    pub fn retract_fact(&self, hash: &ContentHash, fact: MusicValue) -> Result<(), ClientError> {
        interpret_fact_retracted(self.request(&LibraryRequest::RetractFact {
            hash: hash.clone(),
            fact,
        })?)
    }

    // =========================================================================
    // Track lifecycle methods
    // =========================================================================

    /// Soft-delete a track. File/blob retained; recoverable via `track_restore`.
    pub fn track_delete(&self, hash: &ContentHash) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.track_delete(hash),
            LibraryBackend::Gateway(_) => interpret_track_deleted(
                self.request(&LibraryRequest::TrackDelete { hash: hash.clone() })?,
            ),
        }
    }

    /// Restore a soft-deleted track (retracts the Deleted fact).
    pub fn track_restore(&self, hash: &ContentHash) -> Result<(), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.track_restore(hash),
            LibraryBackend::Gateway(_) => interpret_track_restored(
                self.request(&LibraryRequest::TrackRestore { hash: hash.clone() })?,
            ),
        }
    }

    /// Replace an old track with a new file. `new_file_path` must be on the device.
    pub fn track_replace(
        &self,
        old_hash: &ContentHash,
        new_file_path: &str,
    ) -> Result<(ContentHash, usize), ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.track_replace(old_hash, new_file_path),
            LibraryBackend::Gateway(_) => {
                interpret_track_replaced(self.request(&LibraryRequest::TrackReplace {
                    old_hash: old_hash.clone(),
                    new_file_path: new_file_path.to_string(),
                })?)
            }
        }
    }

    /// List hidden (deleted or superseded) tracks.
    pub fn track_orphans(&self) -> Result<Vec<OrphanInfo>, ClientError> {
        match self {
            LibraryBackend::Direct(c) => c.track_orphans(),
            LibraryBackend::Gateway(_) => {
                interpret_orphans_list(self.request(&LibraryRequest::TrackOrphans)?)
            }
        }
    }
}

// =========================================================================
// Shared response interpreters (used by Gateway arm)
// =========================================================================

fn unexpected(operation: &str) -> ClientError {
    ClientError::Protocol(ProtocolError::Internal {
        message: format!("Unexpected response to {}", operation),
    })
}

fn interpret_ping(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::Pong => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("Ping")),
    }
}

fn interpret_status(response: LibraryResponse) -> Result<ServiceStatus, ClientError> {
    match response {
        LibraryResponse::Status(status) => Ok(status),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("GetStatus")),
    }
}

fn interpret_tracks(response: LibraryResponse) -> Result<Vec<TrackInfo>, ClientError> {
    match response {
        LibraryResponse::Tracks(tracks) => Ok(tracks),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("ListTracks")),
    }
}

fn interpret_track(response: LibraryResponse) -> Result<TrackInfo, ClientError> {
    match response {
        LibraryResponse::Track(track) => Ok(*track),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("GetTrack")),
    }
}

fn interpret_facts(
    response: LibraryResponse,
) -> Result<(ContentHash, Vec<(String, String)>), ClientError> {
    match response {
        LibraryResponse::Facts { hash, facts } => Ok((hash, facts)),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("GetFacts")),
    }
}

fn interpret_search_results(response: LibraryResponse) -> Result<Vec<TrackInfo>, ClientError> {
    match response {
        LibraryResponse::SearchResults(tracks) => Ok(tracks),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("Search")),
    }
}

fn interpret_fact_values(response: LibraryResponse) -> Result<Vec<String>, ClientError> {
    match response {
        LibraryResponse::FactValues(values) => Ok(values),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("GetFactValues")),
    }
}

fn interpret_inbox_queue(response: LibraryResponse) -> Result<Vec<InboxPath>, ClientError> {
    match response {
        LibraryResponse::InboxQueue(paths) => Ok(paths),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("GetInboxQueue")),
    }
}

fn interpret_ingest_result(response: LibraryResponse) -> Result<IngestResult, ClientError> {
    match response {
        LibraryResponse::IngestResult(result) => Ok(result),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("IngestFile")),
    }
}

fn interpret_ingest_all(response: LibraryResponse) -> Result<Vec<IngestAllItem>, ClientError> {
    match response {
        LibraryResponse::IngestAllResult(results) => Ok(results),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("IngestAll")),
    }
}

fn interpret_playlist_names(response: LibraryResponse) -> Result<Vec<PlaylistName>, ClientError> {
    match response {
        LibraryResponse::PlaylistNames(names) => Ok(names),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("PlaylistList")),
    }
}

fn interpret_playlist_content(response: LibraryResponse) -> Result<Vec<ContentHash>, ClientError> {
    match response {
        LibraryResponse::PlaylistContent(content) => Ok(content_to_hashes(&content)),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("PlaylistGet")),
    }
}

fn interpret_playlist_ok(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::PlaylistContent(_) => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("PlaylistOperation")),
    }
}

fn interpret_bookmark_written(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::BookmarkWritten => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("WriteBookmark")),
    }
}

fn interpret_fact_written(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::FactWritten => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("WriteFact")),
    }
}

fn interpret_fact_retracted(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::FactRetracted => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("RetractFact")),
    }
}

fn interpret_track_deleted(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::TrackDeleted => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("TrackDelete")),
    }
}

fn interpret_track_restored(response: LibraryResponse) -> Result<(), ClientError> {
    match response {
        LibraryResponse::TrackRestored => Ok(()),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("TrackRestore")),
    }
}

fn interpret_track_replaced(
    response: LibraryResponse,
) -> Result<(ContentHash, usize), ClientError> {
    match response {
        LibraryResponse::TrackReplaced {
            new_hash,
            playlists_rewritten,
        } => Ok((new_hash, playlists_rewritten)),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("TrackReplace")),
    }
}

fn interpret_orphans_list(response: LibraryResponse) -> Result<Vec<OrphanInfo>, ClientError> {
    match response {
        LibraryResponse::OrphansList(items) => Ok(items),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(unexpected("TrackOrphans")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn unexpected_error_contains_operation_name() {
        let err = unexpected("Ping");
        assert!(err.to_string().contains("Ping"));
    }

    #[test]
    fn interpret_ping_pong_ok() {
        assert!(interpret_ping(LibraryResponse::Pong).is_ok());
    }

    #[test]
    fn interpret_ping_unexpected_variant_is_err() {
        assert!(interpret_ping(LibraryResponse::Tracks(vec![])).is_err());
    }

    #[test]
    fn interpret_bookmark_written_ok() {
        assert!(interpret_bookmark_written(LibraryResponse::BookmarkWritten).is_ok());
    }

    #[test]
    fn interpret_bookmark_written_error_propagates() {
        let err = LibraryResponse::Error(ProtocolError::Internal {
            message: "fail".to_string(),
        });
        assert!(interpret_bookmark_written(err).is_err());
    }

    #[test]
    fn interpret_bookmark_written_unexpected_variant_is_err() {
        assert!(interpret_bookmark_written(LibraryResponse::Pong).is_err());
    }

    #[test]
    fn interpret_fact_written_ok() {
        assert!(interpret_fact_written(LibraryResponse::FactWritten).is_ok());
    }

    #[test]
    fn interpret_fact_written_error_propagates() {
        let err = LibraryResponse::Error(ProtocolError::Internal {
            message: "fail".to_string(),
        });
        assert!(interpret_fact_written(err).is_err());
    }

    #[test]
    fn interpret_fact_written_unexpected_variant_is_err() {
        assert!(interpret_fact_written(LibraryResponse::Pong).is_err());
    }

    #[test]
    fn interpret_fact_retracted_ok() {
        assert!(interpret_fact_retracted(LibraryResponse::FactRetracted).is_ok());
    }

    #[test]
    fn interpret_fact_retracted_error_propagates() {
        let err = LibraryResponse::Error(ProtocolError::Internal {
            message: "fail".to_string(),
        });
        assert!(interpret_fact_retracted(err).is_err());
    }

    #[test]
    fn interpret_fact_retracted_unexpected_variant_is_err() {
        assert!(interpret_fact_retracted(LibraryResponse::Pong).is_err());
    }
}
