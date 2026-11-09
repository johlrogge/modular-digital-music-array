//! In-memory library service stub for BDD tests.
//!
//! Reads facts from a `facts.jsonl` file on construction and indexes them in
//! memory. Implements the same IPC handler loop as the real service but without
//! ACID, ingestion, or file-system side effects.

use crate::ipc::{
    parse_request, BeatGridInfo, Bpm, ContentHash, CueInfo, DurationSeconds, FactType, IpcServer,
    LibraryRequest, LibraryResponse, MarkFilter, OrphanInfo, OrphanReason, PlaylistName,
    ProtocolError, ServiceStatus, TrackInfo, TrackQuery,
};
use library_search::{matches_query, TrackFields};
use music_facts::MusicValue;
use music_primitives::{EnergyLevel, TrackRole};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("IPC error: {0}")]
    Ipc(#[from] crate::ipc::IpcError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Compact track record stored in-memory.
#[derive(Clone, Default)]
struct IndexedTrack {
    content_hash: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    label: Option<String>,
    genre: Option<String>,
    styles: Vec<String>,
    duration_seconds: Option<u32>,
    bpm: Option<f32>,
    key: Option<String>,
    year: Option<u32>,
    source: Option<String>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Projected cue points (assert-only; retract not handled in stub).
    memory_cues: Vec<CueInfo>,
    /// Projected beat-grid anchor (last asserted wins).
    beat_grid: Option<BeatGridInfo>,
    /// DJ curation role.
    role: Option<TrackRole>,
    /// Energy level 1–10.
    energy: Option<EnergyLevel>,
    /// Live `Bookmarked` values, from loaded facts and `WriteBookmark`. A loaded
    /// retract removes the matching value, as the real service's provenance does.
    /// Retracts of every other fact are skipped, not applied.
    bookmarks: Vec<MusicValue>,
}

impl IndexedTrack {
    fn is_hidden(&self) -> bool {
        self.deleted_at.is_some()
    }

    fn has_mark(&self, filter: &MarkFilter) -> bool {
        self.bookmarks
            .iter()
            .filter_map(MusicValue::stored_mark)
            .any(|m| filter.admits(&m))
    }
}

impl IndexedTrack {
    fn new(entity: &str) -> Self {
        Self {
            content_hash: entity.to_owned(),
            ..Default::default()
        }
    }

    fn to_track_info(&self) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(self.content_hash.clone()),
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            duration: self.duration_seconds.map(DurationSeconds::new),
            bpm: self.bpm.and_then(|b| Bpm::from_f32(b).ok()),
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: self.track_number,
            disc_number: self.disc_number,
            added: None,
            started: None,
            stopped: None,
            memory_cues: self.memory_cues.clone(),
            beat_grid: self.beat_grid.clone(),
            role: self.role,
            energy: self.energy,
        }
    }

    fn as_fields(&self) -> TrackFields<'_> {
        TrackFields {
            title: self.title.as_deref(),
            artist: self.artist.as_deref(),
            album: self.album.as_deref(),
            label: self.label.as_deref(),
            genre: self.genre.as_deref(),
            styles: &self.styles,
            bpm: self.bpm,
            key: self.key.as_deref(),
            duration: self.duration_seconds,
            year: self.year,
            source: self.source.as_deref(),
            last_started: None,
            last_stopped: None,
            added: None,
            role: self.role,
            energy: self.energy.map(u8::from),
        }
    }
}

/// In-memory library service (stub for BDD tests).
pub struct LibraryService {
    tracks: Mutex<Vec<IndexedTrack>>,
    fact_index: Mutex<HashMap<FactType, HashSet<String>>>,
    /// Playlist name -> content (list/get/new/replace; append, remove and
    /// rename are not supported).
    playlists: Mutex<HashMap<String, String>>,
    tracks_count: usize,
    facts_count: usize,
    start_time: Instant,
}

impl LibraryService {
    /// Create a new stub library service.
    ///
    /// Reads `facts.jsonl` from `metadata_dir` and builds an in-memory index.
    /// `_acid_socket` is accepted for API compatibility but ignored.
    ///
    /// Returns an `Arc`, like the real `library_service::LibraryService`, so a caller
    /// compiles unchanged against either.
    pub fn new(
        music_dir: PathBuf,
        metadata_dir: PathBuf,
        acid_socket: &str,
    ) -> Result<Arc<Self>, ServiceError> {
        Self::build(music_dir, metadata_dir, acid_socket).map(Arc::new)
    }

    /// Same as [`new`](Self::new); the events socket is accepted for API
    /// compatibility with the real service and ignored.
    pub fn new_with_events(
        music_dir: PathBuf,
        metadata_dir: PathBuf,
        acid_socket: &str,
        _acid_events_socket: &str,
    ) -> Result<Arc<Self>, ServiceError> {
        Self::new(music_dir, metadata_dir, acid_socket)
    }

    /// Number of indexed tracks (same accessor as the real service).
    pub fn tracks_count(&self) -> usize {
        self.tracks_count
    }

    /// Number of facts read at startup (same accessor as the real service).
    pub fn facts_count(&self) -> usize {
        self.facts_count
    }

    fn build(
        _music_dir: PathBuf,
        metadata_dir: PathBuf,
        _acid_socket: &str,
    ) -> Result<Self, ServiceError> {
        use music_facts::FactSource;
        use stainless_facts::FactStreamReader;

        let facts_path = metadata_dir.join("facts.jsonl");

        let mut tracks_map: HashMap<String, IndexedTrack> = HashMap::new();
        let mut fact_index: HashMap<FactType, HashSet<String>> = HashMap::new();
        let mut facts_count = 0usize;

        if facts_path.exists() {
            let reader: FactStreamReader<ContentHash, MusicValue, FactSource> =
                match FactStreamReader::open(&facts_path) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Stub: failed to open facts file: {:?}", e);
                        // Return empty service rather than error
                        return Ok(Self {
                            tracks: Mutex::new(vec![]),
                            fact_index: Mutex::new(HashMap::new()),
                            playlists: Mutex::new(HashMap::new()),
                            tracks_count: 0,
                            facts_count: 0,
                            start_time: Instant::now(),
                        });
                    }
                };

            for fact_result in reader {
                facts_count += 1;
                let fact = match fact_result {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("Stub: failed to parse fact: {:?}", e);
                        continue;
                    }
                };

                let entity = fact.entity().as_str().to_owned();
                let entry = tracks_map
                    .entry(entity.clone())
                    .or_insert_with(|| IndexedTrack::new(&entity));

                // Update fact index
                let variant_name = fact.value().display_name();
                let value_str = fact.value().to_string();
                fact_index
                    .entry(FactType::new(variant_name))
                    .or_default()
                    .insert(value_str);

                // Apply fact to the indexed track
                match fact.operation() {
                    stainless_facts::Operation::Assert => apply_fact(entry, fact.value()),
                    stainless_facts::Operation::Retract => retract_fact(entry, fact.value()),
                }
            }
        }

        let tracks_count = tracks_map.len();
        let tracks: Vec<IndexedTrack> = tracks_map.into_values().collect();

        Ok(Self {
            tracks: Mutex::new(tracks),
            fact_index: Mutex::new(fact_index),
            playlists: Mutex::new(HashMap::new()),
            tracks_count,
            facts_count,
            start_time: Instant::now(),
        })
    }

    /// Handle a single IPC request and return the response.
    pub fn handle_request(&self, request: LibraryRequest) -> LibraryResponse {
        match request {
            LibraryRequest::Ping => LibraryResponse::Pong,

            LibraryRequest::GetStatus => {
                let status = ServiceStatus {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    tracks_indexed: self.tracks_count,
                    facts_count: self.facts_count,
                    inbox_queue_size: 0,
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                };
                LibraryResponse::Status(status)
            }

            LibraryRequest::ListTracks { limit } => {
                LibraryResponse::Tracks(self.tracks_where(|_| true, limit))
            }

            LibraryRequest::ListBookmarked => {
                LibraryResponse::Tracks(self.tracks_where(|t| t.has_mark(&MarkFilter::Any), None))
            }

            LibraryRequest::ListMarked { filter } => {
                LibraryResponse::Tracks(self.tracks_where(|t| t.has_mark(&filter), None))
            }

            LibraryRequest::GetTrack { hash } => match self.get_track(&hash) {
                Ok(track) => LibraryResponse::Track(Box::new(track)),
                Err(e) => LibraryResponse::Error(e),
            },

            LibraryRequest::GetFacts { hash } => match self.resolve_hash(&hash) {
                Ok(full_hash) => LibraryResponse::Facts {
                    hash: full_hash,
                    facts: vec![],
                },
                Err(e) => LibraryResponse::Error(e),
            },

            LibraryRequest::Search { query } => {
                let results = self.search_tracks(&query);
                LibraryResponse::SearchResults(results)
            }

            LibraryRequest::GetFactValues { fact_type } => {
                let mut values: Vec<String> = self
                    .fact_index
                    .lock()
                    .unwrap()
                    .get(&fact_type)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                values.sort();
                LibraryResponse::FactValues(values)
            }

            LibraryRequest::HasFact { fact_type, value } => {
                let fact_index = self.fact_index.lock().unwrap();
                let exists = fact_index
                    .get(&fact_type)
                    .is_some_and(|values| values.contains(&value));
                LibraryResponse::FactExists {
                    fact_type,
                    value,
                    exists,
                }
            }

            LibraryRequest::HasFacts { fact_type, values } => {
                let fact_index = self.fact_index.lock().unwrap();
                let existing = match fact_index.get(&fact_type) {
                    Some(indexed) => values.into_iter().filter(|v| indexed.contains(v)).collect(),
                    None => vec![],
                };
                LibraryResponse::FactsExist {
                    fact_type,
                    existing,
                }
            }

            // Playlists are kept in memory. List, get, new and replace mirror the
            // real service (list is sorted; replace creates a missing playlist).
            LibraryRequest::PlaylistList => {
                let mut names: Vec<PlaylistName> = self
                    .playlists
                    .lock()
                    .unwrap()
                    .keys()
                    .filter_map(|n| PlaylistName::new(n).ok())
                    .collect();
                names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                LibraryResponse::PlaylistNames(names)
            }

            LibraryRequest::PlaylistGet { name } => {
                match self.playlists.lock().unwrap().get(name.as_str()) {
                    Some(content) => LibraryResponse::PlaylistContent(content.clone()),
                    None => LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                        name: name.to_string(),
                    }),
                }
            }

            LibraryRequest::PlaylistNew { name, content } => {
                let mut playlists = self.playlists.lock().unwrap();
                if playlists.contains_key(name.as_str()) {
                    LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists {
                        name: name.to_string(),
                    })
                } else {
                    playlists.insert(name.as_str().to_string(), content.clone());
                    LibraryResponse::PlaylistContent(content)
                }
            }

            LibraryRequest::PlaylistReplace { name, content } => {
                self.playlists
                    .lock()
                    .unwrap()
                    .insert(name.as_str().to_string(), content.clone());
                LibraryResponse::PlaylistContent(content)
            }

            LibraryRequest::PlaylistAppend { .. }
            | LibraryRequest::PlaylistRemove { .. }
            | LibraryRequest::PlaylistRename { .. } => {
                LibraryResponse::Error(ProtocolError::Internal {
                    message: "playlist operations not supported in stub".to_string(),
                })
            }

            // Stub no-ops for inbox/ingestion operations
            LibraryRequest::GetInboxQueue => LibraryResponse::InboxQueue(vec![]),

            LibraryRequest::IngestFile { .. }
            | LibraryRequest::DeleteInboxFile { .. }
            | LibraryRequest::IngestAll
            | LibraryRequest::IdentityBackfill { .. }
            | LibraryRequest::ReindexCovers => LibraryResponse::Error(ProtocolError::Internal {
                message: "ingestion not supported in stub".to_string(),
            }),

            // The stub never runs a backfill, so there is nothing to report or cancel.
            LibraryRequest::GetIdentityBackfillStatus | LibraryRequest::CancelIdentityBackfill => {
                LibraryResponse::IdentityBackfillStatus(
                    library_ipc_protocol::IdentityBackfillStatus::Idle,
                )
            }

            LibraryRequest::WriteBookmark { hash, scope } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let mut tracks = self.tracks.lock().unwrap();
                if let Some(t) = tracks
                    .iter_mut()
                    .find(|t| t.content_hash == full_hash.as_str())
                {
                    t.bookmarks.push(MusicValue::Bookmarked {
                        scope,
                        timestamp: chrono::Utc::now(),
                    });
                }
                LibraryResponse::BookmarkWritten
            }

            LibraryRequest::WriteFact { hash, fact } => {
                // Store the fact in the fact_index so has_facts probes work in tests.
                let variant_name = fact.display_name();
                let value_str = fact.to_string();
                self.fact_index
                    .lock()
                    .unwrap()
                    .entry(FactType::new(variant_name))
                    .or_default()
                    .insert(value_str);
                tracing::debug!(hash = %hash.as_str(), "Stub: WriteFact stored");
                LibraryResponse::FactWritten
            }

            LibraryRequest::RetractFact { .. } => LibraryResponse::FactRetracted,

            LibraryRequest::RetractSourceFacts { .. } => LibraryResponse::SourceFactsRetracted,

            LibraryRequest::GetAlbumTitleByItemId { .. } => {
                LibraryResponse::AlbumTitleByItemId(None)
            }

            LibraryRequest::GetTrackCountForItemId { .. } => {
                // Stub: always returns 0 (no real facts file scanning)
                LibraryResponse::TrackCountForItemId(0)
            }

            // Track lifecycle — stub implementations
            LibraryRequest::TrackDelete { hash } => {
                let mut tracks = self.tracks.lock().unwrap();
                match tracks.iter_mut().find(|t| t.content_hash == hash.as_str()) {
                    Some(t) => {
                        t.deleted_at = Some(chrono::Utc::now());
                        LibraryResponse::TrackDeleted
                    }
                    None => LibraryResponse::Error(ProtocolError::TrackNotFound {
                        hash: hash.as_str().to_owned(),
                    }),
                }
            }

            LibraryRequest::TrackRestore { hash } => {
                let mut tracks = self.tracks.lock().unwrap();
                match tracks.iter_mut().find(|t| t.content_hash == hash.as_str()) {
                    Some(t) => {
                        t.deleted_at = None;
                        LibraryResponse::TrackRestored
                    }
                    None => LibraryResponse::Error(ProtocolError::TrackNotFound {
                        hash: hash.as_str().to_owned(),
                    }),
                }
            }

            LibraryRequest::TrackReplace { .. } => {
                LibraryResponse::Error(ProtocolError::Internal {
                    message: "TrackReplace not supported in stub".to_string(),
                })
            }

            // The stub holds no identity facts, so a scan finds no duplicates (and
            // `apply` has nothing to do); merging is unsupported like replace.
            LibraryRequest::DedupScan { apply } => {
                LibraryResponse::DedupScan(library_ipc_protocol::DedupScanReport {
                    groups: vec![],
                    applied: apply.then(library_ipc_protocol::DedupApplyReport::default),
                })
            }

            LibraryRequest::DedupMerge { .. } => LibraryResponse::Error(ProtocolError::Internal {
                message: "DedupMerge not supported in stub".to_string(),
            }),

            // The stub archives nothing: the listing is empty and emptying is a no-op.
            LibraryRequest::ArchiveList => {
                LibraryResponse::ArchiveListing(library_ipc_protocol::ArchiveListing::default())
            }

            LibraryRequest::ArchiveEmpty { .. } => {
                LibraryResponse::ArchiveEmptied(library_ipc_protocol::ArchiveEmptyReport::default())
            }

            LibraryRequest::TrackOrphans => {
                let tracks = self.tracks.lock().unwrap();
                let orphans: Vec<OrphanInfo> = tracks
                    .iter()
                    .filter(|t| t.is_hidden())
                    .map(|t| {
                        let reason = if let Some(dt) = t.deleted_at {
                            OrphanReason::Deleted {
                                timestamp: dt.to_rfc3339(),
                            }
                        } else {
                            OrphanReason::Deleted {
                                timestamp: "unknown".to_string(),
                            }
                        };
                        OrphanInfo {
                            content_hash: ContentHash::new(t.content_hash.clone()),
                            artist: t.artist.clone(),
                            title: t.title.clone(),
                            reason,
                        }
                    })
                    .collect();
                LibraryResponse::OrphansList(orphans)
            }
        }
    }

    /// Visible tracks that match `filter` (hidden tracks excluded), optionally limited.
    fn tracks_where(
        &self,
        filter: impl Fn(&IndexedTrack) -> bool,
        limit: Option<usize>,
    ) -> Vec<TrackInfo> {
        let tracks = self.tracks.lock().unwrap();
        let iter = tracks
            .iter()
            .filter(|t| !t.is_hidden() && filter(t))
            .map(|t| t.to_track_info());
        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    /// Resolve a partial hash to a full hash (same algorithm as real service).
    fn resolve_hash(&self, partial: &ContentHash) -> Result<ContentHash, ProtocolError> {
        let normalize = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();
        let partial_clean = normalize(partial.as_str());

        let tracks = self.tracks.lock().unwrap();
        let matches: Vec<_> = tracks
            .iter()
            .filter(|t| {
                let hash = t
                    .content_hash
                    .as_str()
                    .strip_prefix("sha256:")
                    .unwrap_or(t.content_hash.as_str());
                hash.to_lowercase().starts_with(&partial_clean)
            })
            .collect();

        match matches.len() {
            0 => Err(ProtocolError::TrackNotFound {
                hash: partial.as_str().to_owned(),
            }),
            1 => Ok(ContentHash::new(matches[0].content_hash.clone())),
            _ => {
                // Check if all matches are the same content (prefix/full hash pair)
                let normalized: Vec<String> = matches
                    .iter()
                    .map(|t| normalize(t.content_hash.as_str()))
                    .collect();
                let all_same = normalized.iter().all(|a| {
                    normalized
                        .iter()
                        .all(|b| a.starts_with(b.as_str()) || b.starts_with(a.as_str()))
                });
                if all_same {
                    let best = matches.iter().max_by_key(|t| t.content_hash.len()).unwrap();
                    return Ok(ContentHash::new(best.content_hash.clone()));
                }

                // Genuinely ambiguous
                let n = matches.len();
                let examples: Vec<_> = matches
                    .iter()
                    .take(3)
                    .map(|t| {
                        let hash_str = t.content_hash.as_str();
                        let short = hash_str.get(7..15).unwrap_or(hash_str);
                        let name = t.title.as_deref().unwrap_or("Unknown");
                        format!("  {} ({})", short, name)
                    })
                    .collect();
                Err(ProtocolError::Internal {
                    message: format!(
                        "Ambiguous hash '{}' matches {} tracks:\n{}",
                        partial.as_str(),
                        n,
                        examples.join("\n")
                    ),
                })
            }
        }
    }

    /// Look up a track by content hash (supports partial hashes).
    /// Returns TrackNotFound for hidden (deleted/superseded) tracks.
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;

        let tracks = self.tracks.lock().unwrap();
        let track = tracks.iter().find(|t| t.content_hash == full_hash.as_str());

        match track {
            Some(t) if t.is_hidden() => Err(ProtocolError::TrackNotFound {
                hash: hash.as_str().to_owned(),
            }),
            Some(t) => Ok(t.to_track_info()),
            None => Err(ProtocolError::TrackNotFound {
                hash: hash.as_str().to_owned(),
            }),
        }
    }

    /// Search tracks against a `TrackQuery` (hidden tracks excluded).
    fn search_tracks(&self, query: &TrackQuery) -> Vec<TrackInfo> {
        self.tracks_where(|t| matches_query(query, &t.as_fields()), None)
    }
}

/// Apply a single `MusicValue` to an `IndexedTrack`.
fn apply_fact(entry: &mut IndexedTrack, value: &MusicValue) {
    match value {
        MusicValue::Title(v) => entry.title = Some(v.as_str().to_string()),
        MusicValue::Artist(v) => entry.artist = Some(v.as_str().to_string()),
        MusicValue::Album(v) => entry.album = Some(v.as_str().to_string()),
        MusicValue::Label(v) => entry.label = Some(v.clone()),
        MusicValue::MainGenre(v) => entry.genre = Some(v.clone()),
        MusicValue::StyleDescriptor(v) => entry.styles.push(v.clone()),
        MusicValue::DurationSeconds(v) => entry.duration_seconds = Some(v.value()),
        MusicValue::Bpm(v) => entry.bpm = Some(v.as_f32()),
        MusicValue::Year(v) => entry.year = Some(v.value()),
        MusicValue::TrackNumber(v) => entry.track_number = Some(v.value()),
        MusicValue::DiscNumber(v) => entry.disc_number = Some(v.value()),
        MusicValue::Source(v) => entry.source = Some(v.clone()),
        MusicValue::Replaces(_) => {
            // Replaces is asserted on the new track; no scalar field needed in stub.
        }
        MusicValue::Deleted { timestamp } => {
            entry.deleted_at = Some(*timestamp);
        }
        MusicValue::MemoryCue {
            position_ms,
            kind,
            label,
            index,
        } => {
            entry.memory_cues.push(CueInfo {
                position_ms: *position_ms,
                kind: kind.clone(),
                label: label.clone(),
                index: *index,
            });
        }
        MusicValue::BeatGrid {
            first_beat_ms,
            bpm,
            beats_per_bar,
        } => {
            entry.beat_grid = Bpm::from_f32(bpm.as_f32()).ok().map(|b| BeatGridInfo {
                first_beat_ms: *first_beat_ms,
                bpm: b,
                beats_per_bar: *beats_per_bar,
            });
        }
        MusicValue::Role(v) => entry.role = Some(*v),
        MusicValue::Energy(v) => entry.energy = Some(*v),
        MusicValue::Bookmarked { .. } => entry.bookmarks.push(value.clone()),
        _ => {} // ignore all other facts (key, format, cover art, timestamps, etc.)
    }
}

/// Apply a loaded retraction. Only bookmarks honour it: the first equal value is
/// removed. Every other retraction is skipped.
fn retract_fact(entry: &mut IndexedTrack, value: &MusicValue) {
    if let MusicValue::Bookmarked { .. } = value {
        if let Some(i) = entry.bookmarks.iter().position(|b| b == value) {
            entry.bookmarks.remove(i);
        }
    }
}

/// Run the IPC server loop, handling requests until the socket closes.
///
/// This is a free function in `pub mod service` so the harness can call
/// `library_service::service::run_ipc_server(...)` identically to the real crate.
pub fn run_ipc_server(svc: Arc<LibraryService>, address: &str) -> Result<(), ServiceError> {
    let server = IpcServer::bind(address)?;

    loop {
        match server.recv_raw() {
            Ok(msg) => {
                let response = match parse_request(&msg) {
                    Ok(request) => svc.handle_request(request),
                    Err(e) => {
                        tracing::warn!(error = %e, "Stub: rejected unparsable request");
                        LibraryResponse::Error(e)
                    }
                };
                if let Err(e) = server.send(&response) {
                    tracing::error!(error = %e, "Stub: failed to send response");
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "Stub: recv error, shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// The stub has no ACID stream to follow; present so callers of the real
/// `library_service::service::spawn_fact_subscriber` compile unchanged.
pub fn spawn_fact_subscriber(_service: Arc<LibraryService>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_service() -> Arc<LibraryService> {
        LibraryService::new(
            PathBuf::from("/nonexistent"),
            PathBuf::from("/nonexistent"),
            "ipc:///dev/null",
        )
        .expect("empty stub")
    }

    /// Send `payload` on `req` and decode the reply (2s timeout).
    fn exchange(req: &nng::Socket, payload: &[u8]) -> LibraryResponse {
        req.send(nng::Message::from(payload))
            .map_err(|(_, e)| e)
            .expect("send");
        let reply = req.recv().expect("a reply within the timeout");
        serde_json::from_slice(&reply).expect("reply is a LibraryResponse")
    }

    #[test]
    fn unparsable_request_gets_an_error_reply_and_the_server_keeps_serving() {
        use nng::options::{Options, RecvTimeout};
        let address = format!(
            "ipc://{}/stub-invalid-request-{}.sock",
            std::env::temp_dir().display(),
            std::process::id()
        );
        let svc = empty_service();
        let server_address = address.clone();
        let server = std::thread::spawn(move || run_ipc_server(svc, &server_address));

        let req = nng::Socket::new(nng::Protocol::Req0).expect("req socket");
        req.set_opt::<RecvTimeout>(Some(std::time::Duration::from_secs(2)))
            .expect("timeout");
        // The server thread binds asynchronously; retry the dial briefly.
        let mut dialed = false;
        for _ in 0..50 {
            if req.dial(&address).is_ok() {
                dialed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !dialed {
            if server.is_finished() {
                let outcome = server.join().expect("server thread panicked");
                panic!("stub server exited before it could be dialed: {outcome:?}");
            }
            panic!("could not dial the stub server");
        }

        match exchange(&req, b"not json") {
            LibraryResponse::Error(ProtocolError::InvalidRequest { .. }) => {}
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
        match exchange(&req, br#"{"type":"Ping"}"#) {
            LibraryResponse::Pong => {}
            other => panic!("expected Pong after garbage, got {other:?}"),
        }
        let _ = std::fs::remove_file(address.trim_start_matches("ipc://"));
    }

    fn name(s: &str) -> PlaylistName {
        PlaylistName::new(s).expect("valid name")
    }

    fn list(svc: &LibraryService) -> Vec<String> {
        match svc.handle_request(LibraryRequest::PlaylistList) {
            LibraryResponse::PlaylistNames(names) => {
                names.iter().map(|n| n.as_str().to_string()).collect()
            }
            other => panic!("expected PlaylistNames, got {other:?}"),
        }
    }

    fn service_with_tracks(hashes: &[&str]) -> Arc<LibraryService> {
        let svc = empty_service();
        svc.tracks
            .lock()
            .unwrap()
            .extend(hashes.iter().map(|h| IndexedTrack::new(h)));
        svc
    }

    /// The content hashes of a `Tracks` response, sorted.
    fn sorted_hashes(resp: LibraryResponse) -> Vec<String> {
        match resp {
            LibraryResponse::Tracks(tracks) => {
                let mut hashes: Vec<String> = tracks
                    .iter()
                    .map(|t| t.content_hash.as_str().to_owned())
                    .collect();
                hashes.sort();
                hashes
            }
            other => panic!("expected Tracks, got {other:?}"),
        }
    }

    fn bookmarked(svc: &LibraryService) -> Vec<String> {
        sorted_hashes(svc.handle_request(LibraryRequest::ListBookmarked))
    }

    #[test]
    fn write_bookmark_makes_list_bookmarked_return_the_track() {
        let svc = service_with_tracks(&["sha256:aaaa", "sha256:bbbb"]);
        assert!(bookmarked(&svc).is_empty());
        let resp = svc.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new("aaaa"),
            scope: Some("my-set".to_string()),
        });
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        assert_eq!(bookmarked(&svc), vec!["sha256:aaaa".to_string()]);
    }

    #[test]
    fn write_bookmark_rejects_an_unknown_track_like_the_real_service() {
        let svc = service_with_tracks(&["sha256:aaaa"]);
        let resp = svc.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new("ffff"),
            scope: None,
        });
        assert!(
            matches!(
                resp,
                LibraryResponse::Error(ProtocolError::TrackNotFound { .. })
            ),
            "{resp:?}"
        );
        assert!(bookmarked(&svc).is_empty());
    }

    fn marked(svc: &LibraryService, filter: MarkFilter) -> Vec<String> {
        sorted_hashes(svc.handle_request(LibraryRequest::ListMarked { filter }))
    }

    fn named_mark(s: &str) -> music_facts::Mark {
        music_facts::Mark::Named(music_facts::MarkName::parse(s).unwrap())
    }

    /// One track per mark kind: default, named (stored as "Banger"), legacy
    /// (empty scope), plus a hidden and a retracted mark that must never show.
    fn service_with_one_track_per_mark() -> Arc<LibraryService> {
        use stainless_facts::Operation::{Assert, Retract};
        let ts = chrono::DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        let mark = |scope: Option<&str>| MusicValue::Bookmarked {
            scope: scope.map(str::to_owned),
            timestamp: ts,
        };
        let svc = service_from_facts(
            "marks",
            &[
                ("sha256:markdefault01", mark(None), Assert),
                ("sha256:marknamed01", mark(Some("Banger")), Assert),
                ("sha256:marklegacy01", mark(Some("")), Assert),
                ("sha256:markhidden01", mark(None), Assert),
                ("sha256:markretracted01", mark(Some("banger")), Assert),
                ("sha256:markretracted01", mark(Some("banger")), Retract),
            ],
        );
        let resp = svc.handle_request(LibraryRequest::TrackDelete {
            hash: ContentHash::new("sha256:markhidden01"),
        });
        assert!(matches!(resp, LibraryResponse::TrackDeleted), "{resp:?}");
        svc
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn list_marked_any_lists_default_named_and_legacy_marks() {
        let svc = service_with_one_track_per_mark();
        assert_eq!(
            marked(&svc, MarkFilter::Any),
            strs(&[
                "sha256:markdefault01",
                "sha256:marklegacy01",
                "sha256:marknamed01"
            ])
        );
    }

    #[test]
    fn list_marked_only_default_excludes_named_and_legacy_marks() {
        let svc = service_with_one_track_per_mark();
        assert_eq!(
            marked(&svc, MarkFilter::Only(music_facts::Mark::Default)),
            strs(&["sha256:markdefault01"])
        );
    }

    #[test]
    fn list_marked_only_named_matches_a_differently_cased_stored_scope() {
        let svc = service_with_one_track_per_mark();
        assert_eq!(
            marked(&svc, MarkFilter::Only(named_mark("banger"))),
            strs(&["sha256:marknamed01"])
        );
    }

    #[test]
    fn list_marked_excludes_hidden_and_retracted_marks() {
        let svc = service_with_one_track_per_mark();
        let any = marked(&svc, MarkFilter::Any);
        assert!(!any.contains(&"sha256:markhidden01".to_owned()));
        assert!(!any.contains(&"sha256:markretracted01".to_owned()));
    }

    #[test]
    fn list_bookmarked_excludes_hidden_tracks() {
        let svc = service_with_tracks(&["sha256:aaaa"]);
        let resp = svc.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new("aaaa"),
            scope: None,
        });
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        svc.handle_request(LibraryRequest::TrackDelete {
            hash: ContentHash::new("sha256:aaaa"),
        });
        assert!(bookmarked(&svc).is_empty());
    }

    /// Build a stub from a facts.jsonl holding `facts` (`(hash, value, op)`),
    /// in a fresh temp dir that is removed afterwards.
    fn service_from_facts(
        test: &str,
        facts: &[(&str, MusicValue, stainless_facts::Operation)],
    ) -> Arc<LibraryService> {
        use music_facts::{FactOrigin, FactSource};
        use stainless_facts::{Fact, FactStreamWriter};

        use std::sync::atomic::{AtomicUsize, Ordering};
        // Unique per call: tests sharing a `test` name run in parallel.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mdma-stub-{test}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let ts = chrono::Utc::now();
        let batch: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
            .iter()
            .map(|(hash, v, op)| {
                Fact::new(ContentHash::new(*hash), v.clone(), ts, source.clone(), *op)
            })
            .collect();
        FactStreamWriter::open(dir.join("facts.jsonl"))
            .unwrap()
            .write_batch(&batch)
            .unwrap();
        let svc = LibraryService::new(
            PathBuf::from("/nonexistent"),
            dir.clone(),
            "ipc:///dev/null",
        )
        .expect("stub from facts");
        std::fs::remove_dir_all(&dir).unwrap();
        svc
    }

    fn a_bookmark() -> MusicValue {
        MusicValue::Bookmarked {
            scope: None,
            timestamp: chrono::DateTime::from_timestamp(1_767_225_600, 0).unwrap(),
        }
    }

    #[test]
    fn a_loaded_bookmark_fact_marks_the_track() {
        use stainless_facts::Operation;
        let svc = service_from_facts(
            "bookmark-assert",
            &[("sha256:aaaa", a_bookmark(), Operation::Assert)],
        );
        assert_eq!(bookmarked(&svc), vec!["sha256:aaaa".to_string()]);
    }

    #[test]
    fn a_loaded_bookmark_retract_unmarks_the_track() {
        use stainless_facts::Operation;
        let svc = service_from_facts(
            "bookmark-retract",
            &[
                (
                    "sha256:aaaa",
                    MusicValue::Title(music_facts::Title::new("Kept")),
                    Operation::Assert,
                ),
                ("sha256:aaaa", a_bookmark(), Operation::Assert),
                ("sha256:aaaa", a_bookmark(), Operation::Retract),
            ],
        );
        assert!(bookmarked(&svc).is_empty());
    }

    #[test]
    fn archive_is_empty_and_emptying_it_is_a_no_op() {
        let svc = empty_service();
        match svc.handle_request(LibraryRequest::ArchiveList) {
            LibraryResponse::ArchiveListing(l) => {
                assert!(l.entries.is_empty());
                assert_eq!(l.total_bytes, 0);
            }
            other => panic!("expected ArchiveListing, got {other:?}"),
        }
        match svc.handle_request(LibraryRequest::ArchiveEmpty { up_to: None }) {
            LibraryResponse::ArchiveEmptied(r) => {
                assert_eq!((r.files_deleted, r.bytes_freed), (0, 0));
                assert!(r.errors.is_empty());
            }
            other => panic!("expected ArchiveEmptied, got {other:?}"),
        }
    }

    #[test]
    fn playlist_list_is_empty_without_playlists() {
        assert!(list(&empty_service()).is_empty());
    }

    #[test]
    fn playlist_list_returns_stored_playlists_sorted() {
        let svc = empty_service();
        for n in ["zeta", "alpha"] {
            svc.handle_request(LibraryRequest::PlaylistNew {
                name: name(n),
                content: String::new(),
            });
        }
        assert_eq!(list(&svc), vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn playlist_replace_creates_a_missing_playlist_like_the_real_service() {
        let svc = empty_service();
        let resp = svc.handle_request(LibraryRequest::PlaylistReplace {
            name: name("fresh"),
            content: "x\n".to_string(),
        });
        assert!(matches!(resp, LibraryResponse::PlaylistContent(_)));
        assert_eq!(list(&svc), vec!["fresh".to_string()]);
    }
}
