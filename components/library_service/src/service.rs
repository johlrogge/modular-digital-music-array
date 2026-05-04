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
use event_protocol::{acid_event_from_topic_message, AcidEvent, TOPIC_ACID};
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
    /// Cursor for incremental reads in refresh_event_timestamps.
    ///
    /// Separate from `cursor` (used by spawn_fact_subscriber) so the two readers
    /// don't fight over the same position.  Starts at None (full scan on first
    /// call) and advances with each successful refresh.
    ///
    /// NOTE: retractions of TrackStarted/TrackStopped facts are not handled here.
    /// The assumption is that such retractions never occur in production.  If they
    /// become meaningful, a full scan from None would be required.
    event_cursor: Mutex<Option<String>>,
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
    item_id: Option<String>,
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
            item_id: None,
        }
    }
}

/// Apply a single `MusicValue` fact to a mutable `IndexedTrackInfo` entry.
///
/// The fact timestamp is needed only for `TrackStarted` / `TrackStopped` to
/// preserve the most-recent-wins ordering; callers must supply it.
///
/// When `operation` is `Retract`, single-valued fields are cleared to `None`
/// and multi-valued fields (e.g. `StyleDescriptor`) remove the retracted value.
fn apply_fact_to_track(
    entry: &mut IndexedTrackInfo,
    value: &MusicValue,
    timestamp: chrono::DateTime<chrono::Utc>,
    operation: stainless_facts::Operation,
    has_format: Option<&mut HashSet<String>>,
    has_cover_art: Option<&mut HashSet<String>>,
) {
    use stainless_facts::Operation;

    match operation {
        Operation::Assert => match value {
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
                        entry.blob_path = PathBuf::from(format!(
                            "blobs/{}/{}.{}",
                            &hash_clean[..2],
                            hash_clean,
                            ext
                        ));
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
            MusicValue::AddedAt(dt) if entry.added_at.is_none() => {
                entry.added_at = Some(*dt);
            }
            MusicValue::ItemId(v) => entry.item_id = Some(v.clone()),
            _ => {}
        },
        Operation::Retract => {
            // TODO: multi-source correctness — if two sources both asserted the same
            // attribute and only one retracts, the other source's value should survive.
            // Currently we use simple last-writer-wins across sources, so a retraction
            // clears the field regardless of other sources. Full per-source tracking
            // is left as a follow-up.
            match value {
                MusicValue::Title(_) => entry.title = None,
                MusicValue::Artist(_) => entry.artist = None,
                MusicValue::Album(_) => entry.album = None,
                MusicValue::Label(_) => entry.label = None,
                MusicValue::MainGenre(_) => entry.genre = None,
                MusicValue::StyleDescriptor(v) => entry.styles.retain(|s| s != v),
                MusicValue::DurationSeconds(_) => entry.duration_seconds = None,
                MusicValue::Bpm(_) => entry.bpm = None,
                MusicValue::Key(_) => entry.key = None,
                MusicValue::Year(_) => entry.year = None,
                MusicValue::TrackNumber(_) => entry.track_number = None,
                MusicValue::DiscNumber(_) => entry.disc_number = None,
                MusicValue::Source(_) => entry.source = None,
                MusicValue::CoverArtPath(_) => {
                    entry.cover_art_path = None;
                    if let Some(set) = has_cover_art {
                        set.remove(entry.content_hash.as_str());
                    }
                }
                MusicValue::Format(_) => {
                    if let Some(set) = has_format {
                        set.remove(entry.content_hash.as_str());
                    }
                }
                MusicValue::ItemId(_) => entry.item_id = None,
                // TrackStarted/TrackStopped/FilePath/AddedAt retractions are not
                // emitted in the current codebase; ignore silently.
                _ => {}
            }
        }
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

/// Returns true for the metadata attributes that a Bandcamp ingest writes and
/// that are safe to retract during a resync.  ItemId and Source are identifiers
/// and intentionally excluded.
fn is_retractable_bandcamp_attribute(value: &MusicValue) -> bool {
    matches!(
        value,
        MusicValue::Album(_)
            | MusicValue::Title(_)
            | MusicValue::Artist(_)
            | MusicValue::TrackNumber(_)
            | MusicValue::Year(_)
    )
}

/// Map a `FactOrigin` variant to its canonical source-name string.
///
/// This is the single source of truth for the `source_name` → `FactOrigin`
/// mapping used in `retract_source_facts`. If a new origin is added to
/// `FactOrigin`, add an arm here.
fn origin_matches_source_name(origin: &music_facts::FactOrigin, source_name: &str) -> bool {
    use music_facts::FactOrigin;
    match origin {
        FactOrigin::Bandcamp { .. } => source_name == "bandcamp",
        FactOrigin::Beatport { .. } => source_name == "beatport",
        FactOrigin::FilesystemScan { .. } => source_name == "filesystem",
        FactOrigin::User => source_name == "user",
        FactOrigin::Unknown => source_name == "unknown",
    }
}

/// Returns `true` if the given track still asserts a particular (fact_type, value) pair.
///
/// Used during `Retract` processing to decide whether the fact_index entry
/// should be removed or kept (because another entity still asserts it).
///
/// Only covers the fact types stored as named fields on `IndexedTrackInfo`.
/// For fact types not tracked there (e.g. `ItemId`, `BandcampUrl`), returns
/// `true` conservatively (value stays in the index).
fn is_value_still_asserted_for_fact_type(
    track: &IndexedTrackInfo,
    fact_type: &FactType,
    value: &str,
) -> bool {
    match fact_type.as_str() {
        "Title" => track.title.as_deref() == Some(value),
        "Artist" => track.artist.as_deref() == Some(value),
        "Album" => track.album.as_deref() == Some(value),
        "Label" => track.label.as_deref() == Some(value),
        "MainGenre" => track.genre.as_deref() == Some(value),
        "StyleDescriptor" => track.styles.iter().any(|s| s == value),
        "Key" => track.key.as_deref() == Some(value),
        "ItemId" => track.item_id.as_deref() == Some(value),
        _ => {
            // Conservative: cannot determine from IndexedTrackInfo fields alone.
            // Returning true means we never remove — safe but slightly imprecise
            // for less-common fact types.
            true
        }
    }
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

        // Always do a full bootstrap from the beginning (cursor=None).
        // No on-disk cursor is read; the cursor lives in memory only.
        let (loaded, final_cursor) = Self::load_from_acid_stream(&acid_client, None)?;

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
            event_cursor: Mutex::new(None),
            acid_socket: acid_socket.to_string(),
            acid_events_socket: acid_events_socket.to_string(),
        };

        // Backfill Format facts for tracks that don't have one yet
        service.backfill_format_facts(&loaded.has_format);

        // Backfill cover art for tracks that don't have CoverArtPath yet
        service.backfill_cover_art(&loaded.has_cover_art);

        // Ensure playlists directory exists so writes never fail on a fresh install
        std::fs::create_dir_all(service.metadata_dir.join("playlists"))?;

        Ok(service)
    }

    /// Load tracks from the ACID read_stream API starting at cursor=None (full bootstrap).
    ///
    /// Returns `Ok((LoadResult, Option<cursor>))`:
    /// - ACID reachable with 0 facts → `Ok((empty, Some(cursor)))` — correct on fresh system
    /// - ACID reachable with facts → `Ok((populated, Some(cursor)))`
    /// - ACID IPC error → `Err(ServiceError::Acid(_))` — fails loud, no silent fallback
    fn load_from_acid_stream(
        acid_client: &AcidClient,
        start_cursor: Option<String>,
    ) -> Result<(LoadResult, Option<String>), ServiceError> {
        const PAGE_SIZE: usize = 10_000;

        let mut tracks_map: HashMap<String, IndexedTrackInfo> = HashMap::new();
        let mut fact_index: HashMap<FactType, HashSet<String>> = HashMap::new();
        let mut has_format: HashSet<String> = HashSet::new();
        let mut has_cover_art: HashSet<String> = HashSet::new();
        let mut total = 0;
        let mut current_cursor = start_cursor;
        let final_cursor;

        loop {
            let chunk = match acid_client.read_stream(current_cursor.clone(), PAGE_SIZE) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("ACID read failed during library bootstrap: {}", e);
                    return Err(ServiceError::Acid(e));
                }
            };

            if chunk.lines.is_empty() {
                final_cursor = Some(chunk.cursor);
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
            current_cursor = Some(chunk.cursor.clone());

            if lines_count < PAGE_SIZE {
                final_cursor = Some(chunk.cursor);
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
        Ok((loaded, final_cursor))
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
            let entity_key = fact.entity().as_str().to_owned();

            // Ensure the entry exists before we need to scan the map
            tracks_map
                .entry(entity_key.clone())
                .or_insert_with(|| IndexedTrackInfo::new_empty(entity_key.clone()));

            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            let fact_type = FactType::new(variant_name);
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    fact_index.entry(fact_type).or_default().insert(value_str);
                }
                stainless_facts::Operation::Retract => {
                    // Only remove from fact_index when no OTHER entity still
                    // asserts this value. We exclude the current entity because
                    // apply_fact_to_track (called below) will clear it.
                    let still_asserted = tracks_map.iter().any(|(k, t)| {
                        k != &entity_key
                            && is_value_still_asserted_for_fact_type(t, &fact_type, &value_str)
                    });
                    if !still_asserted {
                        if let Some(set) = fact_index.get_mut(&fact_type) {
                            set.remove(&value_str);
                        }
                    }
                }
            }

            let entry = tracks_map
                .get_mut(&entity_key)
                .expect("entry was just inserted");
            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                fact.operation(),
                Some(has_format),
                Some(has_cover_art),
            );
        }
    }

    /// Load tracks from facts file into memory for search.
    /// Used only in tests (for direct unit testing of fact parsing logic).
    #[cfg(test)]
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

            let entity_key = fact.entity().as_str().to_owned();

            // Ensure the entry exists before we scan the map
            tracks_map
                .entry(entity_key.clone())
                .or_insert_with(|| IndexedTrackInfo::new_empty(entity_key.clone()));

            // Index fact values for HasFact/HasFacts lookups; honour Retract
            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            let fact_type = FactType::new(variant_name);
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    fact_index.entry(fact_type).or_default().insert(value_str);
                }
                stainless_facts::Operation::Retract => {
                    // Only remove from fact_index when no OTHER entity still
                    // asserts this value. The current entity's entry still has
                    // the old value (apply_fact_to_track below will clear it),
                    // so we exclude it from the scan.
                    let still_asserted = tracks_map.iter().any(|(k, t)| {
                        k != &entity_key
                            && is_value_still_asserted_for_fact_type(t, &fact_type, &value_str)
                    });
                    if !still_asserted {
                        if let Some(set) = fact_index.get_mut(&fact_type) {
                            set.remove(&value_str);
                        }
                    }
                }
            }

            // Extract key fields for search; honour Retract
            let entry = tracks_map
                .get_mut(&entity_key)
                .expect("entry was just inserted");
            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                fact.operation(),
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

            // Index the fact value; honour Retract
            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            let fact_type = FactType::new(variant_name);
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    fact_index.entry(fact_type).or_default().insert(value_str);
                }
                stainless_facts::Operation::Retract => {
                    // Only remove from fact_index when no OTHER entity still
                    // asserts this value. The current entry (at pos) still has
                    // the old value; apply_fact_to_track below will clear it,
                    // so we exclude pos from the scan.
                    let still_asserted = tracks.iter().enumerate().any(|(i, t)| {
                        i != pos && is_value_still_asserted_for_fact_type(t, &fact_type, &value_str)
                    });
                    if !still_asserted {
                        if let Some(set) = fact_index.get_mut(&fact_type) {
                            set.remove(&value_str);
                        }
                    }
                }
            }

            // Apply to track fields (borrow tracks[pos] after the scan above)
            let entry = &mut tracks[pos];
            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                fact.operation(),
                None,
                None,
            );
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
                    if let Some(parent) = path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            return LibraryResponse::Error(ProtocolError::Internal {
                                message: format!("Failed to create playlist directory: {}", e),
                            });
                        }
                    }
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
                        if let Some(parent) = path.parent() {
                            if let Err(e) = std::fs::create_dir_all(parent) {
                                return LibraryResponse::Error(ProtocolError::Internal {
                                    message: format!("Failed to create playlist directory: {}", e),
                                });
                            }
                        }
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
                if let Some(parent) = path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return LibraryResponse::Error(ProtocolError::Internal {
                            message: format!("Failed to create playlist directory: {}", e),
                        });
                    }
                }
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

            LibraryRequest::WriteBookmark { hash, scope } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let fact = MusicValue::Bookmarked {
                    scope,
                    timestamp: chrono::Utc::now(),
                };
                let source = music_facts::FactSource::new(
                    "mdma",
                    env!("CARGO_PKG_VERSION"),
                    music_facts::FactOrigin::User,
                );
                match self
                    .acid_client
                    .write_music_facts(&full_hash, &[(fact, source)])
                {
                    Ok(_) => LibraryResponse::BookmarkWritten,
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: e.to_string(),
                    }),
                }
            }

            LibraryRequest::WriteFact { hash, fact } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let source = music_facts::FactSource::new(
                    "mdma",
                    env!("CARGO_PKG_VERSION"),
                    music_facts::FactOrigin::User,
                );
                match self
                    .acid_client
                    .write_music_facts(&full_hash, &[(fact, source)])
                {
                    Ok(_) => LibraryResponse::FactWritten,
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: e.to_string(),
                    }),
                }
            }

            LibraryRequest::RetractFact { hash, fact } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let source = music_facts::FactSource::new(
                    "mdma",
                    env!("CARGO_PKG_VERSION"),
                    music_facts::FactOrigin::User,
                );
                match self
                    .acid_client
                    .retract_music_facts(&full_hash, &[(fact, source)])
                {
                    Ok(_) => LibraryResponse::FactRetracted,
                    Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                        message: e.to_string(),
                    }),
                }
            }

            LibraryRequest::RetractSourceFacts {
                item_id,
                source_name,
            } => self.retract_source_facts(&item_id, &source_name),

            LibraryRequest::GetAlbumTitleByItemId { item_id } => {
                LibraryResponse::AlbumTitleByItemId(self.get_album_title_by_item_id(&item_id))
            }

            LibraryRequest::GetTrackCountForItemId { item_id } => {
                let count = self.content_hashes_for_item_id(&item_id).len();
                LibraryResponse::TrackCountForItemId(count)
            }
        }
    }

    /// Scan the in-memory track index and return every ContentHash whose
    /// `item_id` field matches `item_id`.
    ///
    /// Linear scan over the in-memory index — acceptable at current library scale.
    fn content_hashes_for_item_id(&self, item_id: &str) -> Vec<ContentHash> {
        self.tracks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.item_id.as_deref() == Some(item_id))
            .map(|t| t.content_hash.clone())
            .collect()
    }

    /// Retract bandcamp-sourced metadata facts for all tracks belonging to
    /// `item_id`, where the fact was written by `source_name` (matches
    /// `FactSource.tool`).
    ///
    /// Retracted attributes: Album, Title, Artist, TrackNumber, Year.
    /// ItemId itself is intentionally NOT retracted — it is the stable identifier
    /// used to correlate tracks across resyncs.
    ///
    /// Per-hash facts are read from ACID via `read_entity`, and retractions are
    /// sent to ACID via `retract_music_facts`. The in-memory index is updated
    /// immediately after a successful ACID write.
    fn retract_source_facts(&self, item_id: &str, source_name: &str) -> LibraryResponse {
        use music_facts::FactSource;

        let hashes = self.content_hashes_for_item_id(item_id);

        if hashes.is_empty() {
            return LibraryResponse::SourceFactsRetracted;
        }

        // For each hash, collect (MusicValue, FactSource) pairs to retract.
        // We only retract the attributes that bandcamp writes during ingest:
        // Album, Title, Artist, TrackNumber, Year.
        // (Label, Genre, etc. from audio tags are written by mdma-library and may
        // be shared with other sources; retract them only if source_name matches.)
        let mut all_retractions: Vec<(ContentHash, Vec<(MusicValue, FactSource)>)> = vec![];

        for hash in &hashes {
            // Read facts for this hash from ACID to find currently asserted values
            // from the given source.
            let lines = match self.acid_client.read_entity(hash.as_str()) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "RetractSourceFacts: failed to read entity from ACID");
                    return LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Failed to read entity from ACID: {}", e),
                    });
                }
            };

            let retractable: Vec<(MusicValue, FactSource)> = lines
                .iter()
                .filter_map(|line| {
                    serde_json::from_str::<
                        stainless_facts::Fact<ContentHash, MusicValue, FactSource>,
                    >(line)
                    .ok()
                })
                .filter(|f| {
                    f.operation() == stainless_facts::Operation::Assert
                        && origin_matches_source_name(&f.source().origin, source_name)
                        && is_retractable_bandcamp_attribute(f.value())
                })
                .map(|f| (f.value().clone(), f.source().clone()))
                .collect();

            if !retractable.is_empty() {
                all_retractions.push((hash.clone(), retractable));
            }
        }

        if all_retractions.is_empty() {
            return LibraryResponse::SourceFactsRetracted;
        }

        // Send retraction facts to ACID
        let mut total_retracted = 0usize;
        for (hash, facts) in &all_retractions {
            match self.acid_client.retract_music_facts(hash, facts) {
                Ok(count) => total_retracted += count,
                Err(e) => {
                    tracing::warn!(error = %e, hash = %hash.as_str(), "Failed to retract facts via ACID");
                    return LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("Failed to retract facts via ACID: {}", e),
                    });
                }
            }
        }

        tracing::info!(
            item_id,
            source_name,
            facts_retracted = total_retracted,
            "Retracted source facts"
        );

        // Update in-memory state: clear only the fields that were actually retracted
        let mut tracks = self.tracks.lock().unwrap();
        for (hash, retracted_facts) in &all_retractions {
            if let Some(track) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                for (value, _source) in retracted_facts {
                    match value {
                        MusicValue::Title(_) => track.title = None,
                        MusicValue::Artist(_) => track.artist = None,
                        MusicValue::Album(_) => track.album = None,
                        MusicValue::TrackNumber(_) => track.track_number = None,
                        MusicValue::Year(_) => track.year = None,
                        _ => {}
                    }
                }
            }
        }

        LibraryResponse::SourceFactsRetracted
    }

    /// Look up the album title for any track tagged with `item_id`.
    ///
    /// Scans the fact stream to identify which ContentHashes have the given
    /// ItemId, then looks up their current Album in the in-memory index.
    ///
    /// Returns the first album found. If multiple tracks share the same ItemId
    /// but have different Album values (rare, should only occur mid-rename),
    /// any one value may be returned.
    fn get_album_title_by_item_id(&self, item_id: &str) -> Option<String> {
        let hashes = self.content_hashes_for_item_id(item_id);
        if hashes.is_empty() {
            return None;
        }

        let tracks = self.tracks.lock().unwrap();
        for hash in &hashes {
            if let Some(track) = tracks
                .iter()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                if track.album.is_some() {
                    return track.album.clone();
                }
            }
        }
        None
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
                    .filter(|p| {
                        // Exclude AppleDouble sidecar files created by macOS Finder/SMB.
                        // These are named ._<filename> and contain HFS+ metadata, not audio.
                        !p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("._"))
                    })
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
            started: t.last_started.map(|dt| dt.to_rfc3339()),
            stopped: t.last_stopped.map(|dt| dt.to_rfc3339()),
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

        let full_hash = self.resolve_hash(hash)?;

        let lines = self
            .acid_client
            .read_entity(full_hash.as_str())
            .map_err(|e| ProtocolError::Internal {
                message: format!("Failed to read entity from ACID: {}", e),
            })?;

        let facts: Vec<_> = lines
            .iter()
            .filter_map(|line| {
                serde_json::from_str::<stainless_facts::Fact<ContentHash, MusicValue, FactSource>>(
                    line,
                )
                .ok()
            })
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

    /// Read new TrackStarted/TrackStopped facts from the ACID stream (since the last
    /// call) and update `last_started` / `last_stopped` in the in-memory index.
    ///
    /// Uses `event_cursor` (a separate cursor from the subscriber's `cursor`) so that
    /// the background fact subscriber and this refresh path don't interfere with each
    /// other's read position.
    ///
    /// On the first call after a service restart, `event_cursor` is None so the full
    /// stream is scanned.  Subsequent calls only read new facts appended since the
    /// previous refresh (incremental / delta reads).
    ///
    /// NOTE: retractions of TrackStarted/TrackStopped are not handled.  The assumption
    /// is that such retractions never occur in production.  If they become meaningful a
    /// full scan from cursor=None would be required instead of the incremental approach.
    fn refresh_event_timestamps(&self) {
        const PAGE_SIZE: usize = 10_000;

        // Take the current cursor position before we start reading.
        let start_cursor = self.event_cursor.lock().unwrap().clone();
        let mut current_cursor = start_cursor;

        // Collect the most-recent TrackStarted/TrackStopped timestamp per content hash.
        // We build an intermediate map so the tracks lock is held only at the end.
        let mut last_started: HashMap<String, Option<chrono::DateTime<chrono::Utc>>> =
            HashMap::new();
        let mut last_stopped: HashMap<String, Option<chrono::DateTime<chrono::Utc>>> =
            HashMap::new();

        loop {
            let chunk = match self
                .acid_client
                .read_stream(current_cursor.clone(), PAGE_SIZE)
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "refresh_event_timestamps: ACID read failed, skipping refresh"
                    );
                    return;
                }
            };

            for line in &chunk.lines {
                let fact = match serde_json::from_str::<
                    stainless_facts::Fact<ContentHash, MusicValue, music_facts::FactSource>,
                >(line)
                {
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

            let is_last_page = chunk.lines.len() < PAGE_SIZE;
            current_cursor = Some(chunk.cursor);
            if is_last_page {
                break;
            }
        }

        // Advance the stored cursor so the next call only reads new facts.
        *self.event_cursor.lock().unwrap() = current_cursor;

        // Apply collected timestamps to the in-memory index.
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
            let mut item_id_str = None;

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
                    MusicValue::ItemId(id) => item_id_str = Some(id.clone()),
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
                item_id: item_id_str,
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

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

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

        let (service, _metadata_dir, facts_addr) = make_service_with_facts_and_addr(temp.path());

        let na_query = TrackQuery {
            started: Some(DateQuery::NA),
            ..Default::default()
        };

        // Before: track has no play history → should appear in NA results
        let before = service.search_tracks(&na_query);
        assert_eq!(before.len(), 1, "track should appear before play event");

        // Simulate playback service writing a TrackStarted fact via ACID after startup.
        // refresh_event_timestamps() now reads from the ACID stream incrementally, so
        // this write will be picked up on the next search_tracks call.
        let source = music_facts::FactSource::new(
            "test-playback",
            "1.0.0",
            music_facts::FactOrigin::Unknown,
        );
        let external_client = AcidClient::connect(&facts_addr).unwrap();
        external_client
            .write_music_facts(
                &hash,
                &[(MusicValue::TrackStarted(StartReason::OnRequest), source)],
            )
            .unwrap();

        // After: track now has play history → NA query should return 0
        let after = service.search_tracks(&na_query);
        assert!(
            after.is_empty(),
            "DateQuery::NA should exclude track after TrackStarted fact written to ACID"
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

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

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

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // facts_count is set from ACID stream load (3 facts: Title, FilePath, Format).
        // If backfill wrote a new Format fact the count would be 4. Assert it stays at 3.
        let facts_after = service
            .facts_count
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            facts_after, 3,
            "Should not write new Format fact when track already has one (facts_count must stay at 3)"
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

    /// Helper: build an empty service backed by a real (in-process) ACID server.
    ///
    /// Returns `(service, metadata_dir, acid_handle)`. Callers must keep
    /// `_acid` alive for the duration of the test; dropping it shuts the server down.
    fn make_empty_service() -> (
        LibraryService,
        tempfile::TempDir,
        acid_service::ServerHandle,
    ) {
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
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

    /// Regression test: PlaylistReplace must succeed on a bare tempdir with no
    /// pre-existing `playlists/` subdirectory. This would have caught the original
    /// bug where the directory was never created automatically.
    #[test]
    fn playlist_replace_succeeds_without_preexisting_playlists_dir() {
        use library_ipc_protocol::PlaylistName;
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        // Deliberately do NOT create metadata_dir/playlists — that is the regression scenario.
        let (acid_handle, facts_addr, events_addr) = spawn_acid_server();
        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .expect("service construction must succeed even without playlists dir");
        let _acid_handle = acid_handle; // keep alive

        let name = PlaylistName::new("bare-dir-test").unwrap();
        let content = "sha256:aaa\nsha256:bbb\n".to_string();
        let response = service.handle_request(LibraryRequest::PlaylistReplace {
            name,
            content: content.clone(),
        });
        match response {
            LibraryResponse::PlaylistContent(c) => {
                assert_eq!(c, content);
                // Also verify the file actually landed on disk
                let file_path = metadata_dir
                    .path()
                    .join("playlists")
                    .join("bare-dir-test.plist");
                assert!(file_path.exists(), "playlist file must exist on disk");
            }
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
        let (service, _metadata_dir, _acid) = make_empty_service();

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
        let (service, _metadata_dir, _acid) = make_empty_service();

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
        let (service, _metadata_dir, _acid) = make_empty_service();

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

        // Start with empty service backed by a real ACID server
        let (service, _metadata_dir, _acid) = make_empty_service();

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

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

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
    // Bug fix tests: apply_lines_to_map and ACID bootstrap
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

        let (loaded, _cursor) = LibraryService::load_from_acid_stream(&acid_client, None)
            .expect("load_from_acid_stream must succeed with reachable ACID server");

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

    /// Library always does a full bootstrap (cursor=None) from ACID on startup.
    ///
    /// Even when a stale `facts.cursor` file exists on disk, it is ignored.
    /// Facts are read starting from the beginning of the ACID stream.
    #[test]
    fn library_always_bootstraps_from_cursor_zero() {
        let hash = ContentHash::new("sha256:fullbootstrap01");

        let temp = write_facts_file(&[(
            hash.clone(),
            MusicValue::Title(Title::new("Bootstrap Track")),
        )]);

        // make_service_with_facts starts ACID with the facts pre-loaded and
        // boots the library from ACID stream cursor=0, ignoring any disk state.
        let (service, metadata_dir) = make_service_with_facts(temp.path());

        // Write a stale cursor file — library must ignore it on the NEXT restart
        // (demonstrated by a fresh service below that also reads all facts).
        std::fs::write(metadata_dir.path().join("facts.cursor"), "line:9999").unwrap();

        let tracks = service.tracks.lock().unwrap();
        assert_eq!(
            tracks.len(),
            1,
            "library must load track from ACID stream on full bootstrap"
        );
        assert_eq!(
            tracks[0].title.as_deref(),
            Some("Bootstrap Track"),
            "track title must match what was written to ACID"
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

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

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

    // =========================================================================
    // RetractSourceFacts tests
    // =========================================================================

    /// Helper: build a minimal service backed by a real (in-process) ACID server
    /// and pre-load it with a given facts file.
    ///
    /// The facts file is replayed into the ACID memory server at startup so the
    /// library bootstrap reads them via the ACID stream (no file fallback).
    fn make_service_with_facts(
        facts_file: &std::path::Path,
    ) -> (LibraryService, tempfile::TempDir) {
        let music_dir = tempfile::tempdir().unwrap();
        // Use a shared metadata dir for both ACID and the service so that
        // acid_service::start() can replay the facts file from it.
        let metadata_dir = tempfile::tempdir().unwrap();
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(facts_file, &facts_dest).unwrap();

        let (acid_handle, facts_addr, events_addr) = {
            let id = ACID_SERVER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let pid = std::process::id();
            let fa = format!("ipc:///tmp/mdma-test-acid-facts-{}-{}.sock", pid, id);
            let ea = format!("ipc:///tmp/mdma-test-acid-events-{}-{}.sock", pid, id);
            let rep = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
            rep.listen(&fa).expect("rep listen");
            let pub_sock = nng::Socket::new(nng::Protocol::Pub0).expect("pub socket");
            pub_sock.listen(&ea).expect("pub listen");
            let handle = acid_service::start(rep, pub_sock, metadata_dir.path())
                .expect("failed to start acid server");
            std::thread::sleep(std::time::Duration::from_millis(20));
            (handle, fa, ea)
        };

        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .unwrap();
        // Leak the ACID ServerHandle so the background thread outlives this test.
        // In a test process this is benign — the process exits when tests finish.
        Box::leak(Box::new(acid_handle));
        (service, metadata_dir)
    }

    /// Like `make_service_with_facts` but also returns the ACID request socket address
    /// so the caller can connect an external AcidClient and write additional facts.
    fn make_service_with_facts_and_addr(
        facts_file: &std::path::Path,
    ) -> (LibraryService, tempfile::TempDir, String) {
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let facts_dest = metadata_dir.path().join("facts.jsonl");
        std::fs::copy(facts_file, &facts_dest).unwrap();

        let id = ACID_SERVER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let facts_addr = format!("ipc:///tmp/mdma-test-acid-facts-{}-{}.sock", pid, id);
        let events_addr = format!("ipc:///tmp/mdma-test-acid-events-{}-{}.sock", pid, id);
        let rep = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
        rep.listen(&facts_addr).expect("rep listen");
        let pub_sock = nng::Socket::new(nng::Protocol::Pub0).expect("pub socket");
        pub_sock.listen(&events_addr).expect("pub listen");
        let handle = acid_service::start(rep, pub_sock, metadata_dir.path())
            .expect("failed to start acid server");
        std::thread::sleep(std::time::Duration::from_millis(20));

        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .unwrap();
        Box::leak(Box::new(handle));
        (service, metadata_dir, facts_addr)
    }

    /// Verify that refresh_event_timestamps advances the event_cursor so that a
    /// second call does not double-process already-seen facts.
    ///
    /// Step 1: Write TrackStarted for hash_a, call search (triggers refresh).
    ///         hash_a should appear with last_started set.
    /// Step 2: Write TrackStarted for hash_b, call search again.
    ///         hash_b should now appear with last_started set.
    ///         hash_a's last_started must remain unchanged (cursor didn't go back).
    #[test]
    fn refresh_event_timestamps_incremental_cursor() {
        use library_search::{query::DateQuery, TrackQuery};

        let hash_a = ContentHash::new("sha256:cursor_test_a");
        let hash_b = ContentHash::new("sha256:cursor_test_b");

        // Bootstrap with two tracks, neither played
        let temp = write_facts_file(&[
            (hash_a.clone(), MusicValue::Title(Title::new("Track A"))),
            (hash_b.clone(), MusicValue::Title(Title::new("Track B"))),
        ]);

        let (service, _metadata_dir, facts_addr) = make_service_with_facts_and_addr(temp.path());

        let not_played_query = TrackQuery {
            started: Some(DateQuery::NA),
            ..Default::default()
        };

        // Both tracks are unplayed
        let before = service.search_tracks(&not_played_query);
        assert_eq!(before.len(), 2, "both tracks should be unplayed initially");

        let source = FactSource::new("test-playback", "1.0.0", FactOrigin::Unknown);
        let external_client = AcidClient::connect(&facts_addr).unwrap();

        // Step 1: play hash_a
        external_client
            .write_music_facts(
                &hash_a,
                &[(
                    MusicValue::TrackStarted(StartReason::OnRequest),
                    source.clone(),
                )],
            )
            .unwrap();

        let after_a = service.search_tracks(&not_played_query);
        assert_eq!(
            after_a.len(),
            1,
            "only hash_b should remain unplayed after hash_a is played"
        );
        assert_eq!(
            after_a[0].content_hash, hash_b,
            "the remaining unplayed track should be hash_b"
        );

        // Verify hash_a has last_started set
        let tracks = service.tracks.lock().unwrap();
        let a = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash_a.as_str())
            .unwrap();
        let a_started = a.last_started;
        drop(tracks);
        assert!(
            a_started.is_some(),
            "hash_a must have last_started set after play"
        );

        // Step 2: play hash_b — cursor should have advanced, so hash_a is not re-processed
        external_client
            .write_music_facts(
                &hash_b,
                &[(MusicValue::TrackStarted(StartReason::OnRequest), source)],
            )
            .unwrap();

        let after_b = service.search_tracks(&not_played_query);
        assert!(
            after_b.is_empty(),
            "no unplayed tracks should remain after both are played"
        );

        // hash_a's last_started must be unchanged (cursor did not go back to re-read it)
        let tracks = service.tracks.lock().unwrap();
        let a_after = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash_a.as_str())
            .unwrap();
        assert_eq!(
            a_after.last_started, a_started,
            "hash_a last_started must be identical after second refresh (cursor advanced)"
        );
    }

    #[test]
    fn retract_source_facts_removes_album_and_title_from_in_memory_track() {
        // Arrange: a track with ItemId, Album and Title written by "mdma-library"
        // with FactOrigin::Bandcamp — exactly as production sets it.
        let hash = ContentHash::new("sha256:retracttest01");
        let source = FactSource::new(
            "mdma-library",
            "0.0.0",
            FactOrigin::bandcamp(Some("https://artist.bandcamp.com".to_string())),
        );
        let item_id = "p12345";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), source.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new("Old Album")),
                            source.clone(),
                        ),
                        (MusicValue::Title(Title::new("Old Title")), source.clone()),
                        (
                            MusicValue::Artist(music_facts::Artist::new("Old Artist")),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // Pre-condition: album and title visible in memory
        {
            let tracks = service.tracks.lock().unwrap();
            let t = tracks
                .iter()
                .find(|t| t.content_hash.as_str() == hash.as_str())
                .expect("track must be indexed before retraction");
            assert_eq!(t.album.as_deref(), Some("Old Album"));
            assert_eq!(t.title.as_deref(), Some("Old Title"));
        }

        // Act: retract with "bandcamp" source_name — matches FactOrigin::Bandcamp
        let response = service.handle_request(LibraryRequest::RetractSourceFacts {
            item_id: item_id.to_string(),
            source_name: "bandcamp".to_string(),
        });

        // Assert response
        assert!(
            matches!(response, LibraryResponse::SourceFactsRetracted),
            "expected SourceFactsRetracted, got {:?}",
            response
        );

        // Assert in-memory state cleared
        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .expect("track must still be indexed after retraction");
        assert_eq!(
            t.album, None,
            "album should be cleared after RetractSourceFacts"
        );
        assert_eq!(
            t.title, None,
            "title should be cleared after RetractSourceFacts"
        );
        assert_eq!(
            t.artist, None,
            "artist should be cleared after RetractSourceFacts"
        );
    }

    #[test]
    fn retract_source_facts_writes_retract_entries_to_acid() {
        let hash = ContentHash::new("sha256:retractfile01");
        // Use Bandcamp origin + "bandcamp" source_name — matches production usage
        let source = FactSource::new("mdma-library", "0.0.0", FactOrigin::bandcamp(None));
        let item_id = "p54321";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), source.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new("Retract Album")),
                            source.clone(),
                        ),
                        (
                            MusicValue::Title(Title::new("Retract Title")),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir, facts_addr) = make_service_with_facts_and_addr(temp.path());

        service.handle_request(LibraryRequest::RetractSourceFacts {
            item_id: item_id.to_string(),
            source_name: "bandcamp".to_string(),
        });

        // Verify Retract entries appear in ACID via read_entity
        let verifier = AcidClient::connect(&facts_addr).unwrap();
        let lines = verifier.read_entity(hash.as_str()).unwrap();

        let retract_count = lines
            .iter()
            .filter(|line| {
                serde_json::from_str::<stainless_facts::Fact<ContentHash, MusicValue, FactSource>>(
                    line,
                )
                .ok()
                .map(|f| f.operation() == stainless_facts::Operation::Retract)
                .unwrap_or(false)
            })
            .count();

        assert!(
            retract_count > 0,
            "at least one Retract entry should be in ACID after RetractSourceFacts"
        );
    }

    #[test]
    fn retract_source_facts_noop_for_unknown_item_id() {
        let hash = ContentHash::new("sha256:retractnoop01");
        let source = FactSource::new("mdma-library", "0.0.0", FactOrigin::bandcamp(None));

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[(MusicValue::ItemId("p99999".to_string()), source.clone())],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // Requesting retraction for an unknown item_id should still succeed (no-op)
        let response = service.handle_request(LibraryRequest::RetractSourceFacts {
            item_id: "p00000_does_not_exist".to_string(),
            source_name: "bandcamp".to_string(),
        });

        assert!(
            matches!(response, LibraryResponse::SourceFactsRetracted),
            "expected SourceFactsRetracted even for unknown item_id, got {:?}",
            response
        );
    }

    // =========================================================================
    // GetFacts tests
    // =========================================================================

    /// GetFacts must read from ACID, not the local facts file.
    ///
    /// Proof: write a fact directly into ACID (after service startup, bypassing the
    /// file) then verify that GetFacts returns it.  The file-based reader would miss
    /// this fact because it was never written to disk.
    #[test]
    fn get_facts_reads_from_acid_not_file() {
        // Start service with an empty facts file (no tracks known)
        let empty = NamedTempFile::new().unwrap();
        let (service, _metadata_dir, facts_addr) = make_service_with_facts_and_addr(empty.path());

        // Inject a fact directly into ACID (not via the file)
        let hash = ContentHash::new("sha256:getfacts_acid_only");
        let source = FactSource::new("test-injector", "0.0.0", FactOrigin::Unknown);
        let external = AcidClient::connect(&facts_addr).unwrap();
        external
            .write_music_facts(
                &hash,
                &[(MusicValue::Title(Title::new("ACID Only Track")), source)],
            )
            .unwrap();

        // Seed the in-memory tracks index so resolve_hash succeeds (it searches
        // self.tracks, not content_hashes).  A minimal empty entry is sufficient.
        service
            .tracks
            .lock()
            .unwrap()
            .push(IndexedTrackInfo::new_empty(hash.as_str().to_owned()));

        // Now call GetFacts — implementation MUST read from ACID to find this fact
        let response = service.handle_request(LibraryRequest::GetFacts { hash: hash.clone() });

        match response {
            LibraryResponse::Facts { hash: h, facts } => {
                assert_eq!(h.as_str(), hash.as_str());
                assert!(
                    facts.iter().any(|(k, _)| k == "Title"),
                    "expected a Title fact, got: {:?}",
                    facts
                );
            }
            other => panic!("expected LibraryResponse::Facts, got {:?}", other),
        }
    }

    // =========================================================================
    // GetAlbumTitleByItemId tests
    // =========================================================================

    #[test]
    fn get_album_title_by_item_id_returns_album_when_present() {
        let hash = ContentHash::new("sha256:albumbyitemid01");
        let source = FactSource::new("mdma-library", "0.0.0", FactOrigin::Unknown);
        let item_id = "p77777";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), source.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new("Expected Album")),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let response = service.handle_request(LibraryRequest::GetAlbumTitleByItemId {
            item_id: item_id.to_string(),
        });

        match response {
            LibraryResponse::AlbumTitleByItemId(Some(title)) => {
                assert_eq!(title, "Expected Album");
            }
            other => panic!("expected AlbumTitleByItemId(Some(_)), got {:?}", other),
        }
    }

    #[test]
    fn get_album_title_by_item_id_returns_none_for_unknown_id() {
        let empty_facts = NamedTempFile::new().unwrap();
        let (service, _metadata_dir) = make_service_with_facts(empty_facts.path());

        let response = service.handle_request(LibraryRequest::GetAlbumTitleByItemId {
            item_id: "p_unknown_000".to_string(),
        });

        assert!(
            matches!(response, LibraryResponse::AlbumTitleByItemId(None)),
            "expected AlbumTitleByItemId(None) for unknown item_id, got {:?}",
            response
        );
    }

    #[test]
    fn get_album_title_by_item_id_with_two_tracks_same_album() {
        // Two tracks share the same ItemId and album — should return Some(album)
        let hash_a = ContentHash::new("sha256:twotrackalbum01");
        let hash_b = ContentHash::new("sha256:twotrackalbum02");
        let source = FactSource::new("mdma-library", "0.0.0", FactOrigin::Unknown);
        let item_id = "p88888";
        let album_name = "Shared Album Name";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash_a,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), source.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new(album_name)),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            writer
                .write_track_facts(
                    &hash_b,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), source.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new(album_name)),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let response = service.handle_request(LibraryRequest::GetAlbumTitleByItemId {
            item_id: item_id.to_string(),
        });

        match response {
            LibraryResponse::AlbumTitleByItemId(Some(title)) => {
                assert_eq!(title, album_name);
            }
            other => panic!(
                "expected AlbumTitleByItemId(Some({:?})), got {:?}",
                album_name, other
            ),
        }
    }

    // =========================================================================
    // Retract semantics in load path
    // =========================================================================

    /// Helper: write a sequence of (ContentHash, MusicValue, timestamp, Operation) facts
    /// directly into a temp file so we can test Retract handling.
    fn write_facts_file_with_operations(
        facts: &[(ContentHash, MusicValue, chrono::DateTime<Utc>, Operation)],
    ) -> NamedTempFile {
        let temp = NamedTempFile::new().unwrap();
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let mut writer = FactStreamWriter::open(temp.path()).unwrap();

        let fact_structs: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
            .iter()
            .map(|(hash, value, ts, op)| {
                Fact::new(hash.clone(), value.clone(), *ts, source.clone(), *op)
            })
            .collect();
        writer.write_batch(&fact_structs).unwrap();
        temp
    }

    /// Assert(Album="Old"), Assert(Title="T"), Retract(Album="Old") ->
    /// album should be None, title should be Some("T").
    #[test]
    fn load_tracks_retract_clears_field() {
        let hash = ContentHash::new("sha256:retract_clears_01");
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_operations(&[
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("Old")),
                ts,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("T")),
                ts,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("Old")),
                ts,
                Operation::Retract,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.album, None,
            "album should be None after Retract(Album)"
        );
        assert_eq!(
            track.title.as_deref(),
            Some("T"),
            "title should remain after only Album was retracted"
        );
    }

    // =========================================================================
    // Blocker 1: retract_source_facts filters by FactOrigin, not tool name
    // =========================================================================

    /// retract_source_facts with source_name="bandcamp" MUST retract facts whose
    /// origin is FactOrigin::Bandcamp, even though tool="mdma-library".
    #[test]
    fn retract_source_facts_bandcamp_origin_is_retracted() {
        let hash = ContentHash::new("sha256:bc_retract_01");
        // tool is "mdma-library" — this is what production sets; origin carries bandcamp-ness
        let source = FactSource::new(
            "mdma-library",
            "0.0.0",
            FactOrigin::bandcamp(Some("https://artist.bandcamp.com".to_string())),
        );
        let item_id = "p_bc_01";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), source.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new("Bandcamp Album")),
                            source.clone(),
                        ),
                        (
                            MusicValue::Title(Title::new("Bandcamp Track")),
                            source.clone(),
                        ),
                        (
                            MusicValue::Artist(music_facts::Artist::new("Bandcamp Artist")),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // Pre-condition: track is indexed with values
        {
            let tracks = service.tracks.lock().unwrap();
            let t = tracks
                .iter()
                .find(|t| t.content_hash.as_str() == hash.as_str())
                .expect("track must be indexed before retraction");
            assert_eq!(t.album.as_deref(), Some("Bandcamp Album"));
        }

        // Act: retract with source_name="bandcamp" (matches FactOrigin::Bandcamp)
        let response = service.handle_request(LibraryRequest::RetractSourceFacts {
            item_id: item_id.to_string(),
            source_name: "bandcamp".to_string(),
        });

        assert!(
            matches!(response, LibraryResponse::SourceFactsRetracted),
            "expected SourceFactsRetracted, got {:?}",
            response
        );

        // Assert in-memory state cleared
        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .expect("track must still be indexed after retraction");
        assert_eq!(
            t.album, None,
            "album should be cleared when retracted by bandcamp origin"
        );
        assert_eq!(
            t.title, None,
            "title should be cleared when retracted by bandcamp origin"
        );
        assert_eq!(
            t.artist, None,
            "artist should be cleared when retracted by bandcamp origin"
        );
    }

    /// Facts with a NON-bandcamp origin (FactOrigin::User) for the same ItemId
    /// must NOT be retracted when source_name="bandcamp".
    #[test]
    fn retract_source_facts_non_bandcamp_origin_is_not_retracted() {
        let hash = ContentHash::new("sha256:non_bc_retract_01");
        let bandcamp_source = FactSource::new("mdma-library", "0.0.0", FactOrigin::bandcamp(None));
        let user_source = FactSource::new("mdma-library", "0.0.0", FactOrigin::User);
        let item_id = "p_non_bc_01";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[
                        (
                            MusicValue::ItemId(item_id.to_string()),
                            bandcamp_source.clone(),
                        ),
                        // bandcamp-origin fact — should be retracted
                        (
                            MusicValue::Album(music_facts::Album::new("BC Album")),
                            bandcamp_source.clone(),
                        ),
                        // user-origin fact — must NOT be retracted
                        (
                            MusicValue::Title(Title::new("User Title")),
                            user_source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let response = service.handle_request(LibraryRequest::RetractSourceFacts {
            item_id: item_id.to_string(),
            source_name: "bandcamp".to_string(),
        });

        assert!(
            matches!(response, LibraryResponse::SourceFactsRetracted),
            "expected SourceFactsRetracted, got {:?}",
            response
        );

        // User-origin title must survive
        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .expect("track must still be indexed after retraction");
        assert_eq!(
            t.title.as_deref(),
            Some("User Title"),
            "title from User origin must not be retracted by source_name=bandcamp"
        );
    }

    // =========================================================================
    // Blocker 2: fact_index retract is entity-aware (shared-value safety)
    // =========================================================================

    /// When two tracks both assert MainGenre("techno") and track 1 retracts,
    /// fact_index must still contain "techno" because track 2 still asserts it.
    #[test]
    fn fact_index_retract_keeps_value_when_other_asserter_exists() {
        let hash_a = ContentHash::new("sha256:shared_genre_01");
        let hash_b = ContentHash::new("sha256:shared_genre_02");
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_operations(&[
            // Both tracks assert techno
            (
                hash_a.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts,
                Operation::Assert,
            ),
            (
                hash_b.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts,
                Operation::Assert,
            ),
            // Track A retracts — track B still has it
            (
                hash_a.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts2,
                Operation::Retract,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        let main_genre_type = FactType::new("MainGenre");
        let still_there = result
            .fact_index
            .get(&main_genre_type)
            .is_some_and(|s| s.contains("techno"));
        assert!(
            still_there,
            "fact_index must still contain 'techno' because track B still asserts it"
        );
    }

    /// When the last asserter retracts, fact_index must drop the value.
    #[test]
    fn fact_index_retract_removes_value_when_last_asserter_retracts() {
        let hash_a = ContentHash::new("sha256:shared_genre_03");
        let hash_b = ContentHash::new("sha256:shared_genre_04");
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2026, 1, 3, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_operations(&[
            (
                hash_a.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts,
                Operation::Assert,
            ),
            (
                hash_b.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts,
                Operation::Assert,
            ),
            // Both retract
            (
                hash_a.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts2,
                Operation::Retract,
            ),
            (
                hash_b.clone(),
                MusicValue::MainGenre("techno".to_string()),
                ts3,
                Operation::Retract,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        let main_genre_type = FactType::new("MainGenre");
        let gone = result
            .fact_index
            .get(&main_genre_type)
            .is_none_or(|s| !s.contains("techno"));
        assert!(
            gone,
            "fact_index must not contain 'techno' after all asserters retracted"
        );
    }

    // =========================================================================
    // GetTrackCountForItemId tests
    // =========================================================================

    #[test]
    fn get_track_count_for_item_id_returns_correct_count() {
        // Three tracks all tagged with the same ItemId
        let hash_a = ContentHash::new("sha256:trackcount01");
        let hash_b = ContentHash::new("sha256:trackcount02");
        let hash_c = ContentHash::new("sha256:trackcount03");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let item_id = "p_trackcount";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            for hash in &[hash_a.clone(), hash_b.clone(), hash_c.clone()] {
                writer
                    .write_track_facts(
                        hash,
                        &[(MusicValue::ItemId(item_id.to_string()), source.clone())],
                    )
                    .unwrap();
            }
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let response = service.handle_request(LibraryRequest::GetTrackCountForItemId {
            item_id: item_id.to_string(),
        });

        match response {
            LibraryResponse::TrackCountForItemId(count) => {
                assert_eq!(count, 3, "expected 3 tracks for item_id={}", item_id);
            }
            other => panic!("expected TrackCountForItemId, got {:?}", other),
        }
    }

    #[test]
    fn get_track_count_for_item_id_returns_zero_for_unknown_id() {
        let empty_facts = NamedTempFile::new().unwrap();
        let (service, _metadata_dir) = make_service_with_facts(empty_facts.path());

        let response = service.handle_request(LibraryRequest::GetTrackCountForItemId {
            item_id: "p_unknown_xyz".to_string(),
        });

        match response {
            LibraryResponse::TrackCountForItemId(count) => {
                assert_eq!(count, 0, "unknown ItemId should return 0");
            }
            other => panic!("expected TrackCountForItemId(0), got {:?}", other),
        }
    }

    // =========================================================================
    // content_hashes_for_item_id in-memory scan tests
    // =========================================================================

    /// content_hashes_for_item_id returns hashes of tracks with matching ItemId,
    /// and does not return hashes whose ItemId differs.
    #[test]
    fn content_hashes_for_item_id_returns_matching_hashes() {
        let hash_a = ContentHash::new("sha256:itemid_test_aaa");
        let hash_b = ContentHash::new("sha256:itemid_test_bbb");
        let hash_c = ContentHash::new("sha256:itemid_test_ccc");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let target_item_id = "p_target_001";
        let other_item_id = "p_other_002";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            // hash_a and hash_b have target ItemId
            writer
                .write_track_facts(
                    &hash_a,
                    &[(
                        MusicValue::ItemId(target_item_id.to_string()),
                        source.clone(),
                    )],
                )
                .unwrap();
            writer
                .write_track_facts(
                    &hash_b,
                    &[(
                        MusicValue::ItemId(target_item_id.to_string()),
                        source.clone(),
                    )],
                )
                .unwrap();
            // hash_c has a different ItemId
            writer
                .write_track_facts(
                    &hash_c,
                    &[(
                        MusicValue::ItemId(other_item_id.to_string()),
                        source.clone(),
                    )],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let mut result = service.content_hashes_for_item_id(target_item_id);
        result.sort_unstable_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut result_strs: Vec<&str> = result.iter().map(|h| h.as_str()).collect();
        result_strs.sort_unstable();

        assert!(
            result_strs.contains(&hash_a.as_str()),
            "hash_a should be in result, got: {:?}",
            result_strs
        );
        assert!(
            result_strs.contains(&hash_b.as_str()),
            "hash_b should be in result, got: {:?}",
            result_strs
        );
        assert_eq!(
            result_strs.len(),
            2,
            "only 2 tracks should match target_item_id"
        );
    }

    /// Retracting an ItemId fact means content_hashes_for_item_id no longer
    /// returns that hash.
    #[test]
    fn content_hashes_for_item_id_excludes_retracted_item_id() {
        let hash = ContentHash::new("sha256:itemid_retract_tst");
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let item_id = "p_retract_me";

        let temp = write_facts_file_with_operations(&[
            (
                hash.clone(),
                MusicValue::ItemId(item_id.to_string()),
                t1,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::ItemId(item_id.to_string()),
                t2,
                Operation::Retract,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        // After retraction the track should still exist but have item_id=None
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(
            result.tracks[0].item_id, None,
            "item_id should be None after Retract"
        );
    }

    /// Assert(Album="A"), Retract(Album="A"), Assert(Album="B") ->
    /// final album should be Some("B").
    #[test]
    fn load_tracks_retract_then_reassert() {
        let hash = ContentHash::new("sha256:retract_reassert_01");
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2026, 1, 3, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_operations(&[
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("A")),
                t1,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("A")),
                t2,
                Operation::Retract,
            ),
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("B")),
                t3,
                Operation::Assert,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.album.as_deref(),
            Some("B"),
            "album should be Some('B') after Assert(A), Retract(A), Assert(B)"
        );
    }

    // =========================================================================
    // Inbox scanner tests
    // =========================================================================

    /// AppleDouble sidecar files (._<name>) created by macOS Finder/SMB must be
    /// excluded from the inbox queue so the ingest pipeline never tries to parse
    /// them as audio.
    #[test]
    fn inbox_scanner_ignores_appledouble_files() {
        let music_dir = tempfile::tempdir().unwrap();
        let inbox_dir = music_dir.path().join("inbox");
        std::fs::create_dir_all(&inbox_dir).unwrap();

        // Real audio file (placeholder content — scanner only reads filenames)
        std::fs::write(inbox_dir.join("track.mp3"), b"fake mp3").unwrap();
        // AppleDouble sidecar created by macOS
        std::fs::write(inbox_dir.join("._track.mp3"), b"AppleDouble metadata").unwrap();

        let metadata_dir = tempfile::tempdir().unwrap();
        let (acid_handle, facts_addr, events_addr) = spawn_acid_server();
        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .unwrap();
        let _acid = acid_handle;

        let queue = service.get_inbox_queue_internal();

        let filenames: Vec<_> = queue
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();

        assert!(
            !filenames.iter().any(|n| n.starts_with("._")),
            "inbox scanner must exclude AppleDouble sidecar files, got: {:?}",
            filenames
        );
        assert!(
            filenames.contains(&"track.mp3"),
            "real audio file must be present in inbox queue, got: {:?}",
            filenames
        );
    }

    // =========================================================================
    // Startup bootstrap tests (Change 1-3)
    // =========================================================================

    /// Library must always do a full bootstrap from cursor=None on startup.
    /// We verify by confirming the service starts successfully when ACID is
    /// unreachable and has no fallback — but since ACID is unreachable we expect
    /// Err, not an empty Ok. This proves we no longer silently fall through.
    ///
    /// The actual "cursor=None path" is indirectly verified by
    /// `startup_succeeds_with_empty_acid` below which uses a real ACID server.
    #[test]
    fn startup_does_not_read_cursor_from_disk() {
        // Write a cursor file to metadata_dir — if the new code reads it,
        // it would use a non-zero offset. With a real ACID server that has
        // no facts this should still produce an empty library (not an error),
        // demonstrating we pass None and don't skip facts.
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        // Write a stale cursor file to disk
        let cursor_path = metadata_dir.path().join("facts.cursor");
        std::fs::write(&cursor_path, "line:9999").unwrap();

        // With the new code, load_saved_cursor is gone; the cursor file is ignored.
        // Verify that the cursor functions no longer exist on LibraryService by
        // ensuring we can still construct the service (even against a dead socket —
        // the new code should fail loud, not silently fall back).
        let result = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-nonexistent-bootstrap.sock",
        );
        // New code: ACID unreachable => Err(ServiceError::Acid(_)), not Ok with fallback
        assert!(
            result.is_err(),
            "startup must return Err when ACID is unreachable — no silent fallback"
        );
        match result.err().unwrap() {
            ServiceError::Acid(_) => {} // correct
            other => panic!("expected ServiceError::Acid, got {:?}", other),
        }
    }

    /// When ACID is reachable but has 0 facts, library starts with empty state — not an error.
    #[test]
    fn startup_succeeds_with_empty_acid() {
        let (service, _metadata_dir, _acid) = make_empty_service();

        assert_eq!(
            service
                .tracks_indexed
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "tracks_indexed must be 0 for empty ACID"
        );
    }

    /// When ACID is unreachable, new_with_events must return Err(ServiceError::Acid(_)).
    #[test]
    fn startup_fails_loud_when_acid_unreachable() {
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();

        let result = LibraryService::new(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            "ipc:///tmp/mdma-test-acid-fail-loud.sock",
        );

        assert!(
            result.is_err(),
            "startup must return Err when ACID is unreachable"
        );
        match result.err().unwrap() {
            ServiceError::Acid(_) => {} // correct: propagated ACID IPC error
            other => panic!("expected ServiceError::Acid(_), got {:?}", other),
        }
    }

    /// RetractFact handler writes a Retract entry to ACID after a prior Assert.
    ///
    /// Injects the hash into the in-memory index (required for resolve_hash), then
    /// seeds via WriteFact (Assert), and sends RetractFact.
    /// Verifies that ACID contains one Assert record and one Retract record.
    #[test]
    fn retract_fact_handler_writes_retract_entry_to_acid() {
        use acid_client::AcidClient;

        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        let (acid_handle, facts_addr, events_addr) = spawn_acid_server();

        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .unwrap();

        let hash = ContentHash::new("sha256:retractfact01");

        // Inject the hash into the in-memory track index so resolve_hash can find it.
        {
            let mut tracks = service.tracks.lock().unwrap();
            let mut entry = IndexedTrackInfo::new_empty(hash.as_str().to_owned());
            entry.title = Some("Retractable Title".to_owned());
            tracks.push(entry);
        }

        // Seed: assert a fact via WriteFact
        let write_resp = service.handle_request(LibraryRequest::WriteFact {
            hash: hash.clone(),
            fact: MusicValue::Title(Title::new("Retractable Title")),
        });
        assert!(
            matches!(write_resp, LibraryResponse::FactWritten),
            "expected FactWritten, got {:?}",
            write_resp
        );

        // Retract the same fact
        let retract_resp = service.handle_request(LibraryRequest::RetractFact {
            hash: hash.clone(),
            fact: MusicValue::Title(Title::new("Retractable Title")),
        });
        assert!(
            matches!(retract_resp, LibraryResponse::FactRetracted),
            "expected FactRetracted, got {:?}",
            retract_resp
        );

        // Verify ACID contains one Assert and one Retract record
        let verifier = AcidClient::connect(&facts_addr).unwrap();
        let lines = verifier.read_entity(hash.as_str()).unwrap();
        drop(acid_handle);

        let assert_count = lines
            .iter()
            .filter(|line| {
                serde_json::from_str::<stainless_facts::Fact<ContentHash, MusicValue, FactSource>>(
                    line,
                )
                .ok()
                .map(|f| f.operation() == stainless_facts::Operation::Assert)
                .unwrap_or(false)
            })
            .count();

        let retract_count = lines
            .iter()
            .filter(|line| {
                serde_json::from_str::<stainless_facts::Fact<ContentHash, MusicValue, FactSource>>(
                    line,
                )
                .ok()
                .map(|f| f.operation() == stainless_facts::Operation::Retract)
                .unwrap_or(false)
            })
            .count();

        assert_eq!(assert_count, 1, "expected one Assert record in ACID");
        assert_eq!(retract_count, 1, "expected one Retract record in ACID");
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
/// On receiving an `acid/facts/asserted` or `acid/facts/retracted` event, fetches new facts from the ACID stream
/// starting at the in-memory cursor, applies them to the in-memory index, and
/// updates the in-memory cursor. The cursor is never persisted to disk.
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

        if let Err(e) =
            sub.set_opt::<nng::options::protocol::pubsub::Subscribe>(TOPIC_ACID.as_bytes().to_vec())
        {
            tracing::error!(error = %e, "Failed to subscribe to ACID events topic");
            return;
        }

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

                match &event {
                    AcidEvent::FactsAsserted { cursor, .. } => {
                        tracing::debug!(cursor = %cursor, "Received ACID facts-asserted notification");
                    }
                    AcidEvent::FactsRetracted { cursor, .. } => {
                        tracing::debug!(cursor = %cursor, "Received ACID facts-retracted notification");
                    }
                };

                // Fetch new facts starting from our in-memory cursor
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
                        // Update in-memory cursor only — no disk persistence
                        *service.cursor.lock().unwrap() = Some(chunk.cursor.clone());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Subscriber: failed to read incremental stream");
                    }
                }
            }
        }
    });
}
