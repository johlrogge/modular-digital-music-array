//! Library service implementation
//!
//! Handles IPC requests and manages library state

use crate::fact_writer::FactWriter;
use crate::ipc::{
    Bpm, ContentHash, DurationSeconds, InboxPath, IngestAllItem, IngestResult, IngestSource,
    IpcServer, Key, LibraryRequest, LibraryResponse, ProtocolError, ServiceStatus, TrackInfo,
    TrackQuery,
};
use crate::pipeline::{InboxFile, UploadSource};
use library_search::{matches_query, TrackFields};
use music_facts::MusicValue;
use std::collections::{HashMap, HashSet};
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
    /// Fact writer for persisting to disk
    fact_writer: Mutex<FactWriter>,
    /// Generic fact value index: fact_type -> set of values
    /// Used for fast HasFact/HasFacts lookups (e.g., ItemId -> {"p123", "p456"})
    fact_index: Mutex<HashMap<String, HashSet<String>>>,
    /// Set of known content hashes for dedup on ingest
    content_hashes: Mutex<HashSet<String>>,
}

/// Track info stored in memory for search
#[derive(Clone)]
struct IndexedTrackInfo {
    content_hash: ContentHash,
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
    blob_path: PathBuf,
    last_started: Option<chrono::DateTime<chrono::Utc>>,
    last_stopped: Option<chrono::DateTime<chrono::Utc>>,
}

/// Result of loading tracks from the fact stream
struct LoadResult {
    tracks: Vec<IndexedTrackInfo>,
    facts_count: usize,
    fact_index: HashMap<String, HashSet<String>>,
    content_hashes: HashSet<String>,
}

fn update_if_more_recent(
    slot: &mut Option<chrono::DateTime<chrono::Utc>>,
    ts: chrono::DateTime<chrono::Utc>,
) {
    if slot.is_none_or(|existing| ts > existing) {
        *slot = Some(ts);
    }
}

/// Format a MusicValue for display (returns type name and string value)
fn format_fact_for_display(value: &MusicValue) -> (String, String) {
    (value.display_name().to_string(), value.to_string())
}

impl LibraryService {
    /// Create a new library service
    pub fn new(music_dir: PathBuf, metadata_dir: PathBuf) -> Result<Self, ServiceError> {
        let facts_path = metadata_dir.join("facts.jsonl");

        // Load existing tracks from facts file
        let loaded = Self::load_tracks_from_facts(&facts_path);
        let tracks_count = loaded.tracks.len();

        // Open writer for future writes
        let fact_writer = FactWriter::open(&facts_path)?;

        Ok(Self {
            music_dir,
            metadata_dir,
            start_time: Instant::now(),
            tracks_indexed: AtomicUsize::new(tracks_count),
            facts_count: AtomicUsize::new(loaded.facts_count),
            tracks: Mutex::new(loaded.tracks),
            fact_writer: Mutex::new(fact_writer),
            fact_index: Mutex::new(loaded.fact_index),
            content_hashes: Mutex::new(loaded.content_hashes),
        })
    }

    /// Load tracks from facts file into memory for search
    fn load_tracks_from_facts(facts_path: &PathBuf) -> LoadResult {
        use music_facts::FactSource;
        use stainless_facts::FactStreamReader;

        let reader: FactStreamReader<ContentHash, MusicValue, FactSource> =
            match FactStreamReader::open(facts_path) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Failed to open fact stream: {:?}", e);
                    return LoadResult {
                        tracks: vec![],
                        facts_count: 0,
                        fact_index: HashMap::new(),
                        content_hashes: HashSet::new(),
                    };
                }
            };

        // Aggregate facts by content hash
        let mut tracks_map: HashMap<String, IndexedTrackInfo> = HashMap::new();
        let mut fact_index: HashMap<String, HashSet<String>> = HashMap::new();
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

            // Build track summary
            let entry = tracks_map
                .entry(entity.clone())
                .or_insert_with(|| IndexedTrackInfo {
                    content_hash: ContentHash(entity),
                    title: None,
                    artist: None,
                    album: None,
                    label: None,
                    genre: None,
                    styles: vec![],
                    duration_seconds: None,
                    bpm: None,
                    key: None,
                    year: None,
                    source: None,
                    blob_path: PathBuf::new(),
                    last_started: None,
                    last_stopped: None,
                });

            // Index fact values for HasFact/HasFacts lookups
            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            fact_index
                .entry(variant_name.to_string())
                .or_default()
                .insert(value_str);

            // Extract key fields for search
            match fact.value() {
                MusicValue::Title(v) => entry.title = Some(v.0.clone()),
                MusicValue::Artist(v) => entry.artist = Some(v.0.clone()),
                MusicValue::Album(v) => entry.album = Some(v.0.clone()),
                MusicValue::Label(v) => entry.label = Some(v.clone()),
                MusicValue::MainGenre(v) => entry.genre = Some(v.clone()),
                MusicValue::StyleDescriptor(v) => entry.styles.push(v.clone()),
                MusicValue::DurationSeconds(v) => entry.duration_seconds = Some(v.0),
                MusicValue::Bpm(v) => entry.bpm = Some(v.as_f32()),
                MusicValue::Key(v) => entry.key = Some(v.to_string()),
                MusicValue::Year(v) => entry.year = Some(v.0),
                MusicValue::Source(v) => entry.source = Some(v.clone()),
                MusicValue::TrackStarted(ts) => update_if_more_recent(&mut entry.last_started, *ts),
                MusicValue::TrackStopped(ts) => update_if_more_recent(&mut entry.last_stopped, *ts),
                _ => {}
            }
        }

        tracing::info!("Processed {} facts from file, {} errors", total, errors);

        // Collect content hashes for dedup
        let content_hashes: HashSet<String> = tracks_map.keys().cloned().collect();

        // Set blob paths based on hash
        for (hash, track) in tracks_map.iter_mut() {
            let hash_clean = hash.strip_prefix("sha256:").unwrap_or(hash);
            if hash_clean.len() >= 2 {
                track.blob_path =
                    PathBuf::from(format!("blobs/{}/{}.flac", &hash_clean[..2], hash_clean));
            }
        }

        LoadResult {
            tracks: tracks_map.into_values().collect(),
            facts_count: total,
            fact_index,
            content_hashes,
        }
    }

    /// Get number of indexed tracks
    pub fn tracks_count(&self) -> usize {
        self.tracks_indexed.load(Ordering::Relaxed)
    }

    /// Get number of facts
    pub fn facts_count(&self) -> usize {
        self.facts_count.load(Ordering::Relaxed)
    }

    /// Resolve an InboxPath to an absolute filesystem path
    fn resolve_inbox_path(&self, inbox_path: &InboxPath) -> PathBuf {
        self.music_dir.join("inbox").join(inbox_path.as_str())
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
                    inbox_queue_size: self.get_inbox_queue_internal().len(),
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                };
                LibraryResponse::Status(status)
            }

            LibraryRequest::ListTracks { limit } => {
                let tracks = self.list_tracks(limit);
                LibraryResponse::Tracks(tracks)
            }

            LibraryRequest::GetTrack { hash } => match self.get_track(&hash) {
                Ok(track) => LibraryResponse::Track(track),
                Err(e) => LibraryResponse::Error(e),
            },

            LibraryRequest::GetFacts { hash } => match self.get_facts(&hash) {
                Ok((full_hash, facts)) => LibraryResponse::Facts {
                    hash: full_hash,
                    facts,
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

            LibraryRequest::GetInboxQueue => {
                let queue = self.get_inbox_queue();
                LibraryResponse::InboxQueue(queue)
            }

            LibraryRequest::IngestFile { path, source } => {
                let result = self.ingest_inbox_file(&path, source.as_ref());
                LibraryResponse::IngestResult(result)
            }

            LibraryRequest::DeleteInboxFile { path } => {
                let result = self.delete_inbox_file(&path);
                LibraryResponse::IngestResult(result)
            }

            LibraryRequest::IngestAll => {
                let results = self.ingest_all();
                LibraryResponse::IngestAllResult(results)
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
        }
    }

    /// Get files in inbox directory (internal, returns PathBuf)
    fn get_inbox_queue_internal(&self) -> Vec<PathBuf> {
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

    /// Get files in inbox directory as InboxPath values
    fn get_inbox_queue(&self) -> Vec<InboxPath> {
        self.get_inbox_queue_internal()
            .into_iter()
            .filter_map(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| InboxPath::new(s).ok())
            })
            .collect()
    }

    /// Convert internal track to protocol TrackInfo
    fn to_track_info(&self, t: &IndexedTrackInfo) -> TrackInfo {
        TrackInfo {
            content_hash: t.content_hash.clone(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration: t.duration_seconds.map(DurationSeconds),
            bpm: t.bpm.and_then(|b| Bpm::from_f32(b).ok()),
            key: t.key.as_ref().and_then(|k| Key::from_traditional(k).ok()),
            blob_path: Some(t.blob_path.to_string_lossy().to_string()),
        }
    }

    /// List tracks from in-memory index
    fn list_tracks(&self, limit: Option<usize>) -> Vec<TrackInfo> {
        let tracks = self.tracks.lock().unwrap();

        let iter = tracks.iter().map(|t| self.to_track_info(t));

        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    /// Resolve a partial hash to a full hash (like git short refs)
    fn resolve_hash(&self, partial: &ContentHash) -> Result<ContentHash, ProtocolError> {
        let partial_clean = partial
            .0
            .strip_prefix("sha256:")
            .unwrap_or(&partial.0)
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
            0 => Err(ProtocolError::TrackNotFound {
                hash: partial.0.clone(),
            }),
            1 => Ok(matches[0].content_hash.clone()),
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
                Err(ProtocolError::Internal {
                    message: format!(
                        "Ambiguous hash '{}' matches {} tracks:\n{}",
                        partial.0,
                        n,
                        examples.join("\n")
                    ),
                })
            }
        }
    }

    /// Get track by hash from in-memory index (supports partial hashes)
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;

        let tracks = self.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.0 == full_hash.0)
            .map(|t| self.to_track_info(t))
            .ok_or_else(|| ProtocolError::TrackNotFound {
                hash: hash.0.clone(),
            })
    }

    /// Get all facts for a track by hash (supports partial hashes)
    fn get_facts(
        &self,
        hash: &ContentHash,
    ) -> Result<(ContentHash, Vec<(String, String)>), ProtocolError> {
        use music_facts::FactSource;
        use stainless_facts::FactStreamReader;

        let full_hash = self.resolve_hash(hash)?;
        let facts_path = self.metadata_dir.join("facts.jsonl");

        let reader: FactStreamReader<ContentHash, MusicValue, FactSource> =
            FactStreamReader::open(&facts_path).map_err(|e| ProtocolError::Internal {
                message: format!("Failed to open facts file: {}", e),
            })?;

        let facts: Vec<_> = reader
            .filter_map(|r| r.ok())
            .filter(|f| f.entity().0 == full_hash.0)
            .map(|f| format_fact_for_display(f.value()))
            .collect();

        if facts.is_empty() {
            Err(ProtocolError::TrackNotFound {
                hash: full_hash.0.clone(),
            })
        } else {
            Ok((full_hash, facts))
        }
    }

    /// Re-read the facts file and update only `last_started` / `last_stopped` in the
    /// in-memory index.  Called before any search that filters on started/stopped so
    /// that facts written by external services (e.g. mdma-playback) after startup
    /// are picked up without a full reload.
    fn refresh_event_timestamps(&self) {
        use music_facts::FactSource;
        use stainless_facts::FactStreamReader;

        let facts_path = self.metadata_dir.join("facts.jsonl");

        let reader: FactStreamReader<ContentHash, MusicValue, FactSource> =
            match FactStreamReader::open(&facts_path) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "refresh_event_timestamps: failed to open facts file: {:?}",
                        e
                    );
                    return;
                }
            };

        // Collect the most-recent TrackStarted/TrackStopped timestamp per content hash
        let mut last_started: HashMap<String, Option<chrono::DateTime<chrono::Utc>>> =
            HashMap::new();
        let mut last_stopped: HashMap<String, Option<chrono::DateTime<chrono::Utc>>> =
            HashMap::new();

        for fact_result in reader {
            let fact = match fact_result {
                Ok(f) => f,
                Err(_) => continue,
            };

            let entity = fact.entity().0.clone();
            match fact.value() {
                MusicValue::TrackStarted(ts) => {
                    update_if_more_recent(last_started.entry(entity).or_insert(None), *ts);
                }
                MusicValue::TrackStopped(ts) => {
                    update_if_more_recent(last_stopped.entry(entity).or_insert(None), *ts);
                }
                _ => {}
            }
        }

        // Acquire the lock only after reading the file to avoid deadlock
        let mut tracks = self.tracks.lock().unwrap();
        for track in tracks.iter_mut() {
            let hash = &track.content_hash.0;
            if let Some(Some(ts)) = last_started.get(hash) {
                track.last_started = Some(*ts);
            }
            if let Some(Some(ts)) = last_stopped.get(hash) {
                track.last_stopped = Some(*ts);
            }
        }
    }

    /// Search tracks by structured query (uses library-search for evaluation)
    fn search_tracks(&self, query: &TrackQuery) -> Vec<TrackInfo> {
        if query.started.is_some() || query.stopped.is_some() {
            self.refresh_event_timestamps();
        }
        let tracks = self.tracks.lock().unwrap();

        tracks
            .iter()
            .filter(|t| {
                let fields = TrackFields {
                    title: t.title.as_deref(),
                    artist: t.artist.as_deref(),
                    album: t.album.as_deref(),
                    label: t.label.as_deref(),
                    genre: t.genre.as_deref(),
                    styles: &t.styles,
                    bpm: t.bpm,
                    key: t.key.as_deref(),
                    duration: t.duration_seconds,
                    year: t.year,
                    source: t.source.as_deref(),
                    last_started: t.last_started,
                    last_stopped: t.last_stopped,
                };
                matches_query(query, &fields)
            })
            .map(|t| self.to_track_info(t))
            .collect()
    }

    /// Ingest a file from the inbox
    fn ingest_inbox_file(
        &self,
        inbox_path: &InboxPath,
        source: Option<&IngestSource>,
    ) -> IngestResult {
        let path = self.resolve_inbox_path(inbox_path);

        match self.ingest_file_internal(&path, source) {
            Ok(hash) => IngestResult {
                hash: Some(hash),
                success: true,
                message: "File ingested successfully".to_string(),
            },
            Err(e) => IngestResult {
                hash: None,
                success: false,
                message: e.to_string(),
            },
        }
    }

    /// Delete a file from the inbox without ingesting
    fn delete_inbox_file(&self, inbox_path: &InboxPath) -> IngestResult {
        let path = self.resolve_inbox_path(inbox_path);

        if !path.exists() {
            return IngestResult {
                hash: None,
                success: false,
                message: format!("File not found: {}", inbox_path.as_str()),
            };
        }

        match std::fs::remove_file(&path) {
            Ok(()) => IngestResult {
                hash: None,
                success: true,
                message: format!("Deleted: {}", inbox_path.as_str()),
            },
            Err(e) => IngestResult {
                hash: None,
                success: false,
                message: format!("Failed to delete: {}", e),
            },
        }
    }

    /// Ingest all files in the inbox
    fn ingest_all(&self) -> Vec<IngestAllItem> {
        let inbox_paths = self.get_inbox_queue();

        inbox_paths
            .into_iter()
            .map(|path| {
                let result = self.ingest_inbox_file(&path, None);
                IngestAllItem { path, result }
            })
            .collect()
    }

    /// Map protocol IngestSource to pipeline UploadSource
    fn map_upload_source(source: Option<&IngestSource>) -> UploadSource {
        match source {
            Some(IngestSource::Bandcamp { artist_url, .. }) => UploadSource::BandcampDownload {
                artist_url: artist_url.clone(),
            },
            Some(IngestSource::Beatport { order_id }) => UploadSource::BeatportZip {
                order_id: order_id.clone(),
            },
            Some(IngestSource::Upload) | None => UploadSource::HttpUpload,
        }
    }

    /// Generate provenance facts from IngestSource
    fn source_facts(
        source: Option<&IngestSource>,
        fact_source: &music_facts::FactSource,
    ) -> Vec<(MusicValue, music_facts::FactSource)> {
        match source {
            Some(IngestSource::Bandcamp { item_id, .. }) => vec![
                (
                    MusicValue::Source("bandcamp".to_string()),
                    fact_source.clone(),
                ),
                (MusicValue::ItemId(item_id.clone()), fact_source.clone()),
            ],
            Some(IngestSource::Beatport { .. }) => vec![(
                MusicValue::Source("beatport".to_string()),
                fact_source.clone(),
            )],
            Some(IngestSource::Upload) => vec![(
                MusicValue::Source("upload".to_string()),
                fact_source.clone(),
            )],
            None => vec![],
        }
    }

    /// Ingest a file through the pipeline (internal, takes absolute path)
    fn ingest_file_internal(
        &self,
        path: &PathBuf,
        source: Option<&IngestSource>,
    ) -> Result<ContentHash, ServiceError> {
        if !path.exists() {
            return Err(ServiceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File not found: {}", path.display()),
            )));
        }

        // Stage 1: Create inbox file
        let upload_source = Self::map_upload_source(source);
        let inbox = InboxFile::new(path.clone(), upload_source);

        // Stage 2: Validate and compute hash
        let validated = inbox.validate()?;
        let content_hash = validated.content_hash.clone();

        // Dedup: if content hash already exists, still append any new source facts
        {
            let hashes = self.content_hashes.lock().unwrap();
            if hashes.contains(&content_hash.0) {
                tracing::info!(
                    hash = %content_hash.0,
                    path = %path.display(),
                    "Duplicate content hash, removing inbox file"
                );
                let _ = std::fs::remove_file(path);

                // Append source facts that don't exist yet for this track
                let fact_source = music_facts::FactSource::new(
                    "mdma-library",
                    env!("CARGO_PKG_VERSION"),
                    music_facts::FactOrigin::Unknown,
                );
                let new_facts = Self::source_facts(source, &fact_source);
                if !new_facts.is_empty() {
                    // Check if this track already has a Source fact
                    let needs_source = {
                        let tracks = self.tracks.lock().unwrap();
                        tracks
                            .iter()
                            .find(|t| t.content_hash.0 == content_hash.0)
                            .map(|t| t.source.is_none())
                            .unwrap_or(false)
                    };

                    if needs_source {
                        // Write to fact stream
                        if let Ok(()) = self
                            .fact_writer
                            .lock()
                            .unwrap()
                            .write_track_facts(&content_hash, &new_facts)
                        {
                            tracing::info!(
                                hash = %content_hash.0,
                                facts = new_facts.len(),
                                "Appended source facts to existing track"
                            );

                            // Update in-memory index
                            let mut tracks = self.tracks.lock().unwrap();
                            if let Some(track) = tracks
                                .iter_mut()
                                .find(|t| t.content_hash.0 == content_hash.0)
                            {
                                for (value, _) in &new_facts {
                                    if let MusicValue::Source(s) = value {
                                        track.source = Some(s.clone());
                                    }
                                }
                            }

                            // Update fact index
                            let mut fact_index = self.fact_index.lock().unwrap();
                            for (value, _) in &new_facts {
                                fact_index
                                    .entry(value.display_name().to_string())
                                    .or_default()
                                    .insert(value.to_string());
                            }

                            self.facts_count
                                .fetch_add(new_facts.len(), Ordering::Relaxed);
                        }
                    }
                }

                return Ok(content_hash);
            }
        }

        // Stage 3: Extract metadata
        let extracted = validated.extract_metadata()?;
        let mut facts = extracted.facts.clone();

        // Stage 4: Import to blob storage
        let indexed = extracted.import(&self.music_dir)?;

        // Add provenance facts from source metadata
        let fact_source = music_facts::FactSource::new(
            "mdma-library",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::Unknown,
        );
        let source_facts = Self::source_facts(source, &fact_source);
        facts.extend(source_facts);

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
            let mut label = None;
            let mut genre = None;
            let mut styles: Vec<String> = vec![];
            let mut duration_seconds = None;
            let mut bpm = None;
            let mut key = None;
            let mut year = None;
            let mut source_str = None;

            for (value, _source) in &facts {
                match value {
                    MusicValue::Title(t) => title = Some(t.0.clone()),
                    MusicValue::Artist(a) => artist = Some(a.0.clone()),
                    MusicValue::Album(a) => album = Some(a.0.clone()),
                    MusicValue::Label(l) => label = Some(l.clone()),
                    MusicValue::MainGenre(g) => genre = Some(g.clone()),
                    MusicValue::StyleDescriptor(s) => styles.push(s.clone()),
                    MusicValue::DurationSeconds(d) => duration_seconds = Some(d.0),
                    MusicValue::Bpm(b) => bpm = Some(b.as_f32()),
                    MusicValue::Key(k) => key = Some(k.to_string()),
                    MusicValue::Year(y) => year = Some(y.0),
                    MusicValue::Source(s) => source_str = Some(s.clone()),
                    _ => {}
                }
            }

            tracks.push(IndexedTrackInfo {
                content_hash: content_hash.clone(),
                title,
                artist,
                album,
                label,
                genre,
                styles,
                duration_seconds,
                bpm,
                key,
                year,
                source: source_str,
                blob_path: indexed.blob_path,
                last_started: None,
                last_stopped: None,
            });
        }

        // Update fact index
        {
            let mut fact_index = self.fact_index.lock().unwrap();
            for (value, _) in &facts {
                fact_index
                    .entry(value.display_name().to_string())
                    .or_default()
                    .insert(value.to_string());
            }
        }

        // Update content hash set
        {
            let mut hashes = self.content_hashes.lock().unwrap();
            hashes.insert(content_hash.0.clone());
        }

        // Update counters
        self.tracks_indexed.fetch_add(1, Ordering::Relaxed);
        self.facts_count.fetch_add(facts.len(), Ordering::Relaxed);

        Ok(content_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact_writer::FactWriter;
    use chrono::{TimeZone, Utc};
    use music_facts::{ContentHash, FactOrigin, FactSource, MusicValue, Title};
    use tempfile::NamedTempFile;

    fn write_facts_file(facts: &[(ContentHash, MusicValue)]) -> NamedTempFile {
        let temp = NamedTempFile::new().unwrap();
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let mut writer = FactWriter::open(temp.path()).unwrap();

        // Group by entity and write
        for (hash, value) in facts {
            writer
                .write_track_facts(hash, &[(value.clone(), source.clone())])
                .unwrap();
        }
        temp
    }

    #[test]
    fn load_tracks_aggregates_played_timestamp() {
        let hash = ContentHash("sha256:aabbcc".to_string());
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Test Track"))),
            (hash.clone(), MusicValue::TrackStarted(ts)),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.last_started,
            Some(ts),
            "last_started should be set from TrackStarted fact"
        );
        assert_eq!(track.last_stopped, None);
    }

    #[test]
    fn load_tracks_aggregates_skipped_timestamp() {
        let hash = ContentHash("sha256:ddeeff".to_string());
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 8, 30, 0).unwrap();

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Skip Me"))),
            (hash.clone(), MusicValue::TrackStopped(ts)),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        let track = &result.tracks[0];
        assert_eq!(
            track.last_stopped,
            Some(ts),
            "last_stopped should be set from TrackStopped fact"
        );
        assert_eq!(track.last_started, None);
    }

    #[test]
    fn load_tracks_keeps_most_recent_played() {
        let hash = ContentHash("sha256:112233".to_string());
        let older = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 1, 20, 0, 0, 0).unwrap();

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Two Plays"))),
            (hash.clone(), MusicValue::TrackStarted(older)),
            (hash.clone(), MusicValue::TrackStarted(newer)),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        let track = &result.tracks[0];
        assert_eq!(
            track.last_started,
            Some(newer),
            "should keep the most recent TrackStarted timestamp"
        );
    }

    #[test]
    fn search_passes_last_started_to_evaluator() {
        use library_search::{query::DateQuery, TrackQuery};

        let hash = ContentHash("sha256:445566".to_string());
        let ts = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Played Track"))),
            (hash.clone(), MusicValue::TrackStarted(ts)),
        ]);

        // Build a minimal LibraryService pointing at temp facts
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        // Copy the temp facts file to where the service expects it
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
        )
        .unwrap();

        // A query asking for tracks that have NOT been played (NA) should return 0 results
        let query = TrackQuery {
            started: Some(DateQuery::NA),
            ..Default::default()
        };
        let results = service.search_tracks(&query);
        assert!(
            results.is_empty(),
            "DateQuery::NA should not match a track with last_started set"
        );
    }

    #[test]
    fn search_played_na_excludes_after_external_append() {
        use library_search::{query::DateQuery, TrackQuery};

        let hash = ContentHash("sha256:778899aabbcc".to_string());

        // Setup: one track with no TrackStarted fact
        let temp = write_facts_file(&[(
            hash.clone(),
            MusicValue::Title(Title::new("External Append Track")),
        )]);

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
        )
        .unwrap();

        let na_query = TrackQuery {
            started: Some(DateQuery::NA),
            ..Default::default()
        };

        // Before: track has no play history → should appear in NA results
        let before = service.search_tracks(&na_query);
        assert_eq!(before.len(), 1, "track should appear before play event");

        // Simulate playback service appending a TrackStarted fact after startup
        let ts = Utc.with_ymd_and_hms(2026, 2, 20, 12, 0, 0).unwrap();
        let source = music_facts::FactSource::new(
            "test-playback",
            "1.0.0",
            music_facts::FactOrigin::Unknown,
        );
        let mut writer = FactWriter::open(&facts_dest).unwrap();
        writer
            .write_track_facts(&hash, &[(MusicValue::TrackStarted(ts), source)])
            .unwrap();
        drop(writer);

        // After: track now has play history → NA query should return 0
        let after = service.search_tracks(&na_query);
        assert!(
            after.is_empty(),
            "DateQuery::NA should exclude track after TrackStarted fact appended externally"
        );
    }
}

/// Run the IPC server loop
pub fn run_ipc_server(
    service: Arc<LibraryService>,
    address: &str,
    tcp_address: Option<&str>,
) -> Result<(), ServiceError> {
    let server = IpcServer::bind(address)?;

    // Also listen on TCP if specified (for remote connections)
    if let Some(tcp) = tcp_address {
        server.listen_also(tcp)?;
    }

    tracing::info!("IPC server running, waiting for requests...");

    loop {
        match server.recv() {
            Ok(request) => {
                let response = service.handle_request(request);
                if let Err(e) = server.send(&response) {
                    tracing::error!(error = %e, "Failed to send response, sending error fallback");
                    let fallback = LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Internal error: {}", e),
                    });
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
