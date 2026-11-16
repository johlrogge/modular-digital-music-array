//! IPC Client for mdma-library
//!
//! NNG client wrapper for connecting to the library service.
//! Used by mdma-cli and mdma-console.

pub use library_ipc_protocol::*;
pub use music_primitives::{CanonicalHash, NotCanonical};

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

impl ClientError {
    /// True when the node did not understand the request (it is older than this client).
    pub fn is_unsupported_request(&self) -> bool {
        matches!(
            self,
            ClientError::Protocol(ProtocolError::InvalidRequest { .. })
        )
    }
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
            LibraryResponse::Track(track) => Ok(*track),
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

    /// Ingest a file from the inbox with legacy source metadata.
    pub fn ingest_file_with_source(
        &self,
        path: &InboxPath,
        source: Option<IngestSource>,
    ) -> Result<IngestResult, ClientError> {
        self.ingest_file_request(path, source, None, Vec::new())
    }

    /// Ingest a file from the inbox as `writer`, who also states its own provenance
    /// `facts` about the file.
    pub fn ingest_file_as(
        &self,
        path: &InboxPath,
        writer: &FactSource,
        facts: Vec<MusicValue>,
    ) -> Result<IngestResult, ClientError> {
        self.ingest_file_request(path, None, Some(writer.clone()), facts)
    }

    fn ingest_file_request(
        &self,
        path: &InboxPath,
        source: Option<IngestSource>,
        writer: Option<FactSource>,
        facts: Vec<MusicValue>,
    ) -> Result<IngestResult, ClientError> {
        match self.request(&LibraryRequest::IngestFile {
            path: path.clone(),
            source,
            writer,
            facts,
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

    /// Get a playlist's entries resolved to full hashes, in file order.
    pub fn playlist_entries(&self, name: &PlaylistName) -> Result<Vec<PlaylistEntry>, ClientError> {
        interpret_playlist_entries(
            self.request(&LibraryRequest::PlaylistEntries { name: name.clone() })?,
        )
    }

    /// Create a new playlist with pre-formatted content.
    ///
    /// `content` must be in the canonical `{8hash}  {Artist} - {Title}  [{duration}]`
    /// format produced by `track_formatter::format_playlist_content`.
    pub fn playlist_new(&self, name: &PlaylistName, content: String) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistNew {
            name: name.clone(),
            content,
        })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistNew".to_string(),
            })),
        }
    }

    /// Append pre-formatted content lines to an existing playlist.
    ///
    /// `content` must be in the canonical `{8hash}  {Artist} - {Title}  [{duration}]`
    /// format produced by `track_formatter::format_playlist_content`.
    pub fn playlist_append(&self, name: &PlaylistName, content: String) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::PlaylistAppend {
            name: name.clone(),
            content,
        })? {
            LibraryResponse::PlaylistContent(_) => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to PlaylistAppend".to_string(),
            })),
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
        match self.request(&LibraryRequest::PlaylistReplace {
            name: name.clone(),
            content,
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

    /// Retract a single fact for a track under the writer's own `source` (the binary
    /// states it, with its own version). `false` when no live assertion of the fact by
    /// that source existed, so nothing was retracted.
    pub fn retract_fact_as(
        &self,
        hash: &ContentHash,
        fact: MusicValue,
        source: &FactSource,
    ) -> Result<bool, ClientError> {
        match self.request(&LibraryRequest::RetractFact {
            hash: hash.clone(),
            fact,
            source: Some(source.clone()),
        })? {
            LibraryResponse::FactRetracted => Ok(true),
            LibraryResponse::NothingToRetract => Ok(false),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to RetractFact".to_string(),
            })),
        }
    }

    /// Write a single fact for a track under the writer's own `source` (the binary
    /// states it, with its own version).
    pub fn write_fact_as(
        &self,
        hash: &ContentHash,
        fact: MusicValue,
        source: &FactSource,
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::WriteFact {
            hash: hash.clone(),
            fact,
            source: Some(source.clone()),
        })? {
            LibraryResponse::FactWritten => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to WriteFact".to_string(),
            })),
        }
    }

    /// Retract the live facts of `attributes` that `source` wrote (matched on tool and
    /// origin kind) for every content hash that has an `ItemId` fact equal to
    /// `item_id`. A value another source also asserts stays (ADR-018).
    pub fn retract_source_facts(
        &self,
        item_id: &str,
        source: &FactSource,
        attributes: &AttributeList,
    ) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::RetractSourceFacts {
            item_id: item_id.to_string(),
            source: source.clone(),
            attributes: attributes.clone(),
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

    // =========================================================================
    // Track lifecycle methods
    // =========================================================================

    /// Soft-delete a track. The file/blob is retained and the track is recoverable.
    pub fn track_delete(&self, hash: &ContentHash) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::TrackDelete { hash: hash.clone() })? {
            LibraryResponse::TrackDeleted => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to TrackDelete".to_string(),
            })),
        }
    }

    /// Restore a soft-deleted track (retracts the Deleted fact).
    pub fn track_restore(&self, hash: &ContentHash) -> Result<(), ClientError> {
        match self.request(&LibraryRequest::TrackRestore { hash: hash.clone() })? {
            LibraryResponse::TrackRestored => Ok(()),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to TrackRestore".to_string(),
            })),
        }
    }

    /// Replace an old track with a new file. The new file path must be accessible
    /// by the library service on the device.
    ///
    /// Returns the new content hash and how many playlist lines were rewritten.
    pub fn track_replace(
        &self,
        old_hash: &ContentHash,
        new_file_path: &str,
    ) -> Result<(ContentHash, usize), ClientError> {
        match self.request(&LibraryRequest::TrackReplace {
            old_hash: old_hash.clone(),
            new_file_path: new_file_path.to_string(),
        })? {
            LibraryResponse::TrackReplaced {
                new_hash,
                playlists_rewritten,
            } => Ok((new_hash, playlists_rewritten)),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to TrackReplace".to_string(),
            })),
        }
    }

    /// List hidden (deleted or superseded) tracks.
    pub fn track_orphans(&self) -> Result<Vec<OrphanInfo>, ClientError> {
        match self.request(&LibraryRequest::TrackOrphans)? {
            LibraryResponse::OrphansList(items) => Ok(items),
            LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
            _ => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "Unexpected response to TrackOrphans".to_string(),
            })),
        }
    }

    /// Find duplicate tracks; with `apply`, merge the automatic groups.
    pub fn dedup_scan(&self, apply: bool) -> Result<DedupScanReport, ClientError> {
        interpret_dedup_scan(self.request(&LibraryRequest::DedupScan { apply })?)
    }

    /// Merge `losers` into `winner` (a user-confirmed duplicate group).
    pub fn dedup_merge(
        &self,
        winner: &ContentHash,
        losers: &[ContentHash],
    ) -> Result<DedupApplyReport, ClientError> {
        interpret_dedup_merged(self.request(&LibraryRequest::DedupMerge {
            winner: winner.clone(),
            losers: losers.to_vec(),
        })?)
    }

    /// List the files that merges moved to the archive.
    pub fn archive_list(&self) -> Result<ArchiveListing, ClientError> {
        interpret_archive_listing(self.request(&LibraryRequest::ArchiveList)?)
    }

    /// Permanently delete the archive's files archived at or before `up_to` (RFC 3339,
    /// as listed); `None` deletes everything.
    pub fn archive_empty(&self, up_to: Option<&str>) -> Result<ArchiveEmptyReport, ClientError> {
        interpret_archive_emptied(self.request(&LibraryRequest::ArchiveEmpty {
            up_to: up_to.map(str::to_owned),
        })?)
    }

    /// Start the identity-hash backfill (or learn that one is already running).
    pub fn identity_backfill(
        &self,
        include_pcm: bool,
    ) -> Result<IdentityBackfillStart, ClientError> {
        interpret_identity_backfill(
            self.request(&LibraryRequest::IdentityBackfill { include_pcm })?,
        )
    }

    /// Report progress of the identity-hash backfill.
    pub fn identity_backfill_status(&self) -> Result<IdentityBackfillStatus, ClientError> {
        interpret_identity_backfill_status(
            self.request(&LibraryRequest::GetIdentityBackfillStatus)?,
        )
    }

    /// Ask a running identity-hash backfill to stop; returns the current status.
    pub fn identity_backfill_cancel(&self) -> Result<IdentityBackfillStatus, ClientError> {
        interpret_identity_backfill_status(self.request(&LibraryRequest::CancelIdentityBackfill)?)
    }
}

/// Interpret the answer to `DedupScan`.
pub fn interpret_dedup_scan(response: LibraryResponse) -> Result<DedupScanReport, ClientError> {
    match response {
        LibraryResponse::DedupScan(report) => Ok(report),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to DedupScan".to_string(),
        })),
    }
}

/// Interpret the answer to `DedupMerge`.
pub fn interpret_dedup_merged(response: LibraryResponse) -> Result<DedupApplyReport, ClientError> {
    match response {
        LibraryResponse::DedupMerged(report) => Ok(report),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to DedupMerge".to_string(),
        })),
    }
}

/// Interpret the answer to `ArchiveList`.
pub fn interpret_archive_listing(response: LibraryResponse) -> Result<ArchiveListing, ClientError> {
    match response {
        LibraryResponse::ArchiveListing(listing) => Ok(listing),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to ArchiveList".to_string(),
        })),
    }
}

/// Interpret the answer to `ArchiveEmpty`.
pub fn interpret_archive_emptied(
    response: LibraryResponse,
) -> Result<ArchiveEmptyReport, ClientError> {
    match response {
        LibraryResponse::ArchiveEmptied(report) => Ok(report),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to ArchiveEmpty".to_string(),
        })),
    }
}

/// Interpret the answer to `IdentityBackfill`.
pub fn interpret_identity_backfill(
    response: LibraryResponse,
) -> Result<IdentityBackfillStart, ClientError> {
    match response {
        LibraryResponse::IdentityBackfillStarted(start) => Ok(start),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to IdentityBackfill".to_string(),
        })),
    }
}

/// Interpret the answer to `GetIdentityBackfillStatus` / `CancelIdentityBackfill`.
pub fn interpret_identity_backfill_status(
    response: LibraryResponse,
) -> Result<IdentityBackfillStatus, ClientError> {
    match response {
        LibraryResponse::IdentityBackfillStatus(status) => Ok(status),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to IdentityBackfillStatus".to_string(),
        })),
    }
}

/// Interpret the answer to `PlaylistEntries`.
pub fn interpret_playlist_entries(
    response: LibraryResponse,
) -> Result<Vec<PlaylistEntry>, ClientError> {
    match response {
        LibraryResponse::PlaylistEntries(entries) => Ok(entries),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to PlaylistEntries".to_string(),
        })),
    }
}

// =========================================================================
// Playlist content helpers
// =========================================================================

/// Parse hash-per-line playlist content. Skips empty lines and comment lines.
/// Takes the first whitespace-separated token per line (hash may be followed by display info).
pub fn content_to_hashes(content: &str) -> Vec<ContentHash> {
    playlist_hashes(content).map(ContentHash::new).collect()
}

/// The hashes of a playlist's entries, for sending to playback, which holds
/// only full hashes. Fails on the first unresolved entry, returning its
/// position and why, so nothing is half sent.
pub fn entries_for_playback(
    entries: &[PlaylistEntry],
) -> Result<Vec<CanonicalHash>, UnplayableEntry> {
    match unresolved_entries(entries).next() {
        Some(unplayable) => Err(unplayable),
        None => Ok(track_hashes(entries)),
    }
}

/// The full hash of each available entry of a playlist, in order.
pub fn track_hashes(entries: &[PlaylistEntry]) -> Vec<CanonicalHash> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            PlaylistEntry::Track { hash } => Some(hash.clone()),
            PlaylistEntry::Unresolved { .. } => None,
        })
        .collect()
}

/// The entries of a playlist that are not available, with their 1-based positions.
pub fn unresolved_entries(entries: &[PlaylistEntry]) -> impl Iterator<Item = UnplayableEntry> + '_ {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            PlaylistEntry::Track { .. } => None,
            PlaylistEntry::Unresolved { error } => Some(UnplayableEntry {
                position: index + 1,
                error: error.clone(),
            }),
        })
}

/// Why an entry has no track: the library said it is unresolved, or looking
/// up its track failed.
#[derive(Debug, Error)]
pub enum EntryTrackError<E> {
    /// The library could not resolve the entry.
    #[error("{0}")]
    Unresolved(EntryError),
    /// The entry resolved, but fetching its track failed.
    #[error("cannot look up {hash}: {source}")]
    Lookup {
        hash: CanonicalHash,
        #[source]
        source: E,
    },
}

/// The track of each entry of a playlist, in order and with duplicates.
///
/// Unresolved entries and failed lookups yield an error in their place, so the
/// two stay distinguishable and no entry is dropped. `get_track` is called once
/// per resolved entry, with its full hash.
pub fn entry_tracks<'a, E>(
    entries: &'a [PlaylistEntry],
    mut get_track: impl FnMut(&ContentHash) -> Result<TrackInfo, E> + 'a,
) -> impl Iterator<Item = Result<TrackInfo, EntryTrackError<E>>> + 'a {
    entries.iter().map(move |entry| match entry {
        PlaylistEntry::Track { hash } => {
            get_track(hash.as_content_hash()).map_err(|source| EntryTrackError::Lookup {
                hash: hash.clone(),
                source,
            })
        }
        PlaylistEntry::Unresolved { error } => Err(EntryTrackError::Unresolved(error.clone())),
    })
}

/// A playlist entry that cannot be sent to playback.
#[derive(Debug, Error, PartialEq)]
#[error("entry {position}: {error}")]
pub struct UnplayableEntry {
    /// 1-based position among the playlist's track entries; comments and blank
    /// lines are not counted.
    pub position: usize,
    pub error: EntryError,
}

/// A hash that could not be resolved to a full hash.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// The library could not resolve the hash (unknown, ambiguous, or the
    /// library failed).
    #[error("cannot resolve {hash}: {source}")]
    Lookup {
        hash: ContentHash,
        #[source]
        source: ClientError,
    },
    /// The library answered with a hash that is not in full form (a legacy
    /// short-hash entity in the index).
    #[error("library returned non-full hash {returned} for {hash}")]
    NotCanonical {
        hash: ContentHash,
        returned: ContentHash,
    },
    /// No entry of the playback queue starts with the hash.
    #[error("cannot resolve {hash}: not in the queue")]
    NotInQueue { hash: ContentHash },
    /// Several different entries of the playback queue start with the hash.
    #[error("cannot resolve {hash}: it matches several entries of the queue")]
    AmbiguousInQueue { hash: ContentHash },
    /// The hash names one entry of the queue and a different track of the library.
    #[error("cannot resolve {hash}: it names {queued} in the queue and {library} in the library")]
    AmbiguousAcrossQueueAndLibrary {
        hash: ContentHash,
        queued: CanonicalHash,
        library: CanonicalHash,
    },
}

impl ResolveError {
    /// The hash, as given, that failed to resolve.
    pub fn hash(&self) -> &ContentHash {
        match self {
            ResolveError::Lookup { hash, .. }
            | ResolveError::NotCanonical { hash, .. }
            | ResolveError::NotInQueue { hash }
            | ResolveError::AmbiguousInQueue { hash }
            | ResolveError::AmbiguousAcrossQueueAndLibrary { hash, .. } => hash,
        }
    }

    /// True when the library could not be reached, as opposed to refusing the hash.
    pub fn is_transport(&self) -> bool {
        matches!(
            self,
            ResolveError::Lookup {
                source: ClientError::Transport(_),
                ..
            }
        )
    }
}

/// Resolve one hash, prefix or full, to the canonical hash of its track.
///
/// Every hash goes to `get_track`, the library's own resolution, whatever its
/// length: a hash that does not name a visible track is refused, full or not.
pub fn resolve_full_hash(
    hash: &ContentHash,
    get_track: impl FnOnce(&ContentHash) -> Result<TrackInfo, ClientError>,
) -> Result<CanonicalHash, ResolveError> {
    let track = get_track(hash).map_err(|source| ResolveError::Lookup {
        hash: hash.clone(),
        source,
    })?;
    CanonicalHash::try_from(track.content_hash).map_err(|NotCanonical(returned)| {
        ResolveError::NotCanonical {
            hash: hash.clone(),
            returned,
        }
    })
}

/// Resolve every hash, prefix or full, to the canonical hash of its track, as
/// [`resolve_full_hash`] does for one.
///
/// The resolver for clients that add hashes to playback, which holds only full
/// hashes. All hashes are resolved before any is returned, and the first that is
/// unknown, ambiguous or resolves to a non-full hash fails the whole batch, so
/// nothing is half sent.
pub fn resolve_full_hashes(
    hashes: &[ContentHash],
    get_track: impl Fn(&ContentHash) -> Result<TrackInfo, ClientError>,
) -> Result<Vec<CanonicalHash>, ResolveError> {
    hashes
        .iter()
        .map(|hash| resolve_full_hash(hash, &get_track))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with(hash: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
        }
    }

    fn full(tag: &str) -> String {
        format!("sha256:{}", tag.repeat(64))
    }

    #[test]
    fn resolve_full_hashes_maps_each_prefix_through_the_library() {
        let lookup = |h: &ContentHash| Ok(track_with(&full(h.as_str())));
        let got = resolve_full_hashes(&[ContentHash::new("a"), ContentHash::new("b")], lookup);
        let got: Vec<ContentHash> = got.unwrap().into_iter().map(Into::into).collect();
        assert_eq!(
            got,
            vec![ContentHash::new(full("a")), ContentHash::new(full("b"))]
        );
    }

    #[test]
    fn resolve_full_hashes_looks_up_full_hashes_too() {
        let asked = std::cell::RefCell::new(Vec::new());
        let lookup = |h: &ContentHash| {
            asked.borrow_mut().push(h.clone());
            Ok(track_with(h.as_str()))
        };
        let got = resolve_full_hashes(&[ContentHash::new(full("c"))], lookup).unwrap();
        assert_eq!(got[0], ContentHash::new(full("c")));
        assert_eq!(asked.into_inner(), vec![ContentHash::new(full("c"))]);
    }

    #[test]
    fn resolve_full_hashes_refuses_an_unknown_full_hash() {
        let lookup = |h: &ContentHash| -> Result<TrackInfo, ClientError> {
            Err(ClientError::Protocol(ProtocolError::TrackNotFound {
                hash: h.to_string(),
            }))
        };
        let err = resolve_full_hashes(&[ContentHash::new(full("d"))], lookup).unwrap_err();
        assert!(matches!(err, ResolveError::Lookup { .. }));
        assert_eq!(err.hash(), &ContentHash::new(full("d")));
    }

    #[test]
    fn resolve_full_hashes_refuses_a_library_answer_that_is_not_full() {
        let lookup = |_: &ContentHash| Ok(track_with("9fb4105e"));
        let err = resolve_full_hashes(&[ContentHash::new("9fb4")], lookup).unwrap_err();
        assert!(matches!(err, ResolveError::NotCanonical { .. }));
        assert_eq!(
            err.to_string(),
            "library returned non-full hash 9fb4105e for 9fb4"
        );
    }

    #[test]
    fn resolve_error_is_transport_only_for_transport_failures() {
        let protocol = ResolveError::Lookup {
            hash: ContentHash::new("a"),
            source: ClientError::Protocol(ProtocolError::Internal {
                message: "x".to_string(),
            }),
        };
        assert!(!protocol.is_transport());
        let transport = ResolveError::Lookup {
            hash: ContentHash::new("a"),
            source: ClientError::Transport(nng_transport::NngClientError::Nng(nng::Error::Closed)),
        };
        assert!(transport.is_transport());
    }

    #[test]
    fn resolve_full_hashes_fails_on_the_first_unknown_hash_naming_it() {
        let lookup = |h: &ContentHash| match h.as_str() {
            "unknown" | "also-unknown" => Err(ClientError::Protocol(ProtocolError::Internal {
                message: "no such track".to_string(),
            })),
            other => Ok(track_with(&full(other))),
        };
        let err = resolve_full_hashes(
            &[
                ContentHash::new("a"),
                ContentHash::new("unknown"),
                ContentHash::new("also-unknown"),
            ],
            lookup,
        )
        .unwrap_err();
        assert_eq!(err.hash(), &ContentHash::new("unknown"));
        assert!(err.to_string().contains("cannot resolve unknown"), "{err}");
    }

    fn track_entry(tag: &str) -> PlaylistEntry {
        PlaylistEntry::Track {
            hash: CanonicalHash::try_from(ContentHash::new(full(tag))).unwrap(),
        }
    }

    fn unknown_entry(token: &str) -> PlaylistEntry {
        PlaylistEntry::Unresolved {
            error: EntryError::Unknown {
                hash: ContentHash::new(token),
            },
        }
    }

    #[test]
    fn entries_for_playback_returns_tracks_in_order_with_duplicates() {
        let entries = [track_entry("b"), track_entry("a"), track_entry("b")];
        let got: Vec<ContentHash> = entries_for_playback(&entries)
            .unwrap()
            .into_iter()
            .map(Into::into)
            .collect();
        assert_eq!(
            got,
            vec![
                ContentHash::new(full("b")),
                ContentHash::new(full("a")),
                ContentHash::new(full("b")),
            ]
        );
    }

    #[test]
    fn entries_for_playback_fails_on_first_unresolved_with_its_position() {
        let entries = [
            track_entry("a"),
            unknown_entry("first"),
            track_entry("b"),
            unknown_entry("second"),
        ];
        let unplayable = entries_for_playback(&entries).unwrap_err();
        assert_eq!(unplayable.position, 2, "1-based");
        assert_eq!(
            unplayable.error,
            EntryError::Unknown {
                hash: ContentHash::new("first")
            }
        );
        assert_eq!(unplayable.to_string(), "entry 2: unknown track first");
    }

    #[test]
    fn entry_tracks_keeps_unresolved_and_lookup_failures_apart() {
        let entries = [track_entry("a"), unknown_entry("gone"), track_entry("b")];
        let bad = full("b");
        let got: Vec<_> = entry_tracks(&entries, |h: &ContentHash| {
            if h.as_str() == bad {
                Err("boom".to_string())
            } else {
                Ok(track_with(h.as_str()))
            }
        })
        .collect();
        assert_eq!(got.len(), 3);
        assert!(got[0].is_ok());
        assert!(matches!(
            &got[1],
            Err(EntryTrackError::Unresolved(EntryError::Unknown { hash }))
                if hash == &ContentHash::new("gone")
        ));
        match &got[2] {
            Err(EntryTrackError::Lookup { hash, source }) => {
                assert_eq!(hash.as_content_hash().as_str(), bad);
                assert_eq!(source, "boom");
            }
            _ => panic!("expected a lookup failure"),
        }
    }

    #[test]
    fn entry_tracks_keeps_order_and_duplicates() {
        let entries = [track_entry("b"), track_entry("a"), track_entry("b")];
        let got: Vec<ContentHash> = entry_tracks(&entries, |h: &ContentHash| {
            Ok::<_, String>(track_with(h.as_str()))
        })
        .map(|t| t.unwrap().content_hash)
        .collect();
        assert_eq!(
            got,
            vec![
                ContentHash::new(full("b")),
                ContentHash::new(full("a")),
                ContentHash::new(full("b")),
            ]
        );
    }

    #[test]
    fn unresolved_entries_number_positions_from_one() {
        let entries = [
            unknown_entry("first"),
            track_entry("a"),
            unknown_entry("second"),
        ];
        let positions: Vec<usize> = unresolved_entries(&entries).map(|u| u.position).collect();
        assert_eq!(positions, vec![1, 3]);
    }

    #[test]
    fn invalid_request_is_unsupported_request() {
        let err = ClientError::Protocol(ProtocolError::InvalidRequest {
            message: "unknown variant".to_string(),
        });
        assert!(err.is_unsupported_request());
    }

    #[test]
    fn other_protocol_error_is_not_unsupported_request() {
        let err = ClientError::Protocol(ProtocolError::Internal {
            message: "boom".to_string(),
        });
        assert!(!err.is_unsupported_request());
    }

    #[test]
    fn client_error_display() {
        let err = ClientError::Protocol(ProtocolError::Internal {
            message: "test".to_string(),
        });
        assert!(err.to_string().contains("test"));
    }
}
