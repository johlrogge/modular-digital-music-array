//! Library service implementation
//!
//! Handles IPC requests and manages library state

use crate::fact_writer::FactWriter;
use crate::ipc::{IpcServer, LibraryRequest, LibraryResponse, ServiceStatus, TrackInfo};
use crate::pipeline::{InboxFile, UploadSource};
use music_facts::{ContentHash, MusicValue};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("IPC error: {0}")]
    Ipc(#[from] crate::ipc::IpcError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Ingest error: {0}")]
    Ingest(#[from] crate::pipeline::IngestError),

    #[error("Fact write error: {0}")]
    FactWrite(#[from] crate::fact_writer::FactWriteError),
}

/// Library service state
pub struct LibraryService {
    music_dir: PathBuf,
    metadata_dir: PathBuf,
    start_time: Instant,
    tracks_indexed: AtomicUsize,
    facts_count: AtomicUsize,
    /// In-memory index of tracks for fast search (rebuilt from facts on startup)
    tracks: Mutex<Vec<IndexedTrackInfo>>,
    /// In-memory cache of all facts by content hash (for get_facts queries)
    facts_cache: Mutex<HashMap<String, Vec<MusicValue>>>,
    /// Fact writer for persisting to disk
    fact_writer: Mutex<FactWriter>,
}

/// Track info stored in memory for search
#[derive(Clone)]
struct IndexedTrackInfo {
    content_hash: ContentHash,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    duration_seconds: Option<u32>,
    bpm: Option<f32>,
    key: Option<String>,
    blob_path: PathBuf,
}

/// Format a MusicValue for display (returns type name and string value)
fn format_fact_for_display(value: &MusicValue) -> (String, String) {
    (value.variant_name().to_string(), value.to_string())
}

impl LibraryService {
    /// Create a new library service
    pub fn new(music_dir: PathBuf, metadata_dir: PathBuf) -> Result<Self, ServiceError> {
        let facts_path = metadata_dir.join("facts.jsonl");

        // Load existing tracks and facts from file BEFORE opening writer
        // (FactWriter holds exclusive lock, FactStreamReader also needs lock)
        let (tracks, facts_cache) = Self::load_tracks_and_facts(&facts_path);
        let tracks_count = tracks.len();
        let facts_count = facts_cache.values().map(|v| v.len()).sum();

        // Now open writer for future writes
        let fact_writer = FactWriter::open(&facts_path)?;

        Ok(Self {
            music_dir,
            metadata_dir,
            start_time: Instant::now(),
            tracks_indexed: AtomicUsize::new(tracks_count),
            facts_count: AtomicUsize::new(facts_count),
            tracks: Mutex::new(tracks),
            facts_cache: Mutex::new(facts_cache),
            fact_writer: Mutex::new(fact_writer),
        })
    }

    /// Load tracks and all facts from facts file into memory
    fn load_tracks_and_facts(
        facts_path: &PathBuf,
    ) -> (Vec<IndexedTrackInfo>, HashMap<String, Vec<MusicValue>>) {
        use music_facts::FactSource;
        use stainless_facts::FactStreamReader;

        let reader: FactStreamReader<ContentHash, MusicValue, FactSource> =
            match FactStreamReader::open(facts_path) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Failed to open fact stream: {:?}", e);
                    return (vec![], HashMap::new());
                }
            };

        // Aggregate facts by content hash
        let mut tracks_map: HashMap<String, IndexedTrackInfo> = HashMap::new();
        let mut facts_cache: HashMap<String, Vec<MusicValue>> = HashMap::new();
        let mut errors = 0;
        let mut total = 0;

        for fact_result in reader {
            total += 1;
            let fact = match fact_result {
                Ok(f) => f,
                Err(e) => {
                    errors += 1;
                    if errors <= 3 {
                        tracing::warn!("Failed to parse fact: {:?}", e);
                    }
                    continue;
                }
            };

            let entity = fact.entity().0.clone();

            // Store all facts in cache
            facts_cache
                .entry(entity.clone())
                .or_default()
                .push(fact.value().clone());

            // Build track summary
            let entry = tracks_map
                .entry(entity.clone())
                .or_insert_with(|| IndexedTrackInfo {
                    content_hash: ContentHash(entity),
                    title: None,
                    artist: None,
                    album: None,
                    duration_seconds: None,
                    bpm: None,
                    key: None,
                    blob_path: PathBuf::new(),
                });

            // Extract key fields for search
            match fact.value() {
                MusicValue::Title(v) => entry.title = Some(v.clone()),
                MusicValue::Artist(v) => entry.artist = Some(v.clone()),
                MusicValue::Album(v) => entry.album = Some(v.clone()),
                MusicValue::DurationSeconds(v) => entry.duration_seconds = Some(v.0),
                MusicValue::Bpm(v) => entry.bpm = Some(v.as_f32()),
                MusicValue::Key(v) => entry.key = Some(v.to_string()),
                _ => {}
            }
        }

        tracing::info!("Processed {} facts from file, {} errors", total, errors);

        // Set blob paths based on hash
        for (hash, track) in tracks_map.iter_mut() {
            let hash_clean = hash.strip_prefix("sha256:").unwrap_or(hash);
            if hash_clean.len() >= 2 {
                track.blob_path =
                    PathBuf::from(format!("blobs/{}/{}.flac", &hash_clean[..2], hash_clean));
            }
        }

        (tracks_map.into_values().collect(), facts_cache)
    }

    /// Get number of indexed tracks
    pub fn tracks_count(&self) -> usize {
        self.tracks_indexed.load(Ordering::Relaxed)
    }

    /// Get number of facts
    pub fn facts_count(&self) -> usize {
        self.facts_count.load(Ordering::Relaxed)
    }

    /// Handle a single request
    pub fn handle_request(&self, request: LibraryRequest) -> LibraryResponse {
        tracing::debug!(?request, "Handling request");

        match request {
            LibraryRequest::Ping => LibraryResponse::Pong,

            LibraryRequest::GetStatus => {
                let status = ServiceStatus {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    tracks_indexed: self.tracks_indexed.load(Ordering::Relaxed),
                    facts_count: self.facts_count.load(Ordering::Relaxed),
                    inbox_queue_size: self.get_inbox_queue().len(),
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                };
                LibraryResponse::Status(status)
            }

            LibraryRequest::ListTracks { limit } => {
                // TODO: Read from fact stream and aggregate
                let tracks = self.list_tracks(limit);
                LibraryResponse::Tracks(tracks)
            }

            LibraryRequest::GetTrack { hash } => match self.get_track(&hash) {
                Ok(track) => LibraryResponse::Track(track),
                Err(msg) => LibraryResponse::Error { message: msg },
            },

            LibraryRequest::GetFacts { hash } => match self.get_facts(&hash) {
                Ok(facts) => {
                    let full_hash = self.resolve_hash(&hash).unwrap_or_default();
                    LibraryResponse::Facts {
                        hash: full_hash,
                        facts,
                    }
                }
                Err(msg) => LibraryResponse::Error { message: msg },
            },

            LibraryRequest::Search { query } => {
                let results = self.search_tracks(&query);
                LibraryResponse::SearchResults(results)
            }

            LibraryRequest::GetInboxQueue => {
                let queue = self.get_inbox_queue();
                LibraryResponse::InboxQueue(queue)
            }

            LibraryRequest::IngestFile { path } => {
                // TODO: Trigger ingestion pipeline
                match self.ingest_file(&path) {
                    Ok(hash) => LibraryResponse::IngestResult {
                        hash: Some(hash),
                        success: true,
                        message: "File ingested successfully".to_string(),
                    },
                    Err(e) => LibraryResponse::IngestResult {
                        hash: None,
                        success: false,
                        message: e.to_string(),
                    },
                }
            }
        }
    }

    /// Get files in inbox directory
    fn get_inbox_queue(&self) -> Vec<PathBuf> {
        let inbox_dir = self.music_dir.join("inbox");
        if !inbox_dir.exists() {
            return vec![];
        }

        std::fs::read_dir(&inbox_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List tracks from in-memory index
    fn list_tracks(&self, limit: Option<usize>) -> Vec<TrackInfo> {
        let tracks = self.tracks.lock().unwrap();

        let iter = tracks.iter().map(|t| TrackInfo {
            content_hash: t.content_hash.0.clone(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration_seconds: t.duration_seconds,
            bpm: t.bpm,
            key: t.key.clone(),
            blob_path: Some(self.music_dir.join(&t.blob_path)),
        });

        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    /// Resolve a partial hash to a full hash (like git short refs)
    /// Returns None if no match, Some(hash) if exactly one match, or error message if ambiguous
    fn resolve_hash(&self, partial: &str) -> Result<String, String> {
        let partial_clean = partial
            .strip_prefix("sha256:")
            .unwrap_or(partial)
            .to_lowercase();

        let tracks = self.tracks.lock().unwrap();
        let matches: Vec<_> = tracks
            .iter()
            .filter(|t| {
                let hash = t
                    .content_hash
                    .0
                    .strip_prefix("sha256:")
                    .unwrap_or(&t.content_hash.0);
                hash.to_lowercase().starts_with(&partial_clean)
            })
            .collect();

        match matches.len() {
            0 => Err(format!("No track found matching '{}'", partial)),
            1 => Ok(matches[0].content_hash.0.clone()),
            n => {
                let examples: Vec<_> = matches
                    .iter()
                    .take(3)
                    .map(|t| {
                        let short = &t.content_hash.0[7..15]; // sha256: prefix + 8 chars
                        let name = t.title.as_deref().unwrap_or("Unknown");
                        format!("  {} ({})", short, name)
                    })
                    .collect();
                Err(format!(
                    "Ambiguous hash '{}' matches {} tracks:\n{}",
                    partial,
                    n,
                    examples.join("\n")
                ))
            }
        }
    }

    /// Get track by hash from in-memory index (supports partial hashes)
    fn get_track(&self, hash: &str) -> Result<TrackInfo, String> {
        let full_hash = self.resolve_hash(hash)?;

        let tracks = self.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.0 == full_hash)
            .map(|t| TrackInfo {
                content_hash: t.content_hash.0.clone(),
                title: t.title.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                duration_seconds: t.duration_seconds,
                bpm: t.bpm,
                key: t.key.clone(),
                blob_path: Some(self.music_dir.join(&t.blob_path)),
            })
            .ok_or_else(|| "Track not found".to_string())
    }

    /// Get all facts for a track by hash (supports partial hashes)
    fn get_facts(&self, hash: &str) -> Result<Vec<(String, String)>, String> {
        let full_hash = self.resolve_hash(hash)?;

        let facts_cache = self.facts_cache.lock().unwrap();

        facts_cache
            .get(&full_hash)
            .map(|values| values.iter().map(|v| format_fact_for_display(v)).collect())
            .ok_or_else(|| format!("No facts found for {}", full_hash))
    }

    /// Search tracks by query (case-insensitive, searches title/artist/album)
    fn search_tracks(&self, query: &str) -> Vec<TrackInfo> {
        let query_lower = query.to_lowercase();
        let tracks = self.tracks.lock().unwrap();

        tracks
            .iter()
            .filter(|t| {
                let title_match = t
                    .title
                    .as_ref()
                    .map_or(false, |s| s.to_lowercase().contains(&query_lower));
                let artist_match = t
                    .artist
                    .as_ref()
                    .map_or(false, |s| s.to_lowercase().contains(&query_lower));
                let album_match = t
                    .album
                    .as_ref()
                    .map_or(false, |s| s.to_lowercase().contains(&query_lower));
                title_match || artist_match || album_match
            })
            .map(|t| TrackInfo {
                content_hash: t.content_hash.0.clone(),
                title: t.title.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                duration_seconds: t.duration_seconds,
                bpm: t.bpm,
                key: t.key.clone(),
                blob_path: Some(self.music_dir.join(&t.blob_path)),
            })
            .collect()
    }

    /// Ingest a file through the pipeline
    fn ingest_file(&self, path: &PathBuf) -> Result<String, ServiceError> {
        if !path.exists() {
            return Err(ServiceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File not found: {}", path.display()),
            )));
        }

        // Stage 1: Create inbox file
        let inbox = InboxFile::new(path.clone(), UploadSource::HttpUpload);

        // Stage 2: Validate and compute hash
        let validated = inbox.validate()?;
        let content_hash = validated.content_hash.clone();

        // Stage 3: Extract metadata
        let extracted = validated.extract_metadata()?;
        let facts = extracted.facts.clone();

        // Stage 4: Import to blob storage
        let indexed = extracted.import(&self.music_dir)?;

        let result = (content_hash, facts, indexed);

        let (content_hash, facts, indexed) = result;

        // Write facts to disk
        {
            let mut writer = self.fact_writer.lock().unwrap();
            writer.write_track_facts(&content_hash, &facts)?;
        }

        // Update in-memory index
        {
            let mut tracks = self.tracks.lock().unwrap();

            // Extract metadata from facts for the index
            let mut title = None;
            let mut artist = None;
            let mut album = None;
            let mut duration_seconds = None;
            let mut bpm = None;
            let mut key = None;

            for (value, _source) in &facts {
                match value {
                    MusicValue::Title(t) => title = Some(t.clone()),
                    MusicValue::Artist(a) => artist = Some(a.clone()),
                    MusicValue::Album(a) => album = Some(a.clone()),
                    MusicValue::DurationSeconds(d) => duration_seconds = Some(d.0),
                    MusicValue::Bpm(b) => bpm = Some(b.as_f32()),
                    MusicValue::Key(k) => key = Some(k.to_string()),
                    _ => {}
                }
            }

            tracks.push(IndexedTrackInfo {
                content_hash: content_hash.clone(),
                title,
                artist,
                album,
                duration_seconds,
                bpm,
                key,
                blob_path: indexed.blob_path,
            });
        }

        // Update counters
        self.tracks_indexed.fetch_add(1, Ordering::Relaxed);
        self.facts_count.fetch_add(facts.len(), Ordering::Relaxed);

        Ok(content_hash.0)
    }
}

/// Run the IPC server loop
pub fn run_ipc_server(service: Arc<LibraryService>, address: &str) -> Result<(), ServiceError> {
    let server = IpcServer::bind(address)?;

    tracing::info!("IPC server running, waiting for requests...");

    loop {
        match server.recv() {
            Ok(request) => {
                let response = service.handle_request(request);
                if let Err(e) = server.send(&response) {
                    tracing::error!(error = %e, "Failed to send response, sending error fallback");
                    let fallback = LibraryResponse::Error {
                        message: format!("Internal error: {}", e),
                    };
                    if let Err(e2) = server.send(&fallback) {
                        tracing::error!(error = %e2, "Failed to send fallback error response");
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to receive request");
            }
        }
    }
}
