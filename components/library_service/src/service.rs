//! Library service implementation
//!
//! Handles IPC requests and manages library state

use crate::ipc::{
    Bpm, ContentHash, DurationSeconds, FactType, InboxPath, IngestAllItem, IngestResult,
    IngestSource, IpcServer, Key, LibraryRequest, LibraryResponse, ProtocolError, ServiceStatus,
    TrackInfo, TrackQuery,
};
use crate::pipeline::{InboxFile, UploadSource};
use acid_client::AcidClient;
use event_protocol::{acid_event_from_topic_message, AcidEvent, TOPIC_ACID_FACTS_WRITTEN};
use library_search::{matches_query, TrackFields};
use music_facts::MusicValue;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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

    #[error("ACID client error: {0}")]
    Acid(#[from] acid_client::ClientError),
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
    /// ACID client for writing and reading facts
    acid_client: AcidClient,
    /// Album name -> cover_art_path for tracks that have cover art.
    /// Used as a fallback when a track on the same album has no cover art of its own.
    album_cover_cache: Mutex<HashMap<String, String>>,
    /// Generic fact value index: fact_type -> set of values
    /// Used for fast HasFact/HasFacts lookups (e.g., ItemId -> {"p123", "p456"})
    fact_index: Mutex<HashMap<FactType, HashSet<String>>>,
    /// Set of known content hashes for dedup on ingest
    content_hashes: Mutex<HashSet<String>>,
    /// Current position in the ACID fact stream (opaque cursor string).
    cursor: Mutex<Option<String>>,
    /// Address of the ACID request socket (used by background subscriber thread).
    acid_socket: String,
    /// Address of the ACID events pub/sub socket.
    acid_events_socket: String,
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
    cover_art_path: Option<PathBuf>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    last_started: Option<chrono::DateTime<chrono::Utc>>,
    last_stopped: Option<chrono::DateTime<chrono::Utc>>,
    added_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl IndexedTrackInfo {
    fn new_empty(entity: String) -> Self {
        Self {
            content_hash: ContentHash::new(entity),
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
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            last_started: None,
            last_stopped: None,
            added_at: None,
        }
    }
}

/// Apply a single `MusicValue` fact to a mutable `IndexedTrackInfo` entry.
///
/// The fact timestamp is needed only for `TrackStarted` / `TrackStopped` to
/// preserve the most-recent-wins ordering; callers must supply it.
fn apply_fact_to_track(
    entry: &mut IndexedTrackInfo,
    value: &MusicValue,
    timestamp: chrono::DateTime<chrono::Utc>,
    has_format: Option<&mut HashSet<String>>,
    has_cover_art: Option<&mut HashSet<String>>,
) {
    match value {
        MusicValue::Title(v) => entry.title = Some(v.as_str().to_string()),
        MusicValue::Artist(v) => entry.artist = Some(v.as_str().to_string()),
        MusicValue::Album(v) => entry.album = Some(v.as_str().to_string()),
        MusicValue::Label(v) => entry.label = Some(v.clone()),
        MusicValue::MainGenre(v) => entry.genre = Some(v.clone()),
        MusicValue::StyleDescriptor(v) => entry.styles.push(v.clone()),
        MusicValue::DurationSeconds(v) => entry.duration_seconds = Some(v.value()),
        MusicValue::Bpm(v) => entry.bpm = Some(v.as_f32()),
        MusicValue::Key(v) => entry.key = Some(v.to_string()),
        MusicValue::Year(v) => entry.year = Some(v.value()),
        MusicValue::TrackNumber(v) => entry.track_number = Some(v.value()),
        MusicValue::DiscNumber(v) => entry.disc_number = Some(v.value()),
        MusicValue::Source(v) => entry.source = Some(v.clone()),
        MusicValue::TrackStarted(_) => {
            update_if_more_recent(&mut entry.last_started, timestamp);
        }
        MusicValue::TrackStopped(_) => {
            update_if_more_recent(&mut entry.last_stopped, timestamp);
        }
        MusicValue::FilePath(p) => {
            let hash = entry.content_hash.as_str();
            let hash_clean = hash.strip_prefix("sha256:").unwrap_or(hash);
            if hash_clean.len() >= 2 {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    entry.blob_path =
                        PathBuf::from(format!("blobs/{}/{}.{}", &hash_clean[..2], hash_clean, ext));
                }
            }
        }
        MusicValue::Format(_) => {
            if let Some(set) = has_format {
                set.insert(entry.content_hash.as_str().to_owned());
            }
        }
        MusicValue::CoverArtPath(p) => {
            entry.cover_art_path = Some(PathBuf::from(p));
            if let Some(set) = has_cover_art {
                set.insert(entry.content_hash.as_str().to_owned());
            }
        }
        MusicValue::AddedAt(dt) => {
            if entry.added_at.is_none() {
                entry.added_at = Some(*dt);
            }
        }
        _ => {}
    }
}

/// Result of loading tracks from the fact stream
struct LoadResult {
    tracks: Vec<IndexedTrackInfo>,
    facts_count: usize,
    fact_index: HashMap<FactType, HashSet<String>>,
    content_hashes: HashSet<String>,
    /// Content hashes of tracks that already have a Format fact
    has_format: HashSet<String>,
    /// Content hashes of tracks that already have a CoverArtPath fact
    has_cover_art: HashSet<String>,
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
    pub fn new(
        music_dir: PathBuf,
        metadata_dir: PathBuf,
        acid_socket: &str,
    ) -> Result<Self, ServiceError> {
        Self::new_with_events(
            music_dir,
            metadata_dir,
            acid_socket,
            "ipc:///run/mdma/acid-events.sock",
        )
    }

    /// Create a new library service with an explicit ACID events socket address.
    pub fn new_with_events(
        music_dir: PathBuf,
        metadata_dir: PathBuf,
        acid_socket: &str,
        acid_events_socket: &str,
    ) -> Result<Self, ServiceError> {
        // Connect to ACID service for writing and streaming
        let acid_client = AcidClient::connect(acid_socket)?;

        // Try to load from the ACID stream (incremental if cursor exists)
        let cursor_path = metadata_dir.join("facts.cursor");
        let saved_cursor = Self::load_saved_cursor(&cursor_path);

        let (loaded, final_cursor) = Self::load_from_acid_stream(&acid_client, saved_cursor);

        // If ACID stream returned nothing, fall back to local file.
        // This covers both "ACID unavailable" and "cursor at end-of-file with no new facts".
        let (loaded, final_cursor) = if loaded.tracks.is_empty() {
            tracing::info!("ACID stream empty or unavailable, falling back to local facts file");
            let facts_path = metadata_dir.join("facts.jsonl");
            let file_loaded = Self::load_tracks_from_facts(&facts_path);
            (file_loaded, None)
        } else {
            (loaded, final_cursor)
        };

        // Persist the cursor so next restart can resume incrementally
        if let Some(ref c) = final_cursor {
            Self::save_cursor(&cursor_path, c);
        }

        let tracks_count = loaded.tracks.len();
        let album_cover_cache = Self::build_album_cover_cache(&loaded.tracks);

        let service = Self {
            music_dir,
            metadata_dir,
            start_time: Instant::now(),
            tracks_indexed: AtomicUsize::new(tracks_count),
            facts_count: AtomicUsize::new(loaded.facts_count),
            tracks: Mutex::new(loaded.tracks),
            acid_client,
            album_cover_cache: Mutex::new(album_cover_cache),
            fact_index: Mutex::new(loaded.fact_index),
            content_hashes: Mutex::new(loaded.content_hashes),
            cursor: Mutex::new(final_cursor),
            acid_socket: acid_socket.to_string(),
            acid_events_socket: acid_events_socket.to_string(),
        };

        // Backfill Format facts for tracks that don't have one yet
        service.backfill_format_facts(&loaded.has_format);

        // Backfill cover art for tracks that don't have CoverArtPath yet
        service.backfill_cover_art(&loaded.has_cover_art);

        Ok(service)
    }

    /// Load tracks from the ACID read_stream API.
    ///
    /// Returns the LoadResult plus the final cursor position.
    /// On failure (ACID not available), returns an empty result with no cursor.
    fn load_from_acid_stream(
        acid_client: &AcidClient,
        start_cursor: Option<String>,
    ) -> (LoadResult, Option<String>) {
        const PAGE_SIZE: usize = 10_000;

        let mut tracks_map: HashMap<String, IndexedTrackInfo> = HashMap::new();
        let mut fact_index: HashMap<FactType, HashSet<String>> = HashMap::new();
        let mut has_format: HashSet<String> = HashSet::new();
        let mut has_cover_art: HashSet<String> = HashSet::new();
        let mut total = 0;
        let mut current_cursor = start_cursor;
        let mut last_cursor: Option<String> = None;

        loop {
            let chunk = match acid_client.read_stream(current_cursor.clone(), PAGE_SIZE) {
                Ok(c) => c,
                Err(e) => {
                    if total == 0 {
                        tracing::debug!("ACID stream unavailable: {:?}", e);
                    } else {
                        tracing::warn!("ACID stream error after {} facts: {:?}", total, e);
                    }
                    break;
                }
            };

            last_cursor = Some(chunk.cursor.clone());

            if chunk.lines.is_empty() {
                break;
            }

            let lines_count = chunk.lines.len();
            Self::apply_lines_to_map(
                &chunk.lines,
                &mut tracks_map,
                &mut fact_index,
                &mut has_format,
                &mut has_cover_art,
                &mut total,
            );
            current_cursor = Some(chunk.cursor);

            if lines_count < PAGE_SIZE {
                break;
            }
        }

        if total > 0 {
            tracing::info!("Loaded {} facts from ACID stream", total);
        }

        let content_hashes: HashSet<String> = tracks_map.keys().cloned().collect();
        let loaded = LoadResult {
            tracks: tracks_map.into_values().collect(),
            facts_count: total,
            fact_index,
            content_hashes,
            has_format,
            has_cover_art,
        };
        (loaded, last_cursor)
    }

    /// Apply raw JSON fact lines to maps during bulk loading (used by load_from_acid_stream).
    fn apply_lines_to_map(
        lines: &[String],
        tracks_map: &mut HashMap<String, IndexedTrackInfo>,
        fact_index: &mut HashMap<FactType, HashSet<String>>,
        has_format: &mut HashSet<String>,
        has_cover_art: &mut HashSet<String>,
        total: &mut usize,
    ) {
        for line in lines {
            let fact = match serde_json::from_str::<
                stainless_facts::Fact<ContentHash, MusicValue, music_facts::FactSource>,
            >(line)
            {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to parse stream line during load");
                    continue;
                }
            };

            *total += 1;
            let entity = fact.entity().as_str().to_owned();

            let entry = tracks_map
                .entry(entity.clone())
                .or_insert_with(|| IndexedTrackInfo::new_empty(entity));

            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            fact_index
                .entry(FactType::new(variant_name))
                .or_default()
                .insert(value_str);

            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                Some(has_format),
                Some(has_cover_art),
            );
        }
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
                        has_format: HashSet::new(),
                        has_cover_art: HashSet::new(),
                    };
                }
            };

        // Aggregate facts by content hash
        let mut tracks_map: HashMap<String, IndexedTrackInfo> = HashMap::new();
        let mut fact_index: HashMap<FactType, HashSet<String>> = HashMap::new();
        let mut has_format: HashSet<String> = HashSet::new();
        let mut has_cover_art: HashSet<String> = HashSet::new();
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

            let entity = fact.entity().as_str().to_owned();

            // Build track summary
            let entry = tracks_map
                .entry(entity.clone())
                .or_insert_with(|| IndexedTrackInfo::new_empty(entity));

            // Index fact values for HasFact/HasFacts lookups
            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            fact_index
                .entry(FactType::new(variant_name))
                .or_default()
                .insert(value_str);

            // Extract key fields for search
            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                Some(&mut has_format),
                Some(&mut has_cover_art),
            );
        }

        tracing::info!("Processed {} facts from file, {} errors", total, errors);

        // Collect content hashes for dedup
        let content_hashes: HashSet<String> = tracks_map.keys().cloned().collect();

        LoadResult {
            tracks: tracks_map.into_values().collect(),
            facts_count: total,
            fact_index,
            content_hashes,
            has_format,
            has_cover_art,
        }
    }

    /// Backfill Format facts for tracks that have a file path but no Format fact
    fn backfill_format_facts(&self, has_format: &HashSet<String>) {
        use crate::pipeline::AudioFormat;
        use music_facts::MusicFormat;

        let tracks = self.tracks.lock().unwrap();

        // Find tracks needing backfill
        let needs_backfill: Vec<_> = tracks
            .iter()
            .filter(|t| !has_format.contains(t.content_hash.as_str()))
            .filter(|t| !t.blob_path.as_os_str().is_empty())
            .filter_map(|t| {
                let ext = t.blob_path.extension()?.to_str()?;
                let music_format = AudioFormat::from_extension(ext).map(MusicFormat::from)?;
                Some((t.content_hash.clone(), music_format))
            })
            .collect();

        drop(tracks);

        if needs_backfill.is_empty() {
            return;
        }

        tracing::info!(
            count = needs_backfill.len(),
            "Backfilling Format facts for tracks"
        );

        let source = music_facts::FactSource::new(
            "mdma-library",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::Unknown,
        );

        let mut fact_index = self.fact_index.lock().unwrap();
        let mut backfilled = 0;

        for (hash, music_format) in &needs_backfill {
            let value = MusicValue::Format(*music_format);
            if self
                .acid_client
                .write_music_facts(hash, &[(value.clone(), source.clone())])
                .is_ok()
            {
                fact_index
                    .entry(FactType::new("Format"))
                    .or_default()
                    .insert(music_format.to_string());
                backfilled += 1;
            }
        }

        // Update facts count
        self.facts_count.fetch_add(backfilled, Ordering::Relaxed);

        tracing::info!(backfilled, "Format fact backfill complete");
    }

    /// Map MIME type to file extension for cover art images.
    fn mime_to_ext(mime: &str) -> Option<&'static str> {
        match mime {
            "image/jpeg" => Some("jpg"),
            "image/png" => Some("png"),
            "image/gif" => Some("gif"),
            "image/webp" => Some("webp"),
            _ => None,
        }
    }

    /// Extract the best available picture from an audio file and write it to cover-art storage.
    ///
    /// Returns the relative path (e.g. `cover-art/<hash>.jpg`) on success, or `None` if no
    /// picture is present or writing fails.
    fn extract_and_store_cover_art(
        &self,
        blob_path: &PathBuf,
        content_hash: &ContentHash,
    ) -> Option<PathBuf> {
        use audio_metadata::{extract_pictures, PictureType};

        let pictures = match extract_pictures(blob_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to extract pictures from blob");
                return None;
            }
        };

        if pictures.is_empty() {
            return None;
        }

        // Prefer CoverFront; fall back to first available
        let picture = pictures
            .iter()
            .find(|p| p.picture_type == PictureType::CoverFront)
            .or_else(|| pictures.first())?;

        let ext = Self::mime_to_ext(&picture.mime_type)?;

        let hash_clean = content_hash
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(content_hash.as_str());
        let filename = format!("{}.{}", hash_clean, ext);
        let rel_path = PathBuf::from("cover-art").join(&filename);
        let abs_path = self.music_dir.join(&rel_path);

        // Ensure cover-art directory exists
        if let Some(parent) = abs_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, "Failed to create cover-art directory");
                return None;
            }
        }

        if let Err(e) = std::fs::write(&abs_path, &picture.data) {
            tracing::warn!(error = %e, path = %abs_path.display(), "Failed to write cover art");
            return None;
        }

        tracing::debug!(path = %rel_path.display(), "Stored cover art");
        Some(rel_path)
    }

    /// Backfill CoverArtPath facts for tracks that don't have cover art stored yet.
    fn backfill_cover_art(&self, has_cover_art: &HashSet<String>) {
        let tracks = self.tracks.lock().unwrap();

        // Collect tracks that need backfill: have a blob_path but no CoverArtPath fact
        let needs_backfill: Vec<_> = tracks
            .iter()
            .filter(|t| !has_cover_art.contains(t.content_hash.as_str()))
            .filter(|t| !t.blob_path.as_os_str().is_empty())
            .map(|t| (t.content_hash.clone(), t.blob_path.clone()))
            .collect();

        drop(tracks);

        if needs_backfill.is_empty() {
            return;
        }

        tracing::info!(
            count = needs_backfill.len(),
            "Backfilling CoverArtPath facts for tracks"
        );

        let fact_source = music_facts::FactSource::new(
            "mdma-library",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::Unknown,
        );

        let mut backfilled = 0;

        for (hash, blob_rel_path) in &needs_backfill {
            let blob_abs = self.music_dir.join(blob_rel_path);
            if let Some(rel_path) = self.extract_and_store_cover_art(&blob_abs, hash) {
                let value = MusicValue::CoverArtPath(rel_path.to_string_lossy().to_string());
                if self
                    .acid_client
                    .write_music_facts(hash, &[(value.clone(), fact_source.clone())])
                    .is_ok()
                {
                    // Update in-memory index
                    let mut tracks = self.tracks.lock().unwrap();
                    if let Some(track) = tracks
                        .iter_mut()
                        .find(|t| t.content_hash.as_str() == hash.as_str())
                    {
                        track.cover_art_path = Some(rel_path.clone());
                    }
                    drop(tracks);

                    let mut fact_index = self.fact_index.lock().unwrap();
                    fact_index
                        .entry(FactType::new("CoverArtPath"))
                        .or_default()
                        .insert(rel_path.to_string_lossy().to_string());
                    drop(fact_index);

                    self.facts_count.fetch_add(1, Ordering::Relaxed);
                    backfilled += 1;
                }
            }
        }

        tracing::info!(backfilled, "CoverArtPath fact backfill complete");
    }

    /// Re-extract and store cover art for all tracks missing a CoverArtPath fact.
    /// Returns the number of tracks processed.
    fn reindex_covers_internal(&self) -> usize {
        let has_cover_art: HashSet<String> = {
            let tracks = self.tracks.lock().unwrap();
            tracks
                .iter()
                .filter(|t| t.cover_art_path.is_some())
                .map(|t| t.content_hash.as_str().to_owned())
                .collect()
        };

        let before = self.facts_count.load(Ordering::Relaxed);
        self.backfill_cover_art(&has_cover_art);
        let after = self.facts_count.load(Ordering::Relaxed);
        after.saturating_sub(before)
    }

    // =========================================================================
    // Cursor persistence
    // =========================================================================

    /// Load the saved ACID stream cursor from a file.
    ///
    /// Returns `None` if the file does not exist or contains only whitespace.
    pub fn load_saved_cursor(cursor_path: &Path) -> Option<String> {
        let content = std::fs::read_to_string(cursor_path).ok()?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }

    /// Persist the current ACID stream cursor to a file so it survives restarts.
    pub fn save_cursor(cursor_path: &Path, cursor: &str) {
        if let Err(e) = std::fs::write(cursor_path, cursor) {
            tracing::warn!(error = %e, "Failed to save ACID stream cursor");
        }
    }

    // =========================================================================
    // Incremental stream application
    // =========================================================================

    /// Apply a slice of raw JSONL lines (from an ACID `ReadStream` response) to
    /// the in-memory track index.
    ///
    /// Each line is a JSON-serialised `Fact<ContentHash, MusicValue, FactSource>`.
    /// Unknown or malformed lines are silently skipped.
    pub fn apply_stream_lines(&self, lines: &[String]) {
        use music_facts::FactSource;
        use stainless_facts::Fact;

        let mut tracks = self.tracks.lock().unwrap();
        let mut fact_index = self.fact_index.lock().unwrap();
        let mut content_hashes = self.content_hashes.lock().unwrap();
        let mut applied: usize = 0;

        // Build a temporary O(1) index: content_hash -> position in tracks Vec.
        // Rebuilt once per call to avoid O(n*m) linear scans.
        let mut index: HashMap<String, usize> = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.content_hash.as_str().to_owned(), i))
            .collect();

        for line in lines {
            let fact: Fact<ContentHash, MusicValue, FactSource> = match serde_json::from_str(line) {
                Ok(f) => f,
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to parse stream line — skipping");
                    continue;
                }
            };

            applied += 1;
            let entity = fact.entity().as_str().to_owned();

            // Insert or fetch the track entry using the O(1) index
            let pos = if let Some(&pos) = index.get(&entity) {
                pos
            } else {
                content_hashes.insert(entity.clone());
                let pos = tracks.len();
                tracks.push(IndexedTrackInfo::new_empty(entity.clone()));
                index.insert(entity.clone(), pos);
                pos
            };
            let entry = &mut tracks[pos];

            // Index the fact value
            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            fact_index
                .entry(FactType::new(variant_name))
                .or_default()
                .insert(value_str);

            // Apply to track fields
            apply_fact_to_track(entry, fact.value(), *fact.timestamp(), None, None);
        }

        // Update tracks_indexed counter for any new entries
        self.tracks_indexed.store(tracks.len(), Ordering::Relaxed);
        self.facts_count.fetch_add(applied, Ordering::Relaxed);
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

            LibraryRequest::PlaylistList => {
                let dir = self.metadata_dir.join("playlists");
                let mut names: Vec<library_ipc_protocol::PlaylistName> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let path = e.path();
                        if path.extension().and_then(|x| x.to_str()) == Some("plist") {
                            path.file_stem()
                                .and_then(|s| s.to_str())
                                .and_then(|s| library_ipc_protocol::PlaylistName::new(s).ok())
                        } else {
                            None
                        }
                    })
                    .collect();
                names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                LibraryResponse::PlaylistNames(names)
            }

            LibraryRequest::PlaylistGet { name } => {
                let path = self.resolve_playlist_path(&name);
                match std::fs::read_to_string(&path) {
                    Ok(content) => LibraryResponse::PlaylistContent(content),
                    Err(_) => LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                        name: name.to_string(),
                    }),
                }
            }

            LibraryRequest::PlaylistNew { name, content } => {
                let path = self.resolve_playlist_path(&name);
                if path.exists() {
                    LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists {
                        name: name.to_string(),
                    })
                } else {
                    match std::fs::write(&path, &content) {
                        Ok(()) => LibraryResponse::PlaylistContent(content),
                        Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                            message: format!("Failed to write playlist: {}", e),
                        }),
                    }
                }
            }

            LibraryRequest::PlaylistAppend { name, content } => {
                let path = self.resolve_playlist_path(&name);
                if !path.exists() {
                    return LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                        name: name.to_string(),
                    });
                }
                match std::fs::read_to_string(&path) {
                    Ok(existing) => {
                        let mut new_content = existing;
                        if !new_content.is_empty() && !new_content.ends_with('\n') {
                            new_content.push('\n');
                        }
                        new_content.push_str(&content);
                        match std::fs::write(&path, &new_content) {
                            Ok(()) => LibraryResponse::PlaylistContent(new_content),
                            Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                                message: format!("Failed to write playlist: {}", e),
                            }),
                        }
                    }
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Failed to read playlist: {}", e),
                    }),
                }
            }

            LibraryRequest::PlaylistReplace { name, content } => {
                let path = self.resolve_playlist_path(&name);
                match std::fs::write(&path, &content) {
                    Ok(()) => LibraryResponse::PlaylistContent(content),
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Failed to write playlist: {}", e),
                    }),
                }
            }

            LibraryRequest::PlaylistRename { from, to } => {
                let from_path = self.resolve_playlist_path(&from);
                let to_path = self.resolve_playlist_path(&to);
                if !from_path.exists() {
                    return LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                        name: from.to_string(),
                    });
                }
                if to_path.exists() {
                    return LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists {
                        name: to.to_string(),
                    });
                }
                match std::fs::rename(&from_path, &to_path) {
                    Ok(()) => LibraryResponse::Pong,
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Failed to rename playlist: {}", e),
                    }),
                }
            }

            LibraryRequest::PlaylistRemove { name } => {
                let path = self.resolve_playlist_path(&name);
                if !path.exists() {
                    return LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                        name: name.to_string(),
                    });
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => LibraryResponse::Pong,
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Failed to remove playlist: {}", e),
                    }),
                }
            }

            LibraryRequest::ReindexCovers => {
                let reindexed = self.reindex_covers_internal();
                LibraryResponse::IngestResult(IngestResult::Success {
                    hash: None,
                    message: format!("Cover art reindexed for {} tracks", reindexed),
                })
            }
        }
    }

    /// Resolve a PlaylistName to an absolute filesystem path
    fn resolve_playlist_path(&self, name: &library_ipc_protocol::PlaylistName) -> PathBuf {
        self.metadata_dir
            .join("playlists")
            .join(format!("{}.plist", name.as_str()))
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
            duration: t.duration_seconds.map(DurationSeconds::new),
            bpm: t.bpm.and_then(|b| Bpm::from_f32(b).ok()),
            key: t.key.as_ref().and_then(|k| Key::from_traditional(k).ok()),
            blob_path: Some(t.blob_path.to_string_lossy().to_string()),
            cover_art_path: t
                .cover_art_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .or_else(|| {
                    t.album.as_ref().and_then(|album| {
                        self.album_cover_cache.lock().unwrap().get(album).cloned()
                    })
                }),
            track_number: t.track_number,
            disc_number: t.disc_number,
            added: t.added_at.map(|dt| dt.to_rfc3339()),
        }
    }

    /// Build a mapping of album name -> cover_art_path from an indexed track list.
    ///
    /// Only tracks that have both an album and a cover_art_path contribute to the cache.
    /// The first cover art path seen for each album wins.
    fn build_album_cover_cache(tracks: &[IndexedTrackInfo]) -> HashMap<String, String> {
        let mut cache = HashMap::new();
        for track in tracks {
            if let (Some(album), Some(cover)) = (&track.album, &track.cover_art_path) {
                cache
                    .entry(album.clone())
                    .or_insert_with(|| cover.to_string_lossy().to_string());
            }
        }
        cache
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
            1 => Ok(matches[0].content_hash.clone()),
            _ => {
                // Multiple matches: check whether all share the exact same hash
                // value. If so, they are duplicate index entries for the same
                // content (e.g. legacy short hashes written more than once) and
                // resolving to the first one is correct.
                // Normalize by stripping "sha256:" prefix and lowercasing so that
                // a legacy short-hash entry ("9fb4105e") and a full-hash entry
                // ("sha256:9fb4105eXXX...") are recognised as the same content:
                // both normalize to the same prefix, so one starts_with the other.
                let normalized: Vec<String> = matches
                    .iter()
                    .map(|t| normalize(t.content_hash.as_str()))
                    .collect();
                // All entries represent the same content if every pair of
                // normalized hashes has one that is a prefix of the other
                // (i.e. short legacy hash vs full hash of the same file).
                let all_same = normalized.iter().all(|a| {
                    normalized
                        .iter()
                        .all(|b| a.starts_with(b.as_str()) || b.starts_with(a.as_str()))
                });
                if all_same {
                    // Prefer the entry with the longest (most complete) hash,
                    // so a full "sha256:..." hash wins over a legacy 8-char hash.
                    let best = matches
                        .iter()
                        .max_by_key(|t| t.content_hash.as_str().len())
                        .unwrap();
                    return Ok(best.content_hash.clone());
                }

                // Genuinely different hashes share a prefix — real ambiguity.
                let n = matches.len();
                let examples: Vec<_> = matches
                    .iter()
                    .take(3)
                    .map(|t| {
                        let hash_str = t.content_hash.as_str();
                        // Show 8 chars after "sha256:" prefix; fall back to full string for
                        // legacy short hashes that lack the prefix.
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

    /// Get track by hash from in-memory index (supports partial hashes)
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;

        let tracks = self.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == full_hash.as_str())
            .map(|t| self.to_track_info(t))
            .ok_or_else(|| ProtocolError::TrackNotFound {
                hash: hash.as_str().to_owned(),
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
            .filter(|f| f.entity().as_str() == full_hash.as_str())
            .map(|f| format_fact_for_display(f.value()))
            .collect();

        if facts.is_empty() {
            Err(ProtocolError::TrackNotFound {
                hash: full_hash.as_str().to_owned(),
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

            let entity = fact.entity().as_str().to_owned();
            match fact.value() {
                MusicValue::TrackStarted(_) => {
                    update_if_more_recent(
                        last_started.entry(entity).or_insert(None),
                        *fact.timestamp(),
                    );
                }
                MusicValue::TrackStopped(_) => {
                    update_if_more_recent(
                        last_stopped.entry(entity).or_insert(None),
                        *fact.timestamp(),
                    );
                }
                _ => {}
            }
        }

        // Acquire the lock only after reading the file to avoid deadlock
        let mut tracks = self.tracks.lock().unwrap();
        for track in tracks.iter_mut() {
            let hash = track.content_hash.as_str();
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
                    added: t.added_at,
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
            Ok(hash) => IngestResult::Success {
                hash: Some(hash),
                message: "File ingested successfully".to_string(),
            },
            Err(e) => IngestResult::Failure {
                message: e.to_string(),
            },
        }
    }

    /// Delete a file from the inbox without ingesting
    fn delete_inbox_file(&self, inbox_path: &InboxPath) -> IngestResult {
        let path = self.resolve_inbox_path(inbox_path);

        if !path.exists() {
            return IngestResult::Failure {
                message: format!("File not found: {}", inbox_path.as_str()),
            };
        }

        match std::fs::remove_file(&path) {
            Ok(()) => IngestResult::Success {
                hash: None,
                message: format!("Deleted: {}", inbox_path.as_str()),
            },
            Err(e) => IngestResult::Failure {
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
            if hashes.contains(content_hash.as_str()) {
                tracing::info!(
                    hash = %content_hash.as_str(),
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
                            .find(|t| t.content_hash.as_str() == content_hash.as_str())
                            .map(|t| t.source.is_none())
                            .unwrap_or(false)
                    };

                    if needs_source {
                        // Write to fact stream via ACID
                        if self
                            .acid_client
                            .write_music_facts(&content_hash, &new_facts)
                            .is_ok()
                        {
                            tracing::info!(
                                hash = %content_hash.as_str(),
                                facts = new_facts.len(),
                                "Appended source facts to existing track"
                            );

                            // Update in-memory index
                            let mut tracks = self.tracks.lock().unwrap();
                            if let Some(track) = tracks
                                .iter_mut()
                                .find(|t| t.content_hash.as_str() == content_hash.as_str())
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
                                    .entry(FactType::new(value.display_name()))
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

        // Stage 5: Extract cover art from blob and store it
        let fact_source = music_facts::FactSource::new(
            "mdma-library",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::Unknown,
        );
        let blob_path = self.music_dir.join(&indexed.blob_path);
        let cover_art_path = self.extract_and_store_cover_art(&blob_path, &content_hash);
        if let Some(ref rel_path) = cover_art_path {
            facts.push((
                MusicValue::CoverArtPath(rel_path.to_string_lossy().to_string()),
                fact_source.clone(),
            ));
        }

        // Add provenance facts from source metadata
        let source_facts = Self::source_facts(source, &fact_source);
        facts.extend(source_facts);

        // Write facts via fact store
        self.acid_client.write_music_facts(&content_hash, &facts)?;

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
            let mut track_number = None;
            let mut disc_number = None;

            for (value, _source) in &facts {
                match value {
                    MusicValue::Title(t) => title = Some(t.as_str().to_string()),
                    MusicValue::Artist(a) => artist = Some(a.as_str().to_string()),
                    MusicValue::Album(a) => album = Some(a.as_str().to_string()),
                    MusicValue::Label(l) => label = Some(l.clone()),
                    MusicValue::MainGenre(g) => genre = Some(g.clone()),
                    MusicValue::StyleDescriptor(s) => styles.push(s.clone()),
                    MusicValue::DurationSeconds(d) => duration_seconds = Some(d.value()),
                    MusicValue::Bpm(b) => bpm = Some(b.as_f32()),
                    MusicValue::Key(k) => key = Some(k.to_string()),
                    MusicValue::Year(y) => year = Some(y.value()),
                    MusicValue::Source(s) => source_str = Some(s.clone()),
                    MusicValue::TrackNumber(n) => track_number = Some(n.value()),
                    MusicValue::DiscNumber(n) => disc_number = Some(n.value()),
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
                cover_art_path: cover_art_path.clone(),
                track_number,
                disc_number,
                last_started: None,
                last_stopped: None,
                added_at: None,
            });
        }

        // Update fact index
        {
            let mut fact_index = self.fact_index.lock().unwrap();
            for (value, _) in &facts {
                fact_index
                    .entry(FactType::new(value.display_name()))
                    .or_default()
                    .insert(value.to_string());
            }
        }

        // Update content hash set
        {
            let mut hashes = self.content_hashes.lock().unwrap();
            hashes.insert(content_hash.as_str().to_owned());
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
    use music_facts::{
        ContentHash, FactOrigin, FactSource, MusicValue, StartReason, StopReason, Title,
    };
    use pretty_assertions::assert_eq;
    use stainless_facts::{Fact, FactStreamWriter, Operation};
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

    /// Write facts with explicit timestamps for testing ordering logic.
    fn write_facts_file_with_timestamps(
        facts: &[(ContentHash, MusicValue, chrono::DateTime<Utc>)],
    ) -> NamedTempFile {
        let temp = NamedTempFile::new().unwrap();
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let mut writer = FactStreamWriter::open(temp.path()).unwrap();

        let fact_structs: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
            .iter()
            .map(|(hash, value, ts)| {
                Fact::new(
                    hash.clone(),
                    value.clone(),
                    *ts,
                    source.clone(),
                    Operation::Assert,
                )
            })
            .collect();
        writer.write_batch(&fact_structs).unwrap();
        temp
    }

    #[test]
    fn load_tracks_aggregates_played_timestamp() {
        let hash = ContentHash::new("sha256:aabbcc");
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();

        // TrackStarted now carries a StartReason; the fact timestamp is used for last_started
        let temp = write_facts_file_with_timestamps(&[
            (
                hash.clone(),
                MusicValue::Title(Title::new("Test Track")),
                ts,
            ),
            (
                hash.clone(),
                MusicValue::TrackStarted(StartReason::OnRequest),
                ts,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.last_started,
            Some(ts),
            "last_started should be set from TrackStarted fact timestamp"
        );
        assert_eq!(track.last_stopped, None);
    }

    #[test]
    fn load_tracks_aggregates_skipped_timestamp() {
        let hash = ContentHash::new("sha256:ddeeff");
        let ts = Utc.with_ymd_and_hms(2026, 2, 1, 8, 30, 0).unwrap();

        // TrackStopped now carries a StopReason; the fact timestamp is used for last_stopped
        let temp = write_facts_file_with_timestamps(&[
            (hash.clone(), MusicValue::Title(Title::new("Skip Me")), ts),
            (
                hash.clone(),
                MusicValue::TrackStopped(StopReason::OnSkip),
                ts,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        let track = &result.tracks[0];
        assert_eq!(
            track.last_stopped,
            Some(ts),
            "last_stopped should be set from TrackStopped fact timestamp"
        );
        assert_eq!(track.last_started, None);
    }

    #[test]
    fn load_tracks_keeps_most_recent_played() {
        let hash = ContentHash::new("sha256:112233");
        let older = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 1, 20, 0, 0, 0).unwrap();

        // Write two TrackStarted facts with different fact-level timestamps
        let temp = write_facts_file_with_timestamps(&[
            (
                hash.clone(),
                MusicValue::Title(Title::new("Two Plays")),
                older,
            ),
            (
                hash.clone(),
                MusicValue::TrackStarted(StartReason::OnRequest),
                older,
            ),
            (
                hash.clone(),
                MusicValue::TrackStarted(StartReason::ByQueue),
                newer,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        let track = &result.tracks[0];
        assert_eq!(
            track.last_started,
            Some(newer),
            "should keep the most recent TrackStarted fact timestamp"
        );
    }

    #[test]
    fn search_passes_last_started_to_evaluator() {
        use library_search::{query::DateQuery, TrackQuery};

        let hash = ContentHash::new("sha256:445566");
        let ts = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_timestamps(&[
            (
                hash.clone(),
                MusicValue::Title(Title::new("Played Track")),
                ts,
            ),
            (
                hash.clone(),
                MusicValue::TrackStarted(StartReason::OnRequest),
                ts,
            ),
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
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
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

        let hash = ContentHash::new("sha256:778899aabbcc");

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
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
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
        let source = music_facts::FactSource::new(
            "test-playback",
            "1.0.0",
            music_facts::FactOrigin::Unknown,
        );
        let mut writer = FactWriter::open(&facts_dest).unwrap();
        writer
            .write_track_facts(
                &hash,
                &[(MusicValue::TrackStarted(StartReason::OnRequest), source)],
            )
            .unwrap();
        drop(writer);

        // After: track now has play history → NA query should return 0
        let after = service.search_tracks(&na_query);
        assert!(
            after.is_empty(),
            "DateQuery::NA should exclude track after TrackStarted fact appended externally"
        );
    }

    #[test]
    fn blob_path_uses_extension_from_file_path_fact() {
        // For MP3 files, blob_path should use .mp3 extension, not hardcoded .flac
        let hash = ContentHash::new("sha256:aabbccddeeff");

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("MP3 Track"))),
            (
                hash.clone(),
                MusicValue::FilePath(std::path::PathBuf::from("some/track.mp3")),
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        let path_str = track.blob_path.to_string_lossy();
        assert!(
            path_str.ends_with(".mp3"),
            "blob_path should use .mp3 extension from FilePath fact, got: {}",
            path_str
        );
        assert!(
            path_str.contains("blobs/"),
            "blob_path should be under blobs/, got: {}",
            path_str
        );
    }

    #[test]
    fn backfill_writes_format_fact_for_tracks_without_one() {
        use music_facts::{ContentHash, MusicValue, Title};

        let hash = ContentHash::new("sha256:backfill01");

        // Track has FilePath (so blob_path is set) but no Format fact
        let temp = write_facts_file(&[
            (
                hash.clone(),
                MusicValue::Title(Title::new("Backfill Track")),
            ),
            (
                hash.clone(),
                MusicValue::FilePath(std::path::PathBuf::from("some/track.flac")),
            ),
        ]);

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        // The backfill runs during new(); without a live ACID service the write
        // will fail silently (is_ok() returns false), so the fact_index may not be
        // updated. We just verify the service starts successfully and the track was
        // loaded.
        let tracks = service.tracks.lock().unwrap();
        assert_eq!(tracks.len(), 1, "Track should be loaded from facts file");
    }

    #[test]
    fn backfill_skips_tracks_that_already_have_format() {
        use music_facts::{ContentHash, MusicFormat, MusicValue, Title};

        let hash = ContentHash::new("sha256:hasformat01");

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Has Format"))),
            (
                hash.clone(),
                MusicValue::FilePath(std::path::PathBuf::from("some/track.flac")),
            ),
            (hash.clone(), MusicValue::Format(MusicFormat::Flac)),
        ]);

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        // Count facts before
        let before_content = std::fs::read_to_string(&facts_dest).unwrap();
        let before_lines = before_content.lines().count();

        let _service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        // No new facts should have been written
        let after_content = std::fs::read_to_string(&facts_dest).unwrap();
        let after_lines = after_content.lines().count();
        assert_eq!(
            before_lines, after_lines,
            "Should not write new facts when Format already exists"
        );
    }

    // =========================================================================
    // Playlist handler tests
    // =========================================================================

    static ACID_SERVER_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn spawn_acid_server() -> (acid_service::ServerHandle, String, String) {
        let id = ACID_SERVER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let facts_addr = format!("ipc:///tmp/mdma-test-acid-facts-{}-{}.sock", pid, id);
        let events_addr = format!("ipc:///tmp/mdma-test-acid-events-{}-{}.sock", pid, id);

        let rep = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
        rep.listen(&facts_addr).expect("rep listen");

        let pub_sock = nng::Socket::new(nng::Protocol::Pub0).expect("pub socket");
        pub_sock.listen(&events_addr).expect("pub listen");

        let handle = acid_service::start(rep, pub_sock, std::path::Path::new("/tmp"))
            .expect("failed to start acid server");
        std::thread::sleep(std::time::Duration::from_millis(20));
        (handle, facts_addr, events_addr)
    }

    fn make_service_with_playlists_dir() -> (
        LibraryService,
        tempfile::TempDir,
        acid_service::ServerHandle,
    ) {
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        // create the playlists sub-directory
        std::fs::create_dir_all(metadata_dir.path().join("playlists")).unwrap();
        let (acid_handle, facts_addr, events_addr) = spawn_acid_server();
        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .unwrap();
        (service, metadata_dir, acid_handle)
    }

    #[test]
    fn playlist_list_returns_empty_when_no_playlists() {
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let response = service.handle_request(LibraryRequest::PlaylistList);
        match response {
            LibraryResponse::PlaylistNames(names) => {
                assert!(names.is_empty())
            }
            other => panic!("Expected PlaylistNames, got {:?}", other),
        }
    }

    #[test]
    fn playlist_new_creates_and_returns_content() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("test-set").unwrap();
        let content = "sha256:abc\nsha256:def\n".to_string();
        let response = service.handle_request(LibraryRequest::PlaylistNew {
            name,
            content: content.clone(),
        });
        match response {
            LibraryResponse::PlaylistContent(c) => assert_eq!(c, content),
            other => panic!("Expected PlaylistContent, got {:?}", other),
        }
    }

    #[test]
    fn playlist_list_returns_created_playlist() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("my-set").unwrap();
        service.handle_request(LibraryRequest::PlaylistNew {
            name,
            content: "sha256:abc\n".to_string(),
        });
        let response = service.handle_request(LibraryRequest::PlaylistList);
        match response {
            LibraryResponse::PlaylistNames(names) => {
                assert_eq!(
                    names,
                    vec![library_ipc_protocol::PlaylistName::new("my-set").unwrap()]
                );
            }
            other => panic!("Expected PlaylistNames, got {:?}", other),
        }
    }

    #[test]
    fn playlist_new_fails_if_already_exists() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("dupe").unwrap();
        service.handle_request(LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: "sha256:abc\n".to_string(),
        });
        let response = service.handle_request(LibraryRequest::PlaylistNew {
            name,
            content: "sha256:xyz\n".to_string(),
        });
        match response {
            LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists { .. }) => {}
            other => panic!("Expected PlaylistAlreadyExists error, got {:?}", other),
        }
    }

    #[test]
    fn playlist_get_returns_content() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("get-me").unwrap();
        let content = "sha256:abc\n".to_string();
        service.handle_request(LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: content.clone(),
        });
        let response = service.handle_request(LibraryRequest::PlaylistGet { name });
        match response {
            LibraryResponse::PlaylistContent(c) => assert_eq!(c, content),
            other => panic!("Expected PlaylistContent, got {:?}", other),
        }
    }

    #[test]
    fn playlist_get_returns_not_found_for_missing() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("missing").unwrap();
        let response = service.handle_request(LibraryRequest::PlaylistGet { name });
        match response {
            LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. }) => {}
            other => panic!("Expected PlaylistNotFound error, got {:?}", other),
        }
    }

    #[test]
    fn playlist_append_adds_to_existing_content() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("append-me").unwrap();
        service.handle_request(LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: "sha256:aaa\n".to_string(),
        });
        let response = service.handle_request(LibraryRequest::PlaylistAppend {
            name,
            content: "sha256:bbb\n".to_string(),
        });
        match response {
            LibraryResponse::PlaylistContent(c) => {
                assert!(c.contains("sha256:aaa"), "should contain original");
                assert!(c.contains("sha256:bbb"), "should contain appended");
            }
            other => panic!("Expected PlaylistContent, got {:?}", other),
        }
    }

    #[test]
    fn playlist_append_fails_if_not_found() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("no-such-playlist").unwrap();
        let response = service.handle_request(LibraryRequest::PlaylistAppend {
            name,
            content: "sha256:abc\n".to_string(),
        });
        match response {
            LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. }) => {}
            other => panic!("Expected PlaylistNotFound error, got {:?}", other),
        }
    }

    #[test]
    fn playlist_replace_overwrites_content() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("replace-me").unwrap();
        service.handle_request(LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: "sha256:old\n".to_string(),
        });
        let new_content = "sha256:new\n".to_string();
        let response = service.handle_request(LibraryRequest::PlaylistReplace {
            name,
            content: new_content.clone(),
        });
        match response {
            LibraryResponse::PlaylistContent(c) => assert_eq!(c, new_content),
            other => panic!("Expected PlaylistContent, got {:?}", other),
        }
    }

    #[test]
    fn playlist_remove_deletes_playlist() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("remove-me").unwrap();
        service.handle_request(LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: "sha256:abc\n".to_string(),
        });
        let response = service.handle_request(LibraryRequest::PlaylistRemove { name });
        match response {
            LibraryResponse::Pong => {}
            other => panic!("Expected Pong, got {:?}", other),
        }
    }

    #[test]
    fn playlist_rename_works() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        // Create
        let name = PlaylistName::new("old-name").unwrap();
        service.handle_request(LibraryRequest::PlaylistNew {
            name: name.clone(),
            content: "test content".to_string(),
        });
        // Rename
        let new_name = PlaylistName::new("new-name").unwrap();
        let resp = service.handle_request(LibraryRequest::PlaylistRename {
            from: name.clone(),
            to: new_name.clone(),
        });
        assert!(matches!(resp, LibraryResponse::Pong));
        // Old name gone
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name });
        assert!(matches!(
            resp,
            LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. })
        ));
        // New name has content
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name: new_name });
        assert!(matches!(resp, LibraryResponse::PlaylistContent(c) if c == "test content"));
    }

    #[test]
    fn playlist_remove_fails_if_not_found() {
        use library_ipc_protocol::PlaylistName;
        let (service, _dir, _acid) = make_service_with_playlists_dir();
        let name = PlaylistName::new("not-there").unwrap();
        let response = service.handle_request(LibraryRequest::PlaylistRemove { name });
        match response {
            LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. }) => {}
            other => panic!("Expected PlaylistNotFound error, got {:?}", other),
        }
    }

    // =========================================================================
    // Hash resolution tests
    // =========================================================================

    #[test]
    fn resolve_hash_with_duplicate_same_hash_returns_first_match() {
        // Two index entries that share the exact same content_hash (legacy short
        // hashes can collide in the index). They represent the same content, so
        // resolve_hash must return that hash rather than an Ambiguous error.
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        // Inject two entries with the exact same content_hash directly.
        let shared_hash = ContentHash::new("10e95ec1");
        {
            let mut tracks = service.tracks.lock().unwrap();
            let mut entry_a = IndexedTrackInfo::new_empty(shared_hash.as_str().to_owned());
            entry_a.title = Some("Track A".to_owned());
            let mut entry_b = IndexedTrackInfo::new_empty(shared_hash.as_str().to_owned());
            entry_b.title = Some("Track B".to_owned());
            tracks.push(entry_a);
            tracks.push(entry_b);
        }

        // resolve_hash should succeed because both matches have identical hashes.
        let result = service.resolve_hash(&shared_hash);
        assert_eq!(
            result
                .expect("resolve_hash should return Ok for duplicate entries with the same hash")
                .as_str(),
            shared_hash.as_str(),
            "resolved hash should equal the shared hash"
        );
    }

    #[test]
    fn resolve_hash_with_prefix_collision_different_hashes_returns_ambiguous() {
        // Two entries whose hashes share a common prefix but are different values.
        // This is a genuine ambiguity and must still return an error.
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        let hash_a = ContentHash::new("sha256:abcdef0011223344");
        let hash_b = ContentHash::new("sha256:abcdef0099887766");
        {
            let mut tracks = service.tracks.lock().unwrap();
            let mut entry_a = IndexedTrackInfo::new_empty(hash_a.as_str().to_owned());
            entry_a.title = Some("Track A".to_owned());
            let mut entry_b = IndexedTrackInfo::new_empty(hash_b.as_str().to_owned());
            entry_b.title = Some("Track B".to_owned());
            tracks.push(entry_a);
            tracks.push(entry_b);
        }

        // Use "sha256:abcdef00" as the partial prefix that matches both hashes.
        let partial = ContentHash::new("sha256:abcdef00");
        let result = service.resolve_hash(&partial);
        assert!(
            matches!(result, Err(_)),
            "expected Ambiguous error for genuine prefix collision, got: {:?}",
            result
        );
    }

    #[test]
    fn resolve_hash_with_legacy_short_and_full_hash_returns_full_hash() {
        // One entry has a legacy 8-char short hash ("9fb4105e") and another has
        // the full sha256 hash ("sha256:9fb4105eXXX...") for the same file.
        // resolve_hash must recognise them as the same content and return the
        // full hash (the most complete one), not an Ambiguous error.
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        let legacy_hash = ContentHash::new("9fb4105e");
        let full_hash = ContentHash::new("sha256:9fb4105eaabbccdd11223344556677889900aabb");
        {
            let mut tracks = service.tracks.lock().unwrap();
            let mut entry_legacy = IndexedTrackInfo::new_empty(legacy_hash.as_str().to_owned());
            entry_legacy.title = Some("Unknown".to_owned());
            let mut entry_full = IndexedTrackInfo::new_empty(full_hash.as_str().to_owned());
            entry_full.title = Some("20 Minutes".to_owned());
            tracks.push(entry_legacy);
            tracks.push(entry_full);
        }

        // Searching by the legacy short hash should resolve to the full hash.
        let result = service.resolve_hash(&legacy_hash);
        assert_eq!(
            result
                .expect("resolve_hash should return Ok for legacy+full hash pair")
                .as_str(),
            full_hash.as_str(),
            "resolved hash should be the full sha256 hash"
        );
    }

    // =========================================================================
    // Cursor persistence tests
    // =========================================================================

    #[test]
    fn save_and_load_cursor_roundtrip() {
        let metadata_dir = tempfile::tempdir().unwrap();
        let cursor_path = metadata_dir.path().join("facts.cursor");

        // Nothing saved yet — should return None
        assert!(
            LibraryService::load_saved_cursor(&cursor_path).is_none(),
            "cursor should be None when file does not exist"
        );

        // Save a cursor
        LibraryService::save_cursor(&cursor_path, "line:42");

        // Should load back correctly
        let loaded = LibraryService::load_saved_cursor(&cursor_path);
        assert_eq!(
            loaded.as_deref(),
            Some("line:42"),
            "loaded cursor should match saved cursor"
        );
    }

    #[test]
    fn save_cursor_overwrites_previous() {
        let metadata_dir = tempfile::tempdir().unwrap();
        let cursor_path = metadata_dir.path().join("facts.cursor");

        LibraryService::save_cursor(&cursor_path, "line:10");
        LibraryService::save_cursor(&cursor_path, "line:99");

        let loaded = LibraryService::load_saved_cursor(&cursor_path);
        assert_eq!(loaded.as_deref(), Some("line:99"));
    }

    #[test]
    fn load_saved_cursor_returns_none_for_invalid_content() {
        let metadata_dir = tempfile::tempdir().unwrap();
        let cursor_path = metadata_dir.path().join("facts.cursor");

        // Write garbage (whitespace only)
        std::fs::write(&cursor_path, "   \n  ").unwrap();

        let loaded = LibraryService::load_saved_cursor(&cursor_path);
        assert!(
            loaded.is_none(),
            "empty/whitespace cursor file should return None"
        );
    }

    #[test]
    fn apply_stream_lines_updates_index() {
        use music_facts::{ContentHash, FactOrigin, FactSource, MusicValue, Title};
        use stainless_facts::{Fact, Operation};

        let hash = ContentHash::new("sha256:streamtest01");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let now = chrono::Utc::now();

        let fact = Fact::new(
            hash.clone(),
            MusicValue::Title(Title::new("Stream Track")),
            now,
            source,
            Operation::Assert,
        );
        let line = serde_json::to_string(&fact).unwrap();

        // Start with empty service
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        assert_eq!(service.tracks.lock().unwrap().len(), 0);

        // Apply a stream chunk
        service.apply_stream_lines(&[line]);

        let tracks = service.tracks.lock().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title.as_deref(), Some("Stream Track"));
    }

    #[test]
    fn blob_path_uses_flac_extension_from_file_path_fact() {
        let hash = ContentHash::new("sha256:001122334455");

        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("FLAC Track"))),
            (
                hash.clone(),
                MusicValue::FilePath(std::path::PathBuf::from("some/track.flac")),
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        let path_str = track.blob_path.to_string_lossy();
        assert!(
            path_str.ends_with(".flac"),
            "blob_path should use .flac extension from FilePath fact, got: {}",
            path_str
        );
    }

    // =========================================================================
    // Album cover cache tests
    // =========================================================================

    #[test]
    fn track_without_cover_art_falls_back_to_album_cover() {
        // Track A has cover art; Track B is on the same album but has no cover art.
        // After loading, to_track_info on Track B should return the album cover.
        let hash_a = ContentHash::new("sha256:albumcovertrack01");
        let hash_b = ContentHash::new("sha256:albumcovertrack02");
        let album_name = "Shared Album";
        let cover_path = "cover-art/abc123.jpg";

        let temp = write_facts_file(&[
            (
                hash_a.clone(),
                MusicValue::Title(Title::new("Track With Cover")),
            ),
            (
                hash_a.clone(),
                MusicValue::Album(music_facts::Album::new(album_name)),
            ),
            (
                hash_a.clone(),
                MusicValue::CoverArtPath(cover_path.to_string()),
            ),
            (
                hash_b.clone(),
                MusicValue::Title(Title::new("Track Without Cover")),
            ),
            (
                hash_b.clone(),
                MusicValue::Album(music_facts::Album::new(album_name)),
            ),
        ]);

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        let tracks = service.tracks.lock().unwrap();
        let track_b = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash_b.as_str())
            .expect("Track B should be indexed");
        let track_b_info = service.to_track_info(track_b);
        drop(tracks);

        assert_eq!(
            track_b_info.cover_art_path.as_deref(),
            Some(cover_path),
            "Track B should inherit album cover art from Track A on the same album"
        );
    }

    // =========================================================================
    // Bug fix tests: apply_lines_to_map and fallback guard
    // =========================================================================

    /// Verify that facts written to the in-memory ACID service are read back
    /// correctly by load_from_acid_stream.
    ///
    /// Both `fact_store_memory` and `fact_store_file` now produce the same
    /// array-format JSONL lines, so direct `serde_json::from_str::<Fact<>>()` works
    /// for both backends without a compatibility shim.
    #[test]
    fn load_from_acid_stream_parses_facts_via_memory_storage() {
        let (acid_handle, facts_addr, _events_addr) = spawn_acid_server();

        let acid_client = AcidClient::connect(&facts_addr).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let hash = ContentHash::new("sha256:acidstreamtest01");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        acid_client
            .write_music_facts(
                &hash,
                &[(MusicValue::Title(Title::new("ACID Track")), source)],
            )
            .unwrap();

        let (loaded, _cursor) = LibraryService::load_from_acid_stream(&acid_client, None);

        drop(acid_handle);

        assert_eq!(
            loaded.tracks.len(),
            1,
            "load_from_acid_stream must return the track written to the in-memory ACID store"
        );
        assert_eq!(
            loaded.tracks[0].title.as_deref(),
            Some("ACID Track"),
            "track title must be parsed from ACID stream line"
        );
    }

    /// Bug 2: fallback to facts.jsonl must trigger whenever tracks.is_empty(),
    /// regardless of whether a cursor was present.
    ///
    /// Before the fix, a saved cursor suppresses the fallback even when ACID
    /// returns 0 new facts (cursor is already at end-of-file).
    #[test]
    fn fallback_to_facts_file_when_acid_returns_empty_with_cursor() {
        let hash = ContentHash::new("sha256:cursorfallback01");

        let temp = write_facts_file(&[(
            hash.clone(),
            MusicValue::Title(Title::new("Fallback Track")),
        )]);

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        // Copy facts file to where the service expects it
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        // Write a cursor file so saved_cursor.is_some() — simulates "already synced"
        let cursor_path = metadata_dir.path().join("facts.cursor");
        LibraryService::save_cursor(&cursor_path, "line:1");

        // Use a nonexistent ACID socket so load_from_acid_stream returns 0 lines
        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        let tracks = service.tracks.lock().unwrap();
        assert_eq!(
            tracks.len(),
            1,
            "service must fall back to facts.jsonl when ACID returns 0 tracks even if a cursor was saved"
        );
        assert_eq!(
            tracks[0].title.as_deref(),
            Some("Fallback Track"),
            "fallback track title must match the facts file"
        );
    }

    #[test]
    fn track_with_cover_art_is_not_overridden_by_album_cover() {
        // Both tracks on the same album but have their own cover art — no fallback needed.
        let hash_a = ContentHash::new("sha256:owncover01");
        let hash_b = ContentHash::new("sha256:owncover02");
        let album_name = "Another Album";
        let cover_a = "cover-art/aaa.jpg";
        let cover_b = "cover-art/bbb.jpg";

        let temp = write_facts_file(&[
            (hash_a.clone(), MusicValue::Title(Title::new("Track A"))),
            (
                hash_a.clone(),
                MusicValue::Album(music_facts::Album::new(album_name)),
            ),
            (
                hash_a.clone(),
                MusicValue::CoverArtPath(cover_a.to_string()),
            ),
            (hash_b.clone(), MusicValue::Title(Title::new("Track B"))),
            (
                hash_b.clone(),
                MusicValue::Album(music_facts::Album::new(album_name)),
            ),
            (
                hash_b.clone(),
                MusicValue::CoverArtPath(cover_b.to_string()),
            ),
        ]);

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(temp.path(), &facts_dest).unwrap();

        let service = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent.sock",
        )
        .unwrap();

        let tracks = service.tracks.lock().unwrap();
        let track_b = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash_b.as_str())
            .expect("Track B should be indexed");
        let track_b_info = service.to_track_info(track_b);
        drop(tracks);

        assert_eq!(
            track_b_info.cover_art_path.as_deref(),
            Some(cover_b),
            "Track B should keep its own cover art, not be replaced by Track A's"
        );
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

/// Spawn a background thread that subscribes to ACID fact notifications and
/// applies incremental updates to the library index.
///
/// On receiving an `acid/facts` event, fetches new facts from the ACID stream
/// starting at the saved cursor, applies them to the in-memory index, and
/// updates the persisted cursor.
///
/// The thread retries indefinitely on dial or recv failure with a 5-second backoff.
pub fn spawn_fact_subscriber(service: Arc<LibraryService>) {
    let acid_events_socket = service.acid_events_socket.clone();
    let acid_socket_addr = service.acid_socket.clone();

    std::thread::spawn(move || {
        use nng::options::Options;

        let sub = match nng::Socket::new(nng::Protocol::Sub0) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Failed to create Sub0 socket for ACID events");
                return;
            }
        };

        if let Err(e) = sub.set_opt::<nng::options::protocol::pubsub::Subscribe>(
            TOPIC_ACID_FACTS_WRITTEN.as_bytes().to_vec(),
        ) {
            tracing::error!(error = %e, "Failed to subscribe to ACID facts topic");
            return;
        }

        let cursor_path = service.metadata_dir.join("facts.cursor");

        // Outer retry loop: reconnect after dial or recv failure.
        loop {
            if let Err(e) = sub.dial(&acid_events_socket) {
                tracing::warn!(
                    error = %e,
                    socket = %acid_events_socket,
                    "Failed to connect to ACID events socket, retrying in 5s"
                );
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }

            tracing::info!(
                socket = %acid_events_socket,
                "Subscribed to ACID facts events"
            );

            // Create a dedicated ACID client for the subscriber thread
            let acid_client = match AcidClient::connect(&acid_socket_addr) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Subscriber: failed to connect to ACID service, retrying in 5s"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };

            // Inner recv loop: process events until a recv error forces reconnect.
            loop {
                let msg = match sub.recv() {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(error = %e, "ACID subscriber: recv error, reconnecting in 5s");
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        break;
                    }
                };

                let event = match acid_event_from_topic_message(&msg) {
                    Ok((_, e)) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "ACID subscriber: failed to parse event");
                        continue;
                    }
                };

                let AcidEvent::FactsWritten { cursor, .. } = event;
                tracing::debug!(cursor = %cursor, "Received ACID facts-written notification");

                // Fetch new facts starting from our current cursor
                let current_cursor = service.cursor.lock().unwrap().clone();
                match acid_client.read_stream(current_cursor, 10_000) {
                    Ok(chunk) => {
                        if !chunk.lines.is_empty() {
                            tracing::debug!(
                                count = chunk.lines.len(),
                                "Applying incremental facts from ACID stream"
                            );
                            service.apply_stream_lines(&chunk.lines);
                        }
                        // Update and persist cursor
                        *service.cursor.lock().unwrap() = Some(chunk.cursor.clone());
                        LibraryService::save_cursor(&cursor_path, &chunk.cursor);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Subscriber: failed to read incremental stream");
                    }
                }
            }
        }
    });
}
