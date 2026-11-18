//! In-memory library service stub for BDD tests.
//!
//! Reads facts from a `facts.jsonl` file on construction and indexes them in
//! memory. Implements the same IPC handler loop as the real service but without
//! ACID, ingestion, or file-system side effects.

use crate::ipc::{
    bookmark_scope_mark, parse_request, BinId, ContentHash, FactType, IpcServer, LibraryRequest,
    LibraryResponse, Mark, MarkWrite, OrphanInfo, OrphanReason, PlaylistName, PlaylistVersion,
    ProtocolError, ServiceStatus, TrackInfo, TrackQuery, Unmark,
};
use library_projection::{
    apply_fact, distinct_retractions, fact_values, marked, project, resolve_prefix,
    resolve_visible, retain_live, search, track_info, unresolved_error, visible, AlbumCovers,
    IndexedTrackInfo, LineageLog,
};
use music_facts::{FactSource, MusicValue};
use stainless_facts::{Fact, Operation};
use std::collections::hash_map::{Entry, OccupiedEntry};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
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

/// A version string was empty.
#[derive(Debug, Error)]
#[error("a release version must not be empty")]
pub struct EmptyReleaseVersion;

/// The release version of the binary hosting the service (ADR-020); same type as the
/// real `library_service::ReleaseVersion`, so a caller compiles unchanged against either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion(String);

impl ReleaseVersion {
    pub fn new(version: &str) -> Result<Self, EmptyReleaseVersion> {
        if version.trim().is_empty() {
            return Err(EmptyReleaseVersion);
        }
        Ok(Self(version.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What `playlists_lock` guards.
#[derive(Default)]
struct Playlists {
    /// Playlist name -> content (everything but append).
    live: HashMap<String, String>,
    /// The playlist bin (ADR-033): id -> the stored content, as the real service
    /// keeps `playlists/.bin/<id>.plist`. Read by neither list nor search.
    bin: Vec<(BinId, String)>,
}

/// In-memory library service (stub for BDD tests).
///
/// It holds the projection's own track index and answers through the projection's
/// read side, so it cannot disagree with the real service about what a fact means
/// or how a list is ordered (ADR-030 §1).
pub struct LibraryService {
    tracks: Mutex<Vec<IndexedTrackInfo>>,
    /// The plays and covers of the lineage, recorded as every fact is projected and
    /// applied to the tracks before every catalog read, as the real service does.
    ///
    /// Lock order: `tracks` -> `lineage`.
    lineage: Mutex<LineageLog>,
    fact_index: Mutex<HashMap<FactType, HashSet<String>>>,
    /// The playlists and the bin. This mutex is the stub's `playlists_lock`: held
    /// by every playlist reader and writer, the bin's included, so the bin needs
    /// no lock order of its own.
    ///
    /// Lock order: `playlists` -> `tracks`, as in the real service.
    playlists: Mutex<Playlists>,
    /// Every `WriteFact` received, with the source it stated, in order.
    written: Mutex<Vec<(ContentHash, MusicValue, Option<FactSource>)>>,
    tracks_count: usize,
    facts_count: usize,
    /// Release version the host binary passed at construction; reported by `GetStatus`.
    version: ReleaseVersion,
    start_time: Instant,
}

impl LibraryService {
    /// The names of all playlists, sorted (as the real service lists them).
    fn playlist_names(&self) -> Vec<PlaylistName> {
        let mut names: Vec<PlaylistName> = self
            .playlists
            .lock()
            .unwrap()
            .live
            .keys()
            .filter_map(|n| PlaylistName::new(n).ok())
            .collect();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        names
    }

    /// The source the last `WriteFact` of `fact` for `hash` stated: `None` when no
    /// such write was received, `Some(None)` when it stated no source.
    pub fn written_source(
        &self,
        hash: &ContentHash,
        fact: &MusicValue,
    ) -> Option<Option<FactSource>> {
        self.written
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(h, f, _)| h == hash && f == fact)
            .map(|(_, _, source)| source.clone())
    }

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
        version: ReleaseVersion,
    ) -> Result<Arc<Self>, ServiceError> {
        Self::build(music_dir, metadata_dir, acid_socket, version).map(Arc::new)
    }

    /// Same as [`new`](Self::new); the events socket is accepted for API
    /// compatibility with the real service and ignored.
    pub fn new_with_events(
        music_dir: PathBuf,
        metadata_dir: PathBuf,
        acid_socket: &str,
        _acid_events_socket: &str,
        version: ReleaseVersion,
    ) -> Result<Arc<Self>, ServiceError> {
        Self::new(music_dir, metadata_dir, acid_socket, version)
    }

    /// A stub for tests that need only the facts in `metadata_dir`: no music dir, no
    /// ACID, a fixed `0.0.0-stub` release version.
    pub fn from_metadata_dir(metadata_dir: PathBuf) -> Arc<Self> {
        Self::new(
            PathBuf::from("/nonexistent"),
            metadata_dir,
            "ipc:///dev/null",
            ReleaseVersion::new("0.0.0-stub").expect("non-empty"),
        )
        .expect("stub from metadata dir")
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
        version: ReleaseVersion,
    ) -> Result<Self, ServiceError> {
        use stainless_facts::FactStreamReader;

        let facts_path = metadata_dir.join("facts.jsonl");

        let mut facts = Vec::new();
        let mut facts_count = 0usize;

        if facts_path.exists() {
            // An unreadable file gives an empty service rather than an error.
            let reader: Vec<_> =
                match FactStreamReader::<ContentHash, MusicValue, FactSource>::open(&facts_path) {
                    Ok(reader) => reader.collect(),
                    Err(e) => {
                        tracing::warn!("Stub: failed to open facts file: {:?}", e);
                        Vec::new()
                    }
                };
            for fact_result in reader {
                facts_count += 1;
                match fact_result {
                    Ok(fact) => facts.push(fact),
                    Err(e) => tracing::warn!("Stub: failed to parse fact: {:?}", e),
                }
            }
        }

        // The real service's startup fold, then its plays.
        let mut projection = project(facts);
        projection.lineage.apply_to(&mut projection.tracks);

        Ok(Self {
            tracks_count: projection.tracks.len(),
            tracks: Mutex::new(projection.tracks),
            lineage: Mutex::new(projection.lineage),
            fact_index: Mutex::new(projection.fact_index),
            playlists: Mutex::new(Playlists::default()),
            written: Mutex::new(Vec::new()),
            facts_count,
            version,
            start_time: Instant::now(),
        })
    }

    /// Handle a single IPC request and return the response.
    pub fn handle_request(&self, request: LibraryRequest) -> LibraryResponse {
        match request {
            LibraryRequest::Ping => LibraryResponse::Pong,

            LibraryRequest::GetStatus => {
                let status = ServiceStatus {
                    version: self.version.as_str().to_string(),
                    tracks_indexed: self.tracks_count,
                    facts_count: self.facts_count,
                    inbox_queue_size: 0,
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                    unparseable_lines: 0,
                };
                LibraryResponse::Status(status)
            }

            LibraryRequest::ListTracks { limit } => LibraryResponse::Tracks(
                self.with_catalog(|tracks, covers| visible(tracks, covers, limit)),
            ),

            LibraryRequest::ListMarked { filter } => LibraryResponse::Tracks(
                self.with_catalog(|tracks, covers| marked(tracks, covers, &filter)),
            ),

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
                let values = fact_values(&self.fact_index.lock().unwrap(), &fact_type);
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
            LibraryRequest::PlaylistList => LibraryResponse::PlaylistNames(self.playlist_names()),

            LibraryRequest::PlaylistSearch { query } => {
                let matcher = library_search::StringMatcher::new(&query);
                let mut names = self.playlist_names();
                names.retain(|n| matcher.matches(n.as_str()));
                LibraryResponse::PlaylistNames(names)
            }

            LibraryRequest::PlaylistGet { name } => self.with_playlist(&name, |entry, _| {
                LibraryResponse::PlaylistContent(entry.get().clone())
            }),

            LibraryRequest::PlaylistEntries { name } => {
                self.with_playlist(&name, |entry, tracks| {
                    LibraryResponse::PlaylistEntries(playlist_edit::entries(entry.get(), tracks))
                })
            }

            LibraryRequest::PlaylistSnapshot { name } => {
                self.with_playlist(&name, |entry, tracks| {
                    LibraryResponse::PlaylistSnapshot(playlist_edit::snapshot(entry.get(), tracks))
                })
            }

            LibraryRequest::PlaylistEdit { name, version, ops } => {
                self.with_playlist(&name, |entry, tracks| {
                    let covers = AlbumCovers::build(tracks);
                    match playlist_edit::plan_edit(entry.get(), &version, &ops, tracks, |t| {
                        track_info(t, &covers)
                    }) {
                        Ok(planned) => commit(entry, planned),
                        Err(e) => LibraryResponse::Error(e),
                    }
                })
            }

            LibraryRequest::PlaylistRevert {
                name,
                version,
                content: to,
            } => self.with_playlist(&name, |entry, tracks| {
                let planned = playlist_edit::plan_revert(entry.get(), &version, &to, tracks);
                commit(entry, planned)
            }),

            // Delete bins too (ADR-033 §3), and answers as it always did.
            LibraryRequest::PlaylistDelete { name, version } => {
                self.bin_at_version(&name, &version, |_| LibraryResponse::PlaylistDeleted)
            }

            LibraryRequest::PlaylistBin { name, version } => {
                self.bin_at_version(&name, &version, |id| LibraryResponse::PlaylistBinned { id })
            }

            LibraryRequest::PlaylistRestore { id, as_name } => {
                let mut guard = self.playlists.lock().unwrap();
                let playlists = &mut *guard;
                let Some(at) = playlists.bin.iter().position(|(held, _)| *held == id) else {
                    return bin_entry_not_found(&id);
                };
                let target = playlist_edit::restore_target(&id, as_name);
                if playlists.live.contains_key(target.as_str()) {
                    return LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists {
                        name: target.to_string(),
                    });
                }
                let (_, content) = playlists.bin.remove(at);
                playlists.live.insert(target.as_str().to_string(), content);
                LibraryResponse::PlaylistRestored { name: target }
            }

            LibraryRequest::BinList => {
                let mut entries: Vec<_> = self
                    .playlists
                    .lock()
                    .unwrap()
                    .bin
                    .iter()
                    .map(|(id, content)| playlist_edit::bin_entry(id.clone(), content))
                    .collect();
                playlist_edit::sort_newest_first(&mut entries);
                LibraryResponse::BinEntries(entries)
            }

            LibraryRequest::BinPurge { id } => {
                let mut playlists = self.playlists.lock().unwrap();
                match playlists.bin.iter().position(|(held, _)| *held == id) {
                    Some(at) => {
                        playlists.bin.remove(at);
                        LibraryResponse::Pong
                    }
                    None => bin_entry_not_found(&id),
                }
            }

            LibraryRequest::BinEmpty => {
                self.playlists.lock().unwrap().bin.clear();
                LibraryResponse::Pong
            }

            LibraryRequest::PlaylistRemove { name } => {
                self.with_playlist_and_bin(&name, |entry, bin, _| {
                    match bin_playlist(bin, entry, &name) {
                        Ok(_) => LibraryResponse::Pong,
                        Err(e) => LibraryResponse::Error(e),
                    }
                })
            }

            LibraryRequest::PlaylistAppend { name, content } => {
                self.with_playlist(&name, |mut entry, _| {
                    let new_content = playlist_edit::appended(entry.get(), &content);
                    *entry.get_mut() = new_content.clone();
                    LibraryResponse::PlaylistContent(new_content)
                })
            }

            LibraryRequest::PlaylistRename { from, to } => {
                let mut playlists = self.playlists.lock().unwrap();
                let live = &mut playlists.live;
                if !live.contains_key(from.as_str()) {
                    return not_found(&from);
                }
                if live.contains_key(to.as_str()) {
                    return LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists {
                        name: to.to_string(),
                    });
                }
                if let Some(content) = live.remove(from.as_str()) {
                    live.insert(to.as_str().to_string(), content);
                }
                LibraryResponse::Pong
            }

            LibraryRequest::PlaylistNew { name, content } => {
                let mut playlists = self.playlists.lock().unwrap();
                if playlists.live.contains_key(name.as_str()) {
                    LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists {
                        name: name.to_string(),
                    })
                } else {
                    playlists
                        .live
                        .insert(name.as_str().to_string(), content.clone());
                    LibraryResponse::PlaylistContent(content)
                }
            }

            LibraryRequest::PlaylistReplace { name, content } => {
                self.playlists
                    .lock()
                    .unwrap()
                    .live
                    .insert(name.as_str().to_string(), content.clone());
                LibraryResponse::PlaylistContent(content)
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

            // Server-side alias of WriteMark for old clients. They can't decode new
            // error variants, so a bad scope is an `Internal` error.
            LibraryRequest::WriteBookmark { hash, scope } => {
                match bookmark_scope_mark(scope.as_deref())
                    .and_then(|mark| self.write_mark(&hash, &mark))
                {
                    Ok(_) => LibraryResponse::BookmarkWritten,
                    Err(e) => LibraryResponse::Error(e),
                }
            }

            LibraryRequest::WriteMark { hash, mark } => match self.write_mark(&hash, &mark) {
                Ok(outcome) => LibraryResponse::MarkWritten(outcome),
                Err(e) => LibraryResponse::Error(e),
            },

            LibraryRequest::UnmarkTrack { hash, mark } => match self.unmark(&hash, &mark) {
                Ok(outcome) => LibraryResponse::TrackUnmarked(outcome),
                Err(e) => LibraryResponse::Error(e),
            },

            LibraryRequest::WriteFact { hash, fact, source } => {
                // Like the real service, a write names a track that exists.
                let entity = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                self.written
                    .lock()
                    .unwrap()
                    .push((hash, fact.clone(), source.clone()));
                let source = source.unwrap_or_else(|| self.user_source());
                self.project(&entity, fact, source, Operation::Assert);
                tracing::debug!(hash = %entity.as_str(), "Stub: WriteFact stored");
                LibraryResponse::FactWritten
            }

            LibraryRequest::RetractFact { hash, fact, source } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let source = source.unwrap_or_else(|| self.user_source());
                let live = self
                    .tracks
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|t| t.content_hash == full_hash && t.is_live_from(&fact, &source));
                if !live {
                    return LibraryResponse::NothingToRetract;
                }
                self.project(&full_hash, fact, source, Operation::Retract);
                LibraryResponse::FactRetracted
            }

            LibraryRequest::RetractSourceFacts { .. } => LibraryResponse::SourceFactsRetracted,

            LibraryRequest::GetAlbumTitleByItemId { .. } => {
                LibraryResponse::AlbumTitleByItemId(None)
            }

            LibraryRequest::GetTrackCountForItemId { .. } => {
                // Stub: always returns 0 (no real facts file scanning)
                LibraryResponse::TrackCountForItemId(0)
            }

            // Track lifecycle — stub implementations
            // Like the real service: assert `Deleted`, and retract the current one.
            LibraryRequest::TrackDelete { hash } => match self.resolve_hash(&hash) {
                Ok(full_hash) => {
                    let fact = MusicValue::Deleted {
                        timestamp: chrono::Utc::now(),
                    };
                    self.project(&full_hash, fact, self.user_source(), Operation::Assert);
                    LibraryResponse::TrackDeleted
                }
                Err(e) => LibraryResponse::Error(e),
            },

            LibraryRequest::TrackRestore { hash } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let deleted_at = self
                    .tracks
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|t| t.content_hash == full_hash)
                    .and_then(|t| t.deleted_at);
                let Some(timestamp) = deleted_at else {
                    return LibraryResponse::Error(ProtocolError::Internal {
                        message: "Track is not deleted".to_string(),
                    });
                };
                let fact = MusicValue::Deleted { timestamp };
                self.project(&full_hash, fact, self.user_source(), Operation::Retract);
                LibraryResponse::TrackRestored
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
                    unfinished: vec![],
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
                            content_hash: t.content_hash.clone(),
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

    /// What the stub's own writes state as their source when they state none: the
    /// user, like the real service's `user_default`.
    fn user_source(&self) -> FactSource {
        FactSource::new("mdma", self.version.as_str(), "User")
    }

    /// Run a read over the track index and its album covers, which the projection's
    /// read side needs (rebuilt per read: the stub is small). The lineage is applied
    /// first, as the node does before every such read (ADR-030 §1.3), so a `Replaces`
    /// or a cover written after startup takes effect.
    fn with_catalog<R>(&self, read: impl FnOnce(&[IndexedTrackInfo], &AlbumCovers) -> R) -> R {
        let mut tracks = self.tracks.lock().unwrap();
        self.lineage.lock().unwrap().apply_to(&mut tracks);
        read(&tracks, &AlbumCovers::build(&tracks))
    }

    /// Project a write: build the fact and apply it with the projection's own
    /// `apply_fact`, as the real service projects what it wrote.
    fn project(
        &self,
        entity: &ContentHash,
        value: MusicValue,
        source: FactSource,
        operation: Operation,
    ) {
        let fact = Fact::new(entity.clone(), value, chrono::Utc::now(), source, operation);
        let mut tracks = self.tracks.lock().unwrap();
        self.lineage.lock().unwrap().record(&fact);
        let mut positions: HashMap<String, usize> = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.content_hash.as_str().to_owned(), i))
            .collect();
        apply_fact(
            &mut tracks,
            &mut positions,
            &mut HashSet::new(),
            &mut self.fact_index.lock().unwrap(),
            &fact,
        );
        // The real service's sweep after a catch-up: what no assertion holds is gone.
        retain_live(&mut tracks, &mut HashSet::new());
    }

    /// Apply `mark` to a track unless the track already carries it live; then
    /// nothing is written and the original time is kept.
    fn write_mark(&self, hash: &ContentHash, mark: &Mark) -> Result<MarkWrite, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;
        let already_live = self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.content_hash == full_hash)
            .is_some_and(|t| t.live_marks(mark).next().is_some());
        if already_live {
            return Ok(MarkWrite::AlreadyLive);
        }
        self.project(
            &full_hash,
            mark.fact_value(chrono::Utc::now()),
            self.user_source(),
            Operation::Assert,
        );
        Ok(MarkWrite::Written)
    }

    /// Retract every live assertion of `mark` on a track: one retract per (value,
    /// source identity), like the real service. `count` is the number of retracts.
    fn unmark(&self, hash: &ContentHash, mark: &Mark) -> Result<Unmark, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;
        let live = {
            let tracks = self.tracks.lock().unwrap();
            let track = tracks
                .iter()
                .find(|t| t.content_hash == full_hash)
                .ok_or_else(|| ProtocolError::TrackNotFound {
                    hash: hash.as_str().to_owned(),
                })?;
            distinct_retractions(track.live_marks(mark).cloned())
        };
        let Some(count) = NonZeroUsize::new(live.len()) else {
            return Ok(Unmark::NotLive);
        };
        for (value, source) in live {
            self.project(&full_hash, value, source, Operation::Retract);
        }
        Ok(Unmark::Retracted { count })
    }

    /// Resolve a partial hash to a full hash (the real service's `resolve_prefix`).
    fn resolve_hash(&self, partial: &ContentHash) -> Result<ContentHash, ProtocolError> {
        let tracks = self.tracks.lock().unwrap();
        resolve_prefix(&tracks, partial.as_str())
            .map(|track| track.content_hash.clone())
            .map_err(|unresolved| unresolved_error(partial.as_str(), unresolved))
    }

    /// Look up a track by content hash (supports partial hashes).
    /// Returns TrackNotFound for hidden (deleted/superseded) tracks.
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ProtocolError> {
        self.with_catalog(|tracks, covers| {
            resolve_visible(tracks, hash.as_str())
                .map(|track| track_info(track, covers))
                .map_err(|unresolved| unresolved_error(hash.as_str(), unresolved))
        })
    }

    /// Search tracks against a `TrackQuery` (hidden tracks excluded).
    fn search_tracks(&self, query: &TrackQuery) -> Vec<TrackInfo> {
        self.with_catalog(|tracks, covers| search(tracks, covers, query))
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

fn not_found(name: &PlaylistName) -> LibraryResponse {
    LibraryResponse::Error(ProtocolError::PlaylistNotFound {
        name: name.to_string(),
    })
}

fn bin_entry_not_found(id: &BinId) -> LibraryResponse {
    LibraryResponse::Error(ProtocolError::BinEntryNotFound { id: id.to_string() })
}

/// Store what `planned` says to write, and answer its outcome.
fn commit(
    entry: OccupiedEntry<'_, String, String>,
    planned: playlist_edit::Planned,
) -> LibraryResponse {
    let (write, outcome) = planned.into_parts();
    if let Some(bytes) = write {
        *entry.into_mut() = bytes;
    }
    LibraryResponse::PlaylistEdited(outcome)
}

/// Move the playlist of `entry` into `bin` and answer its id.
fn bin_playlist(
    bin: &mut Vec<(BinId, String)>,
    entry: OccupiedEntry<'_, String, String>,
    name: &PlaylistName,
) -> Result<BinId, ProtocolError> {
    let id = playlist_edit::fresh_bin_id(name, std::time::SystemTime::now(), |id| {
        bin.iter().any(|(held, _)| held == id)
    })
    .map_err(playlist_edit::bin_id_error)?;
    bin.push((id.clone(), entry.remove()));
    Ok(id)
}

impl LibraryService {
    /// Move playlist `name` to the bin if it is still at `version` and answer
    /// `done` with its id; a stale `version` answers `PlaylistDeleteConflict`.
    fn bin_at_version(
        &self,
        name: &PlaylistName,
        version: &PlaylistVersion,
        done: impl FnOnce(BinId) -> LibraryResponse,
    ) -> LibraryResponse {
        self.with_playlist_and_bin(name, |entry, bin, tracks| match playlist_edit::plan_delete(
            entry.get(),
            version,
            tracks,
        ) {
            Ok(()) => match bin_playlist(bin, entry, name) {
                Ok(id) => done(id),
                Err(e) => LibraryResponse::Error(e),
            },
            Err(current) => LibraryResponse::PlaylistDeleteConflict { current },
        })
    }

    /// Run `f` on the entry of playlist `name` (read, change or remove it) and on
    /// the tracks, under the playlists lock and then the tracks lock (the lock
    /// order). A missing playlist is `PlaylistNotFound`.
    fn with_playlist(
        &self,
        name: &PlaylistName,
        f: impl FnOnce(OccupiedEntry<'_, String, String>, &[IndexedTrackInfo]) -> LibraryResponse,
    ) -> LibraryResponse {
        self.with_playlist_and_bin(name, |entry, _, tracks| f(entry, tracks))
    }

    /// `with_playlist`, for `f` that also moves playlists in or out of the bin.
    fn with_playlist_and_bin(
        &self,
        name: &PlaylistName,
        f: impl FnOnce(
            OccupiedEntry<'_, String, String>,
            &mut Vec<(BinId, String)>,
            &[IndexedTrackInfo],
        ) -> LibraryResponse,
    ) -> LibraryResponse {
        let mut guard = self.playlists.lock().unwrap();
        let playlists = &mut *guard;
        let Entry::Occupied(entry) = playlists.live.entry(name.as_str().to_string()) else {
            return not_found(name);
        };
        let tracks = self.tracks.lock().unwrap();
        f(entry, &mut playlists.bin, &tracks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{CanonicalHash, EntryError, MarkFilter, PlaylistEntry};

    fn empty_service() -> Arc<LibraryService> {
        LibraryService::from_metadata_dir(PathBuf::from("/nonexistent"))
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

    fn search_playlists(svc: &LibraryService, text: &str) -> Vec<String> {
        let query = library_search::parse_string_query(text);
        match svc.handle_request(LibraryRequest::PlaylistSearch { query }) {
            LibraryResponse::PlaylistNames(names) => {
                names.iter().map(|n| n.as_str().to_string()).collect()
            }
            other => panic!("expected PlaylistNames, got {other:?}"),
        }
    }

    #[test]
    fn playlist_search_uses_the_library_text_rules() {
        let svc = empty_service();
        for n in ["Carbon-Based-Lifeforms", "warmup"] {
            svc.handle_request(LibraryRequest::PlaylistNew {
                name: name(n),
                content: String::new(),
            });
        }
        // Upper case is an initialism; lower case is a plain "contains".
        assert_eq!(search_playlists(&svc, "CBL"), ["Carbon-Based-Lifeforms"]);
        assert!(search_playlists(&svc, "cbl").is_empty());
        assert_eq!(search_playlists(&svc, "warm"), ["warmup"]);
    }

    fn service_with_tracks(hashes: &[&str]) -> Arc<LibraryService> {
        let svc = empty_service();
        svc.tracks.lock().unwrap().extend(
            hashes
                .iter()
                .map(|h| IndexedTrackInfo::new_empty((*h).to_owned())),
        );
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

    fn marked_any(svc: &LibraryService) -> Vec<String> {
        marked(svc, MarkFilter::Any)
    }

    #[test]
    fn a_prefix_of_two_tracks_is_an_ambiguous_hash_not_an_internal_error() {
        let a = format!("sha256:ab{}", "0".repeat(62));
        let b = format!("sha256:ab{}", "1".repeat(62));
        let svc = service_with_tracks(&[a.as_str(), b.as_str()]);
        let partial = ContentHash::new("sha256:ab");
        for result in [
            svc.resolve_hash(&partial).map(|_| ()),
            svc.get_track(&partial).map(|_| ()),
        ] {
            match result {
                Err(ProtocolError::AmbiguousHash { hash, candidates }) => {
                    assert_eq!(hash, "sha256:ab");
                    assert_eq!(candidates.len(), 2);
                    assert!(candidates.contains(&a) && candidates.contains(&b));
                }
                other => panic!("expected AmbiguousHash, got {other:?}"),
            }
        }
    }

    #[test]
    fn search_by_key_finds_projected_track() {
        let svc = service_with_tracks(&["sha256:aaaa", "sha256:bbbb", "sha256:cccc"]);
        for (i, key) in ["A Minor", "C Major", "E Minor"].iter().enumerate() {
            let mut tracks = svc.tracks.lock().unwrap();
            library_projection::apply_fact_to_track(
                &mut tracks[i],
                &MusicValue::Key(key.parse().expect("valid key")),
                chrono::Utc::now(),
                Operation::Assert,
                &FactSource::new("test", "1.0.0", "Unknown"),
            );
        }
        let search = |key: &str| {
            let query = TrackQuery {
                key: Some(key.parse().expect("valid key query")),
                ..Default::default()
            };
            match svc.handle_request(LibraryRequest::Search { query }) {
                LibraryResponse::SearchResults(tracks) => {
                    let mut hashes: Vec<String> = tracks
                        .iter()
                        .map(|t| t.content_hash.as_str().to_owned())
                        .collect();
                    hashes.sort();
                    hashes
                }
                other => panic!("expected SearchResults, got {other:?}"),
            }
        };
        assert_eq!(search("8A"), vec!["sha256:aaaa"]);
        // 8B is a mode switch away (one step); 9A is one step clockwise.
        assert_eq!(search("8A-1"), vec!["sha256:aaaa", "sha256:bbbb"]);
        assert_eq!(
            search("8A+-1"),
            vec!["sha256:aaaa", "sha256:bbbb", "sha256:cccc"]
        );
        assert!(search("1A").is_empty());
    }

    #[test]
    fn every_track_list_is_in_catalog_order() {
        use stainless_facts::Operation::Assert;
        let artist = |a: &str| MusicValue::Artist(music_facts::Artist::new(a));
        let svc = service_from_facts(
            "catalog-order",
            &[
                ("sha256:c", artist("B"), Assert),
                ("sha256:b", artist("a"), Assert),
                ("sha256:a", artist("a"), Assert),
                (
                    "sha256:d",
                    MusicValue::ItemId("untagged".to_owned()),
                    Assert,
                ),
            ],
        );
        let in_order = |resp: LibraryResponse| match resp {
            LibraryResponse::Tracks(tracks) | LibraryResponse::SearchResults(tracks) => tracks
                .iter()
                .map(|t| t.content_hash.as_str().to_owned())
                .collect::<Vec<_>>(),
            other => panic!("expected tracks, got {other:?}"),
        };
        let expected = strs(&["sha256:a", "sha256:b", "sha256:c", "sha256:d"]);
        assert_eq!(
            in_order(svc.handle_request(LibraryRequest::ListTracks { limit: None })),
            expected,
            "artist ignoring case, then hash; a missing artist last"
        );
        assert_eq!(
            in_order(svc.handle_request(LibraryRequest::Search {
                query: TrackQuery::default()
            })),
            expected
        );
    }

    fn listed(svc: &LibraryService) -> Vec<String> {
        sorted_hashes(svc.handle_request(LibraryRequest::ListTracks { limit: None }))
    }

    #[test]
    fn write_fact_to_an_unknown_hash_is_track_not_found_and_makes_no_entity() {
        let svc = service_with_mark_track(&[]);
        let resp = svc.handle_request(LibraryRequest::WriteFact {
            hash: ContentHash::new("sha256:nobody"),
            fact: MusicValue::ItemId("p1".to_string()),
            source: None,
        });
        assert_track_not_found(resp);
        assert_eq!(listed(&svc), strs(&[MARK_TRACK]));
        assert!(svc
            .written_source(
                &ContentHash::new("sha256:nobody"),
                &MusicValue::ItemId("p1".to_string())
            )
            .is_none());
    }

    #[test]
    fn a_retract_that_empties_an_entity_drops_it_from_the_list() {
        let svc = service_with_mark_track(&[]);
        let resp = svc.handle_request(LibraryRequest::RetractFact {
            hash: ContentHash::new(MARK_TRACK),
            fact: MusicValue::Title(music_facts::Title::new("T")),
            source: Some(FactSource::new("test", "1.0.0", "Unknown")),
        });
        assert!(matches!(resp, LibraryResponse::FactRetracted), "{resp:?}");
        assert!(listed(&svc).is_empty());
    }

    #[test]
    fn delete_hides_a_track_and_restore_brings_it_back() {
        let svc = service_with_mark_track(&[]);
        let hash = ContentHash::new(MARK_TRACK);
        let resp = svc.handle_request(LibraryRequest::TrackDelete { hash: hash.clone() });
        assert!(matches!(resp, LibraryResponse::TrackDeleted), "{resp:?}");
        assert!(listed(&svc).is_empty());

        let resp = svc.handle_request(LibraryRequest::TrackRestore { hash: hash.clone() });
        assert!(matches!(resp, LibraryResponse::TrackRestored), "{resp:?}");
        assert_eq!(listed(&svc), strs(&[MARK_TRACK]));

        let resp = svc.handle_request(LibraryRequest::TrackRestore { hash });
        assert!(
            matches!(resp, LibraryResponse::Error(ProtocolError::Internal { .. })),
            "{resp:?}"
        );
    }

    #[test]
    fn write_bookmark_makes_list_marked_return_the_track() {
        let svc = service_with_tracks(&["sha256:aaaa", "sha256:bbbb"]);
        assert!(marked_any(&svc).is_empty());
        let resp = svc.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new("aaaa"),
            scope: Some("my-set".to_string()),
        });
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        assert_eq!(marked_any(&svc), vec!["sha256:aaaa".to_string()]);
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
        assert!(marked_any(&svc).is_empty());
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

    const MARK_TRACK: &str = "sha256:writemark01";

    /// A stub holding one track, `MARK_TRACK`, plus `extra` marks loaded as facts.
    fn service_with_mark_track(extra: &[MusicValue]) -> Arc<LibraryService> {
        use stainless_facts::Operation::Assert;
        let mut facts = vec![(
            MARK_TRACK,
            MusicValue::Title(music_facts::Title::new("T")),
            Assert,
        )];
        facts.extend(extra.iter().map(|v| (MARK_TRACK, v.clone(), Assert)));
        service_from_facts("write-mark", &facts)
    }

    const TRACK_A: &str = "sha256:marktrackA01";
    const TRACK_B: &str = "sha256:marktrackB01";

    /// A stub holding two titled tracks, `TRACK_A` and `TRACK_B`.
    fn service_with_two_tracks() -> Arc<LibraryService> {
        use stainless_facts::Operation::Assert;
        let title = |h| (h, MusicValue::Title(music_facts::Title::new("T")), Assert);
        service_from_facts("two-tracks", &[title(TRACK_A), title(TRACK_B)])
    }

    /// A `Bookmarked` value with a fixed timestamp, as a loaded fact would carry.
    fn stored_bookmark(scope: &str, secs: i64) -> MusicValue {
        MusicValue::Bookmarked {
            scope: Some(scope.to_owned()),
            timestamp: chrono::DateTime::from_timestamp(secs, 0).unwrap(),
        }
    }

    fn assert_track_not_found(resp: LibraryResponse) {
        assert!(
            matches!(
                resp,
                LibraryResponse::Error(ProtocolError::TrackNotFound { .. })
            ),
            "{resp:?}"
        );
    }

    fn write_mark(svc: &LibraryService, hash: &str, mark: &Mark) -> LibraryResponse {
        svc.handle_request(LibraryRequest::WriteMark {
            hash: ContentHash::new(hash),
            mark: mark.clone(),
        })
    }

    fn assert_mark_written(resp: LibraryResponse, expected: MarkWrite) {
        match resp {
            LibraryResponse::MarkWritten(outcome) => assert_eq!(outcome, expected),
            other => panic!("expected MarkWritten({expected:?}), got {other:?}"),
        }
    }

    /// The live bookmark values of `MARK_TRACK`.
    fn live_bookmarks(svc: &LibraryService) -> Vec<MusicValue> {
        let tracks = svc.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == MARK_TRACK)
            .expect("the mark track")
            .provenance
            .iter()
            .filter(|(v, _)| matches!(v, MusicValue::Bookmarked { .. }))
            .map(|(v, _)| v.clone())
            .collect()
    }

    #[test]
    fn write_mark_lists_the_track_under_that_mark() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, "writemark01", &banger), MarkWrite::Written);
        assert_eq!(marked(&svc, MarkFilter::Only(banger)), strs(&[MARK_TRACK]));
        assert!(marked(&svc, MarkFilter::Only(Mark::Default)).is_empty());
    }

    #[test]
    fn write_mark_is_per_track() {
        let svc = service_with_two_tracks();
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, TRACK_A, &banger), MarkWrite::Written);
        assert_mark_written(write_mark(&svc, TRACK_B, &banger), MarkWrite::Written);
    }

    #[test]
    fn write_mark_twice_is_already_live_and_changes_nothing() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        let before = live_bookmarks(&svc);
        assert_eq!(before.len(), 1);

        assert_mark_written(
            write_mark(&svc, MARK_TRACK, &banger),
            MarkWrite::AlreadyLive,
        );
        assert_eq!(live_bookmarks(&svc), before);
    }

    #[test]
    fn write_mark_is_already_live_for_a_differently_cased_stored_mark() {
        let svc = service_with_mark_track(&[stored_bookmark("Banger", 1_767_225_600)]);
        let before = live_bookmarks(&svc);
        assert_eq!(before.len(), 1);

        assert_mark_written(
            write_mark(&svc, MARK_TRACK, &named_mark("banger")),
            MarkWrite::AlreadyLive,
        );
        assert_eq!(live_bookmarks(&svc), before);
    }

    #[test]
    fn retract_that_removes_nothing_answers_nothing_to_retract() {
        let svc = service_with_mark_track(&[]);
        let resp = svc.handle_request(LibraryRequest::RetractFact {
            hash: ContentHash::new(MARK_TRACK),
            fact: MusicValue::Source("nothing".to_string()),
            source: None,
        });
        assert!(
            matches!(resp, LibraryResponse::NothingToRetract),
            "{resp:?}"
        );
    }

    #[test]
    fn write_mark_writes_again_after_the_mark_was_retracted() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        let fact = live_bookmarks(&svc).remove(0);
        let resp = svc.handle_request(LibraryRequest::RetractFact {
            hash: ContentHash::new(MARK_TRACK),
            fact,
            source: None,
        });
        assert!(matches!(resp, LibraryResponse::FactRetracted), "{resp:?}");
        assert!(live_bookmarks(&svc).is_empty());

        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        assert_eq!(live_bookmarks(&svc).len(), 1);
    }

    #[test]
    fn two_marks_coexist_on_one_track() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        assert_mark_written(
            write_mark(&svc, MARK_TRACK, &named_mark("opener")),
            MarkWrite::Written,
        );
        assert_mark_written(
            write_mark(&svc, MARK_TRACK, &Mark::Default),
            MarkWrite::Written,
        );
        assert_eq!(live_bookmarks(&svc).len(), 3);
        for mark in [banger, named_mark("opener"), music_facts::Mark::Default] {
            assert_eq!(marked(&svc, MarkFilter::Only(mark)), strs(&[MARK_TRACK]));
        }
    }

    #[test]
    fn write_mark_rejects_an_unknown_track() {
        let svc = service_with_mark_track(&[]);
        assert_track_not_found(write_mark(&svc, "ffff", &music_facts::Mark::Default));
        assert!(live_bookmarks(&svc).is_empty());
    }

    fn unmark(svc: &LibraryService, hash: &str, mark: &Mark) -> LibraryResponse {
        svc.handle_request(LibraryRequest::UnmarkTrack {
            hash: ContentHash::new(hash),
            mark: mark.clone(),
        })
    }

    fn assert_unmarked(resp: LibraryResponse, expected: Unmark) {
        match resp {
            LibraryResponse::TrackUnmarked(outcome) => assert_eq!(outcome, expected),
            other => panic!("expected TrackUnmarked({expected:?}), got {other:?}"),
        }
    }

    fn retracted(count: usize) -> Unmark {
        Unmark::Retracted {
            count: std::num::NonZeroUsize::new(count).unwrap(),
        }
    }

    #[test]
    fn unmark_removes_the_track_from_that_mark() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        assert_unmarked(unmark(&svc, MARK_TRACK, &banger), retracted(1));
        assert!(marked(&svc, MarkFilter::Only(banger)).is_empty());
        assert!(live_bookmarks(&svc).is_empty());
    }

    #[test]
    fn unmark_keeps_the_other_marks() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        for mark in [&banger, &named_mark("opener"), &Mark::Default] {
            assert_mark_written(write_mark(&svc, MARK_TRACK, mark), MarkWrite::Written);
        }
        assert_unmarked(unmark(&svc, MARK_TRACK, &banger), retracted(1));
        assert_eq!(live_bookmarks(&svc).len(), 2);
        assert_unmarked(unmark(&svc, MARK_TRACK, &Mark::Default), retracted(1));
        assert_eq!(
            marked(&svc, MarkFilter::Only(named_mark("opener"))),
            strs(&[MARK_TRACK])
        );
        assert_eq!(live_bookmarks(&svc).len(), 1);
    }

    #[test]
    fn unmark_twice_is_not_live() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        assert_unmarked(unmark(&svc, MARK_TRACK, &banger), retracted(1));
        assert_unmarked(unmark(&svc, MARK_TRACK, &banger), Unmark::NotLive);
    }

    #[test]
    fn unmark_removes_a_differently_cased_stored_mark() {
        let svc = service_with_mark_track(&[stored_bookmark("Banger", 1_767_225_600)]);
        assert_unmarked(
            unmark(&svc, MARK_TRACK, &named_mark("banger")),
            retracted(1),
        );
        assert!(live_bookmarks(&svc).is_empty());
    }

    #[test]
    fn unmark_removes_every_live_entry_of_the_mark() {
        // Two loaded assertions stand in for a merge carry: one retract per value.
        let svc = service_with_mark_track(&[
            stored_bookmark("banger", 1_767_225_600),
            stored_bookmark("banger", 1_767_312_000),
        ]);
        assert_eq!(live_bookmarks(&svc).len(), 2);
        assert_unmarked(
            unmark(&svc, MARK_TRACK, &named_mark("banger")),
            retracted(2),
        );
        assert!(live_bookmarks(&svc).is_empty());
    }

    #[test]
    fn unmark_is_per_track() {
        let svc = service_with_two_tracks();
        let banger = named_mark("banger");
        for hash in [TRACK_A, TRACK_B] {
            assert_mark_written(write_mark(&svc, hash, &banger), MarkWrite::Written);
        }
        assert_unmarked(unmark(&svc, TRACK_A, &banger), retracted(1));
        assert_eq!(marked(&svc, MarkFilter::Only(banger)), strs(&[TRACK_B]));
    }

    #[test]
    fn unmark_rejects_an_unknown_track() {
        let svc = service_with_mark_track(&[]);
        assert_track_not_found(unmark(&svc, "ffff", &Mark::Default));
    }

    #[test]
    fn write_mark_writes_again_after_unmark() {
        let svc = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        assert_unmarked(unmark(&svc, MARK_TRACK, &banger), retracted(1));
        assert_mark_written(write_mark(&svc, MARK_TRACK, &banger), MarkWrite::Written);
        assert_eq!(live_bookmarks(&svc).len(), 1);
    }

    fn write_bookmark(svc: &LibraryService, scope: Option<&str>) -> LibraryResponse {
        svc.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new(MARK_TRACK),
            scope: scope.map(str::to_owned),
        })
    }

    #[test]
    fn write_bookmark_with_a_scope_stores_the_normalised_mark() {
        let svc = service_with_mark_track(&[]);
        let resp = write_bookmark(&svc, Some("Friday"));
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        let stored: Vec<_> = live_bookmarks(&svc)
            .into_iter()
            .filter_map(|v| match v {
                MusicValue::Bookmarked { scope, .. } => Some(scope),
                _ => None,
            })
            .collect();
        assert_eq!(stored, vec![Some("friday".to_owned())]);
    }

    #[test]
    fn write_bookmark_without_a_scope_stores_the_default_mark() {
        let svc = service_with_mark_track(&[]);
        let resp = write_bookmark(&svc, None);
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        assert_eq!(
            marked(&svc, MarkFilter::Only(music_facts::Mark::Default)),
            strs(&[MARK_TRACK])
        );
    }

    #[test]
    fn write_bookmark_twice_still_answers_bookmark_written() {
        let svc = service_with_mark_track(&[]);
        let first = write_bookmark(&svc, Some("friday"));
        assert!(
            matches!(first, LibraryResponse::BookmarkWritten),
            "{first:?}"
        );
        let resp = write_bookmark(&svc, Some("friday"));
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        assert_eq!(live_bookmarks(&svc).len(), 1);
    }

    #[test]
    fn write_bookmark_with_an_empty_scope_is_an_internal_error() {
        let svc = service_with_mark_track(&[]);
        match write_bookmark(&svc, Some("")) {
            LibraryResponse::Error(ProtocolError::Internal { message }) => {
                assert!(message.contains("empty"), "{message}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
        assert!(live_bookmarks(&svc).is_empty());
    }

    #[test]
    fn list_marked_excludes_hidden_tracks_marked_via_write_bookmark() {
        let svc = service_with_tracks(&["sha256:aaaa"]);
        let resp = svc.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new("aaaa"),
            scope: None,
        });
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        svc.handle_request(LibraryRequest::TrackDelete {
            hash: ContentHash::new("sha256:aaaa"),
        });
        assert!(marked_any(&svc).is_empty());
    }

    /// Build a stub from a facts.jsonl holding `facts` (`(hash, value, op)`),
    /// in a fresh temp dir that is removed afterwards.
    fn service_from_facts(
        test: &str,
        facts: &[(&str, MusicValue, stainless_facts::Operation)],
    ) -> Arc<LibraryService> {
        use stainless_facts::{Fact, FactStreamWriter};

        use std::sync::atomic::{AtomicUsize, Ordering};
        // Unique per call: tests sharing a `test` name run in parallel.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mdma-stub-{test}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = FactSource::new("test", "1.0.0", "Unknown");
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
        let svc = LibraryService::from_metadata_dir(dir.clone());
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
        assert_eq!(marked_any(&svc), vec!["sha256:aaaa".to_string()]);
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
        assert!(marked_any(&svc).is_empty());
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

    // The PlaylistEntries contract, as the real service's tests state it.

    fn full_hash(prefix: &str, fill: char) -> String {
        format!(
            "sha256:{prefix}{}",
            fill.to_string().repeat(64 - prefix.len())
        )
    }

    fn track_entry(hash: &str) -> PlaylistEntry {
        PlaylistEntry::Track {
            hash: CanonicalHash::try_from(ContentHash::new(hash)).unwrap(),
        }
    }

    /// A stub holding tracks `aaaa..` and two sharing the prefix `ab`, with the
    /// playlist `set` holding `content`.
    fn service_with_playlist(content: &str) -> Arc<LibraryService> {
        let svc = service_with_tracks(&[
            &full_hash("aaaa", 'a'),
            &full_hash("ab", '1'),
            &full_hash("ab", '2'),
        ]);
        svc.handle_request(LibraryRequest::PlaylistNew {
            name: name("set"),
            content: content.to_string(),
        });
        svc
    }

    fn playlist_entries(svc: &LibraryService, playlist: &str) -> LibraryResponse {
        svc.handle_request(LibraryRequest::PlaylistEntries {
            name: name(playlist),
        })
    }

    fn entries_of(resp: LibraryResponse) -> Vec<PlaylistEntry> {
        match resp {
            LibraryResponse::PlaylistEntries(entries) => entries,
            other => panic!("expected PlaylistEntries, got {other:?}"),
        }
    }

    #[test]
    fn playlist_entries_keep_order_and_duplicates_of_mixed_short_and_full_lines() {
        let a = full_hash("aaaa", 'a');
        let b1 = full_hash("ab", '1');
        let content =
            format!("# a set\naaaaaaaa  Artist - Title  [3:00]\n\n{b1}\nsha256:aaaa\n{a}\n");
        let svc = service_with_playlist(&content);
        assert_eq!(
            entries_of(playlist_entries(&svc, "set")),
            vec![
                track_entry(&a),
                track_entry(&b1),
                track_entry(&a),
                track_entry(&a),
            ]
        );
    }

    #[test]
    fn playlist_entries_report_unresolved_entries_in_place() {
        let a = full_hash("aaaa", 'a');
        let svc = service_with_playlist("ffffffff\naaaaaaaa\nab\n");
        assert_eq!(
            entries_of(playlist_entries(&svc, "set")),
            vec![
                PlaylistEntry::Unresolved {
                    error: EntryError::Unknown {
                        hash: ContentHash::new("ffffffff")
                    }
                },
                track_entry(&a),
                PlaylistEntry::Unresolved {
                    error: EntryError::Ambiguous {
                        hash: ContentHash::new("ab"),
                        candidates: vec![
                            ContentHash::new(full_hash("ab", '1')),
                            ContentHash::new(full_hash("ab", '2')),
                        ],
                    }
                },
            ]
        );
    }

    #[test]
    fn playlist_entries_leave_the_file_byte_identical() {
        let content =
            "# a set\r\naaaaaaaa  Artist - Title\n\nffffffff\n  ab\t x\nno newline at end";
        let svc = service_with_playlist(content);
        let _ = playlist_entries(&svc, "set");
        let resp = svc.handle_request(LibraryRequest::PlaylistGet { name: name("set") });
        assert!(matches!(resp, LibraryResponse::PlaylistContent(c) if c == content));
    }

    #[test]
    fn playlist_entries_of_missing_playlist_is_playlist_not_found() {
        let svc = service_with_playlist("");
        let resp = playlist_entries(&svc, "nope");
        assert!(
            matches!(
                resp,
                LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. })
            ),
            "{resp:?}"
        );
    }

    // The ADR-027 contract, stated once in `playlist_edit::contract`.

    fn contract_setup(content: &str) -> playlist_edit::contract::Handler {
        let svc = service_with_playlist(content);
        Box::new(move |request| svc.handle_request(request))
    }

    playlist_edit::playlist_contract_tests!(contract_setup);

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
