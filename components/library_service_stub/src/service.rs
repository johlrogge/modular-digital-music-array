//! In-memory library service stub for BDD tests.
//!
//! Reads facts from a `facts.jsonl` file on construction and indexes them in
//! memory. Implements the same IPC handler loop as the real service but without
//! ACID, ingestion, or file-system side effects.

use crate::ipc::{
    Bpm, ContentHash, DurationSeconds, FactType, IpcServer, LibraryRequest, LibraryResponse,
    ProtocolError, ServiceStatus, TrackInfo, TrackQuery,
};
use library_search::{matches_query, TrackFields};
use music_facts::MusicValue;
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
        }
    }
}

/// In-memory library service (stub for BDD tests).
pub struct LibraryService {
    tracks: Mutex<Vec<IndexedTrack>>,
    fact_index: Mutex<HashMap<FactType, HashSet<String>>>,
    tracks_count: usize,
    facts_count: usize,
    start_time: Instant,
}

impl LibraryService {
    /// Create a new stub library service.
    ///
    /// Reads `facts.jsonl` from `metadata_dir` and builds an in-memory index.
    /// `_acid_socket` is accepted for API compatibility but ignored.
    pub fn new(
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
                apply_fact(entry, fact.value());
            }
        }

        let tracks_count = tracks_map.len();
        let tracks: Vec<IndexedTrack> = tracks_map.into_values().collect();

        Ok(Self {
            tracks: Mutex::new(tracks),
            fact_index: Mutex::new(fact_index),
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
                let tracks = self.tracks.lock().unwrap();
                let iter = tracks.iter().map(|t| t.to_track_info());
                let results: Vec<TrackInfo> = match limit {
                    Some(n) => iter.take(n).collect(),
                    None => iter.collect(),
                };
                LibraryResponse::Tracks(results)
            }

            LibraryRequest::GetTrack { hash } => match self.get_track(&hash) {
                Ok(track) => LibraryResponse::Track(track),
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

            // Stub no-ops for playlist operations
            LibraryRequest::PlaylistList => LibraryResponse::PlaylistNames(vec![]),

            LibraryRequest::PlaylistGet { name } => {
                LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                    name: name.to_string(),
                })
            }

            LibraryRequest::PlaylistNew { .. }
            | LibraryRequest::PlaylistAppend { .. }
            | LibraryRequest::PlaylistReplace { .. }
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
            | LibraryRequest::ReindexCovers => LibraryResponse::Error(ProtocolError::Internal {
                message: "ingestion not supported in stub".to_string(),
            }),

            LibraryRequest::WriteBookmark { .. } => LibraryResponse::BookmarkWritten,

            LibraryRequest::WriteFact { .. } => LibraryResponse::FactWritten,

            LibraryRequest::RetractSourceFacts { .. } => LibraryResponse::SourceFactsRetracted,

            LibraryRequest::GetAlbumTitleByItemId { .. } => {
                LibraryResponse::AlbumTitleByItemId(None)
            }
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
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;

        let tracks = self.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash == full_hash.as_str())
            .map(|t| t.to_track_info())
            .ok_or_else(|| ProtocolError::TrackNotFound {
                hash: hash.as_str().to_owned(),
            })
    }

    /// Search tracks against a `TrackQuery`.
    fn search_tracks(&self, query: &TrackQuery) -> Vec<TrackInfo> {
        let tracks = self.tracks.lock().unwrap();
        tracks
            .iter()
            .filter(|t| matches_query(query, &t.as_fields()))
            .map(|t| t.to_track_info())
            .collect()
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
        _ => {} // ignore all other facts (key, format, cover art, timestamps, etc.)
    }
}

/// Run the IPC server loop, handling requests until the socket closes.
///
/// This is a free function in `pub mod service` so the harness can call
/// `library_service::service::run_ipc_server(...)` identically to the real crate.
pub fn run_ipc_server(svc: Arc<LibraryService>, address: &str) -> Result<(), ServiceError> {
    let server = IpcServer::bind(address)?;

    loop {
        match server.recv() {
            Ok(request) => {
                let response = svc.handle_request(request);
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
