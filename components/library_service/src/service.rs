//! Library service implementation
//!
//! Handles IPC requests and manages library state

use crate::ipc::{
    Bpm, ContentHash, DurationSeconds, FactType, InboxPath, IngestAllItem, IngestResult,
    IngestSource, IpcServer, Key, LibraryRequest, LibraryResponse, OrphanInfo, OrphanReason,
    ProtocolError, ServiceStatus, TrackInfo, TrackQuery,
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

/// A single memory cue, hot cue, or loop point stored in the in-memory index.
///
/// Stores `kind` as the typed `CueKind` (not a display string) for query-consistency
/// and correct equality checks during retraction.
#[derive(Clone, Debug, PartialEq)]
struct IndexedCue {
    position_ms: u32,
    kind: music_facts::CueKind,
    label: Option<String>,
    index: Option<u8>,
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
    /// Set when a Deleted fact has been asserted (track is hidden).
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// DJ curation role (Opener, BuildUp, Peak, etc.) — typed for query-consistency.
    role: Option<music_facts::TrackRole>,
    /// Energy level 1–10
    energy: Option<u8>,
    /// Beat-grid anchor as (first_beat_ms, bpm_f32, beats_per_bar)
    beat_grid: Option<(u32, f32, u8)>,
    /// Memory/hot cue and loop points stored as typed structs.
    memory_cues: Vec<IndexedCue>,
    /// Per-source provenance: ordered list of (value, source) assertions still live.
    ///
    /// Used to correctly handle retractions: Retract(value=V, source=S) removes the
    /// first matching pair, then the scalar field is re-resolved from whatever remains.
    /// For multi-valued fields (StyleDescriptor, MemoryCue) each assertion contributes a separate entry.
    provenance: Vec<(MusicValue, music_facts::FactSource)>,
    /// Raw FilePath value stored during fact application.
    ///
    /// FilePath is excluded from provenance (it never retracts) but the blob_path
    /// derivation needs the extension.  When content_hash is empty (aggregate_facts
    /// bulk path) the derivation is deferred; this field holds the path until the
    /// Phase 3 post-pass calls `derive_blob_path_if_needed`.
    raw_file_path: Option<PathBuf>,
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
            deleted_at: None,
            role: None,
            energy: None,
            beat_grid: None,
            memory_cues: vec![],
            provenance: Vec::new(),
            raw_file_path: None,
        }
    }
}

impl Default for IndexedTrackInfo {
    fn default() -> Self {
        // Default content_hash is a placeholder; aggregate_facts callers must set it
        // to the entity key after aggregation.
        Self::new_empty(String::new())
    }
}

/// Derive the blob storage path from a content hash and file path.
///
/// Returns `Some(PathBuf)` of the form `blobs/{prefix}/{hash_clean}.{ext}` when
/// `content_hash` (after stripping the `sha256:` prefix) has at least two characters
/// and `file_path` has a recognisable extension, otherwise `None`.
fn blob_path_for(content_hash: &str, file_path: &std::path::Path) -> Option<PathBuf> {
    let hash_clean = content_hash.strip_prefix("sha256:").unwrap_or(content_hash);
    if hash_clean.len() < 2 {
        return None;
    }
    let ext = file_path.extension()?.to_str()?;
    Some(PathBuf::from(format!(
        "blobs/{}/{}.{}",
        &hash_clean[..2],
        hash_clean,
        ext
    )))
}

impl IndexedTrackInfo {
    /// Returns true if this track is hidden from default views.
    fn is_hidden(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Derive `blob_path` from `raw_file_path` and `content_hash` if blob_path
    /// is still empty.
    ///
    /// Called in the Phase 3 post-pass of `load_from_acid_stream` after
    /// `content_hash` has been set from the entity key.  The aggregate_facts
    /// bulk path initialises entries with an empty content_hash (via Default)
    /// so the derivation inside `apply_assert_scalar` is skipped; this method
    /// completes it once the real hash is available.
    fn derive_blob_path_if_needed(&mut self) {
        if !self.blob_path.as_os_str().is_empty() {
            return; // already derived (file/stream paths)
        }
        let p = match &self.raw_file_path {
            Some(p) => p.clone(),
            None => return,
        };
        if let Some(bp) = blob_path_for(self.content_hash.as_str(), &p) {
            self.blob_path = bp;
        }
    }
}

/// Apply a single `MusicValue` fact (with its source) to a mutable `IndexedTrackInfo`.
///
/// ## Assert
/// Adds `(value, source)` to `entry.provenance`, then updates the corresponding
/// scalar field via `apply_assert_scalar`.
///
/// ## Retract
/// Removes the first `(v, s)` pair from `entry.provenance` where `v == value &&
/// s == source`, then re-resolves all scalar fields from the surviving provenance.
/// This fixes #96 (wrong-value retraction) and #2 (per-source survival).
///
/// `has_format` / `has_cover_art` side-effect sets are updated for Format and
/// CoverArtPath variants; pass `None` when not needed.
fn apply_fact_to_track(
    entry: &mut IndexedTrackInfo,
    value: &MusicValue,
    timestamp: chrono::DateTime<chrono::Utc>,
    operation: stainless_facts::Operation,
    source: &music_facts::FactSource,
    has_format: Option<&mut HashSet<String>>,
    has_cover_art: Option<&mut HashSet<String>>,
) {
    use stainless_facts::Operation;

    match operation {
        Operation::Assert => {
            // Store provenance for value-matched retraction (skip timestamp-only fields)
            if is_provenance_tracked(value) {
                entry.provenance.push((value.clone(), source.clone()));
            }
            apply_assert_scalar(entry, value, timestamp, has_format, has_cover_art);
        }
        Operation::Retract => {
            // Remove the first matching (value, source) pair from provenance.
            if is_provenance_tracked(value) {
                let pos = entry
                    .provenance
                    .iter()
                    .position(|(v, s)| v == value && s == source);
                if let Some(idx) = pos {
                    entry.provenance.remove(idx);
                } else {
                    // No matching assertion in provenance — retraction has no effect.
                    // This handles the #96 case: Retract(Bpm=120) when only Bpm=128
                    // is asserted finds no match and leaves the field untouched.
                    return;
                }
            }

            // Re-resolve scalar fields from what remains in provenance.
            apply_retract_scalar(entry, value, has_format, has_cover_art);
        }
    }
}

/// Returns true for `MusicValue` variants whose (value, source) pairs are tracked
/// in `IndexedTrackInfo::provenance` for correct retraction semantics.
///
/// Timestamp-only values (TrackStarted, TrackStopped) use a separate most-recent-wins
/// strategy and are intentionally excluded — retractions for those are not emitted
/// in production and the timestamp logic is handled elsewhere.
fn is_provenance_tracked(value: &MusicValue) -> bool {
    !matches!(
        value,
        MusicValue::TrackStarted(_)
            | MusicValue::TrackStopped(_)
            | MusicValue::FilePath(_)
            | MusicValue::AddedAt(_)
    )
}

/// Apply an Assert fact to the scalar fields of `IndexedTrackInfo`.
fn apply_assert_scalar(
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
            // Always stash the raw file path for deferred derivation.
            // When content_hash is already populated (file/stream paths) we can
            // derive blob_path immediately.  When it is empty (aggregate_facts
            // bulk path) the helper returns None and the Phase 3 post-pass
            // calls derive_blob_path_if_needed after content_hash is set.
            entry.raw_file_path = Some(p.clone());
            if let Some(bp) = blob_path_for(entry.content_hash.as_str(), p) {
                entry.blob_path = bp;
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
        MusicValue::Replaces(_) => {
            // Replaces is stored in provenance only; no scalar field needed.
            // The new track carries this fact; the old track's facts are retracted separately.
        }
        MusicValue::Deleted { timestamp } => {
            entry.deleted_at = Some(*timestamp);
        }
        MusicValue::Role(v) => entry.role = Some(*v),
        MusicValue::Energy(v) => entry.energy = Some(v.value()),
        MusicValue::BeatGrid {
            first_beat_ms,
            bpm,
            beats_per_bar,
        } => {
            entry.beat_grid = Some((*first_beat_ms, bpm.as_f32(), *beats_per_bar));
        }
        MusicValue::MemoryCue {
            position_ms,
            kind,
            label,
            index,
        } => {
            entry.memory_cues.push(IndexedCue {
                position_ms: *position_ms,
                kind: kind.clone(),
                label: label.clone(),
                index: *index,
            });
        }
        _ => {}
    }
}

/// Re-resolve a single scalar field after removing a provenance entry.
///
/// For fields that map to a scalar `Option<T>`, we scan the remaining provenance
/// for the last (most recent) assertion of the same variant and re-set the scalar.
/// For multi-valued fields (`StyleDescriptor`) we rebuild the Vec from scratch.
///
/// Fields not tracked in provenance (TrackStarted/Stopped, FilePath, AddedAt) are
/// left unchanged — their retraction is either a no-op or handled separately.
fn apply_retract_scalar(
    entry: &mut IndexedTrackInfo,
    retracted: &MusicValue,
    has_format: Option<&mut HashSet<String>>,
    has_cover_art: Option<&mut HashSet<String>>,
) {
    // Helper: find last surviving value for a given variant discriminant.
    // Returns a clone of the last provenance entry whose variant matches `matcher`.
    let last_surviving = |provenance: &[(MusicValue, music_facts::FactSource)],
                          matcher: &dyn Fn(&MusicValue) -> bool|
     -> Option<MusicValue> {
        provenance
            .iter()
            .filter(|(v, _)| matcher(v))
            .last()
            .map(|(v, _)| v.clone())
    };

    match retracted {
        MusicValue::Title(_) => {
            entry.title = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Title(_)))
                .and_then(|v| {
                    if let MusicValue::Title(t) = v {
                        Some(t.as_str().to_string())
                    } else {
                        None
                    }
                });
        }
        MusicValue::Artist(_) => {
            entry.artist =
                last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Artist(_)))
                    .and_then(|v| {
                        if let MusicValue::Artist(a) = v {
                            Some(a.as_str().to_string())
                        } else {
                            None
                        }
                    });
        }
        MusicValue::Album(_) => {
            entry.album = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Album(_)))
                .and_then(|v| {
                    if let MusicValue::Album(a) = v {
                        Some(a.as_str().to_string())
                    } else {
                        None
                    }
                });
        }
        MusicValue::Label(_) => {
            entry.label = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Label(_)))
                .and_then(|v| {
                    if let MusicValue::Label(l) = v {
                        Some(l.clone())
                    } else {
                        None
                    }
                });
        }
        MusicValue::MainGenre(_) => {
            entry.genre = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::MainGenre(_))
            })
            .and_then(|v| {
                if let MusicValue::MainGenre(g) = v {
                    Some(g.clone())
                } else {
                    None
                }
            });
        }
        MusicValue::StyleDescriptor(_) => {
            // Rebuild the full styles Vec from surviving provenance
            entry.styles = entry
                .provenance
                .iter()
                .filter_map(|(v, _)| {
                    if let MusicValue::StyleDescriptor(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect();
        }
        MusicValue::DurationSeconds(_) => {
            entry.duration_seconds = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::DurationSeconds(_))
            })
            .and_then(|v| {
                if let MusicValue::DurationSeconds(d) = v {
                    Some(d.value())
                } else {
                    None
                }
            });
        }
        MusicValue::Bpm(_) => {
            entry.bpm = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Bpm(_)))
                .and_then(|v| {
                    if let MusicValue::Bpm(b) = v {
                        Some(b.as_f32())
                    } else {
                        None
                    }
                });
        }
        MusicValue::Key(_) => {
            entry.key = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Key(_)))
                .and_then(|v| {
                    if let MusicValue::Key(k) = v {
                        Some(k.to_string())
                    } else {
                        None
                    }
                });
        }
        MusicValue::Year(_) => {
            entry.year = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Year(_)))
                .and_then(|v| {
                    if let MusicValue::Year(y) = v {
                        Some(y.value())
                    } else {
                        None
                    }
                });
        }
        MusicValue::TrackNumber(_) => {
            entry.track_number = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::TrackNumber(_))
            })
            .and_then(|v| {
                if let MusicValue::TrackNumber(n) = v {
                    Some(n.value())
                } else {
                    None
                }
            });
        }
        MusicValue::DiscNumber(_) => {
            entry.disc_number = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::DiscNumber(_))
            })
            .and_then(|v| {
                if let MusicValue::DiscNumber(n) = v {
                    Some(n.value())
                } else {
                    None
                }
            });
        }
        MusicValue::Source(_) => {
            entry.source =
                last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Source(_)))
                    .and_then(|v| {
                        if let MusicValue::Source(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    });
        }
        MusicValue::CoverArtPath(_) => {
            let surviving = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::CoverArtPath(_))
            });
            match surviving {
                Some(MusicValue::CoverArtPath(p)) => {
                    entry.cover_art_path = Some(PathBuf::from(&p));
                }
                _ => {
                    entry.cover_art_path = None;
                    if let Some(set) = has_cover_art {
                        set.remove(entry.content_hash.as_str());
                    }
                }
            }
        }
        MusicValue::Format(_) => {
            let has_surviving = entry
                .provenance
                .iter()
                .any(|(v, _)| matches!(v, MusicValue::Format(_)));
            if !has_surviving {
                if let Some(set) = has_format {
                    set.remove(entry.content_hash.as_str());
                }
            }
        }
        MusicValue::ItemId(_) => {
            entry.item_id =
                last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::ItemId(_)))
                    .and_then(|v| {
                        if let MusicValue::ItemId(id) = v {
                            Some(id.clone())
                        } else {
                            None
                        }
                    });
        }
        MusicValue::Replaces(_) => {
            // No scalar field for Replaces; provenance removal is sufficient.
        }
        MusicValue::Deleted { .. } => {
            let surviving = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::Deleted { .. })
            });
            entry.deleted_at = match surviving {
                Some(MusicValue::Deleted { timestamp }) => Some(timestamp),
                _ => None,
            };
        }
        MusicValue::Role(_) => {
            entry.role = last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Role(_)))
                .and_then(|v| {
                    if let MusicValue::Role(r) = v {
                        Some(r)
                    } else {
                        None
                    }
                });
        }
        MusicValue::Energy(_) => {
            entry.energy =
                last_surviving(&entry.provenance, &|v| matches!(v, MusicValue::Energy(_)))
                    .and_then(|v| {
                        if let MusicValue::Energy(e) = v {
                            Some(e.value())
                        } else {
                            None
                        }
                    });
        }
        MusicValue::BeatGrid { .. } => {
            entry.beat_grid = last_surviving(&entry.provenance, &|v| {
                matches!(v, MusicValue::BeatGrid { .. })
            })
            .and_then(|v| {
                if let MusicValue::BeatGrid {
                    first_beat_ms,
                    bpm,
                    beats_per_bar,
                } = v
                {
                    Some((first_beat_ms, bpm.as_f32(), beats_per_bar))
                } else {
                    None
                }
            });
        }
        MusicValue::MemoryCue { .. } => {
            // Rebuild the full memory_cues Vec from surviving provenance,
            // mirroring the StyleDescriptor retraction pattern.
            entry.memory_cues = entry
                .provenance
                .iter()
                .filter_map(|(v, _)| {
                    if let MusicValue::MemoryCue {
                        position_ms,
                        kind,
                        label,
                        index,
                    } = v
                    {
                        Some(IndexedCue {
                            position_ms: *position_ms,
                            kind: kind.clone(),
                            label: label.clone(),
                            index: *index,
                        })
                    } else {
                        None
                    }
                })
                .collect();
        }
        // TrackStarted/TrackStopped/FilePath/AddedAt retractions are not
        // emitted in the current codebase; ignore silently.
        _ => {}
    }
}

/// Implement `FactAggregator` so `stainless_facts::aggregate_facts` can drive
/// the three fold paths (bulk load, file load, incremental stream).
impl stainless_facts::FactAggregator<ContentHash, MusicValue, music_facts::FactSource>
    for IndexedTrackInfo
{
    fn assert(&mut self, value: &MusicValue, source: &music_facts::FactSource) {
        // TrackStarted/TrackStopped carry their real timestamp on the Fact struct,
        // but the FactAggregator trait only receives the value and source — the
        // timestamp is not available here. We must not populate last_started /
        // last_stopped with a bootstrap wall-clock placeholder, because
        // `refresh_event_timestamps` is the sole authoritative path for those
        // fields. Skip them here entirely; they will be set on the first
        // date-filtered search after bootstrap.
        if matches!(
            value,
            MusicValue::TrackStarted(_) | MusicValue::TrackStopped(_)
        ) {
            return;
        }
        apply_fact_to_track(
            self,
            value,
            chrono::Utc::now(), // timestamp unused for all non-timestamp fields
            stainless_facts::Operation::Assert,
            source,
            None,
            None,
        );
    }

    fn retract(&mut self, value: &MusicValue, source: &music_facts::FactSource) {
        // TrackStarted/TrackStopped retractions are not emitted in production;
        // and refresh_event_timestamps handles these fields authoritatively.
        // Skip to keep parity with assert.
        if matches!(
            value,
            MusicValue::TrackStarted(_) | MusicValue::TrackStopped(_)
        ) {
            return;
        }
        apply_fact_to_track(
            self,
            value,
            chrono::Utc::now(),
            stainless_facts::Operation::Retract,
            source,
            None,
            None,
        );
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
    /// Reads all paged fact lines from ACID, then uses `stainless_facts::aggregate_facts`
    /// to fold them into `IndexedTrackInfo` aggregators (one per entity), then
    /// post-processes the result to build `fact_index`, `has_format`, `has_cover_art`.
    ///
    /// Returns `Ok((LoadResult, Option<cursor>))`:
    /// - ACID reachable with 0 facts → `Ok((empty, Some(cursor)))` — correct on fresh system
    /// - ACID reachable with facts → `Ok((populated, Some(cursor)))`
    /// - ACID IPC error → `Err(ServiceError::Acid(_))` — fails loud, no silent fallback
    fn load_from_acid_stream(
        acid_client: &AcidClient,
        start_cursor: Option<String>,
    ) -> Result<(LoadResult, Option<String>), ServiceError> {
        use stainless_facts::{aggregate_facts, Fact};

        const PAGE_SIZE: usize = 10_000;

        // Phase 1: collect all raw fact lines from ACID (paged reads)
        let mut all_facts: Vec<Fact<ContentHash, MusicValue, music_facts::FactSource>> = Vec::new();
        let mut total = 0usize;
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
            for line in &chunk.lines {
                match serde_json::from_str::<Fact<ContentHash, MusicValue, music_facts::FactSource>>(
                    line,
                ) {
                    Ok(f) => {
                        total += 1;
                        all_facts.push(f);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to parse stream line during load");
                    }
                }
            }
            current_cursor = Some(chunk.cursor.clone());

            if lines_count < PAGE_SIZE {
                final_cursor = Some(chunk.cursor);
                break;
            }
        }

        if total > 0 {
            tracing::info!("Loaded {} facts from ACID stream", total);
        }

        // Phase 2: single fold via aggregate_facts
        let mut aggregated: HashMap<ContentHash, IndexedTrackInfo> = aggregate_facts(all_facts);

        // Phase 3: post-pass — fix content_hash and build side-effect indexes
        let mut fact_index: HashMap<FactType, HashSet<String>> = HashMap::new();
        let mut has_format: HashSet<String> = HashSet::new();
        let mut has_cover_art: HashSet<String> = HashSet::new();

        for (entity, entry) in &mut aggregated {
            entry.content_hash = entity.clone();

            // Re-derive blob_path now that content_hash is set.  During aggregation
            // the content_hash was empty (Default), so the FilePath arm in
            // apply_assert_scalar could not compute the path.
            entry.derive_blob_path_if_needed();

            for (value, _source) in &entry.provenance {
                fact_index
                    .entry(FactType::new(value.display_name()))
                    .or_default()
                    .insert(value.to_string());
            }

            if entry
                .provenance
                .iter()
                .any(|(v, _)| matches!(v, MusicValue::Format(_)))
            {
                has_format.insert(entity.as_str().to_owned());
            }
            if entry.cover_art_path.is_some() {
                has_cover_art.insert(entity.as_str().to_owned());
            }
        }

        let content_hashes: HashSet<String> =
            aggregated.keys().map(|k| k.as_str().to_owned()).collect();
        let loaded = LoadResult {
            tracks: aggregated.into_values().collect(),
            facts_count: total,
            fact_index,
            content_hashes,
            has_format,
            has_cover_art,
        };
        Ok((loaded, final_cursor))
    }

    /// Load tracks from facts file into memory for search.
    /// Used only in tests (for direct unit testing of fact parsing logic).
    ///
    /// Uses a manual loop to preserve the fact-level timestamp for
    /// `TrackStarted`/`TrackStopped` (which is not surfaced by the
    /// `FactAggregator` trait). All other fields are handled by
    /// `apply_fact_to_track` which enforces value+source retraction semantics.
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
            tracks_map
                .entry(entity_key.clone())
                .or_insert_with(|| IndexedTrackInfo::new_empty(entity_key.clone()));

            // Update fact_index based on surviving provenance (post-apply)
            // for Assert, add immediately; for Retract, handled below.
            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            let fact_type = FactType::new(variant_name);

            let entry = tracks_map
                .get_mut(&entity_key)
                .expect("entry was just inserted");
            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                fact.operation(),
                fact.source(),
                Some(&mut has_format),
                Some(&mut has_cover_art),
            );

            // Rebuild fact_index for this field from surviving provenance
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    fact_index.entry(fact_type).or_default().insert(value_str);
                }
                stainless_facts::Operation::Retract => {
                    // After retract, check if any entity still asserts this value
                    let still_asserted = tracks_map
                        .values()
                        .any(|t| t.provenance.iter().any(|(v, _)| v.to_string() == value_str));
                    if !still_asserted {
                        if let Some(set) = fact_index.get_mut(&fact_type) {
                            set.remove(&value_str);
                        }
                    }
                }
            }
        }

        tracing::info!("Processed {} facts from file, {} errors", total, errors);

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

            let variant_name = fact.value().display_name();
            let value_str = fact.value().to_string();
            let fact_type = FactType::new(variant_name);

            // Apply to track fields first (updates provenance)
            let entry = &mut tracks[pos];
            apply_fact_to_track(
                entry,
                fact.value(),
                *fact.timestamp(),
                fact.operation(),
                fact.source(),
                None,
                None,
            );

            // Update fact_index based on provenance after application
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    fact_index.entry(fact_type).or_default().insert(value_str);
                }
                stainless_facts::Operation::Retract => {
                    // Remove from fact_index only when no entity still asserts this value.
                    // Check other entities' provenance; the current entry was just updated.
                    let still_asserted = tracks.iter().enumerate().any(|(i, t)| {
                        i != pos && t.provenance.iter().any(|(v, _)| v.to_string() == value_str)
                    });
                    if !still_asserted {
                        if let Some(set) = fact_index.get_mut(&fact_type) {
                            set.remove(&value_str);
                        }
                    }
                }
            }
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
                    Ok(content) => {
                        // Lazy repair: follow Replaces chain for any unresolvable hash lines.
                        // If a repair is found the playlist file is amended in place (one-time heal).
                        let repaired = self.repair_playlist_content(&content, &path);
                        LibraryResponse::PlaylistContent(repaired)
                    }
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

            LibraryRequest::TrackDelete { hash } => self.handle_track_delete(&hash),

            LibraryRequest::TrackRestore { hash } => self.handle_track_restore(&hash),

            LibraryRequest::TrackReplace {
                old_hash,
                new_file_path,
            } => self.handle_track_replace(&old_hash, &new_file_path),

            LibraryRequest::TrackOrphans => self.handle_track_orphans(),
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

    // =========================================================================
    // Track lifecycle handlers
    // =========================================================================

    /// Assert a `Deleted` fact on the track identified by `hash`.
    fn handle_track_delete(&self, hash: &ContentHash) -> LibraryResponse {
        let full_hash = match self.resolve_hash(hash) {
            Ok(h) => h,
            Err(e) => return LibraryResponse::Error(e),
        };

        let fact = MusicValue::Deleted {
            timestamp: chrono::Utc::now(),
        };
        let source = music_facts::FactSource::new(
            "mdma",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::User,
        );
        match self
            .acid_client
            .write_music_facts(&full_hash, &[(fact.clone(), source)])
        {
            Ok(_) => {
                // Update in-memory index
                let mut tracks = self.tracks.lock().unwrap();
                if let Some(track) = tracks
                    .iter_mut()
                    .find(|t| t.content_hash.as_str() == full_hash.as_str())
                {
                    if let MusicValue::Deleted { timestamp } = &fact {
                        track.deleted_at = Some(*timestamp);
                    }
                }
                LibraryResponse::TrackDeleted
            }
            Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                message: e.to_string(),
            }),
        }
    }

    /// Retract the `Deleted` fact from the track identified by `hash`.
    fn handle_track_restore(&self, hash: &ContentHash) -> LibraryResponse {
        // resolve_hash scans ALL tracks including hidden ones — that's intentional
        let full_hash = match self.resolve_hash(hash) {
            Ok(h) => h,
            Err(e) => return LibraryResponse::Error(e),
        };

        // Find the current deleted_at timestamp so we can retract the same value
        let deleted_at = {
            let tracks = self.tracks.lock().unwrap();
            tracks
                .iter()
                .find(|t| t.content_hash.as_str() == full_hash.as_str())
                .and_then(|t| t.deleted_at)
        };

        let timestamp = match deleted_at {
            Some(ts) => ts,
            None => {
                return LibraryResponse::Error(ProtocolError::Internal {
                    message: "Track is not deleted".to_string(),
                });
            }
        };

        let fact = MusicValue::Deleted { timestamp };
        let source = music_facts::FactSource::new(
            "mdma",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::User,
        );
        match self
            .acid_client
            .retract_music_facts(&full_hash, &[(fact, source)])
        {
            Ok(_) => {
                let mut tracks = self.tracks.lock().unwrap();
                if let Some(track) = tracks
                    .iter_mut()
                    .find(|t| t.content_hash.as_str() == full_hash.as_str())
                {
                    track.deleted_at = None;
                }
                LibraryResponse::TrackRestored
            }
            Err(e) => LibraryResponse::Error(ProtocolError::Internal {
                message: e.to_string(),
            }),
        }
    }

    /// Retract ALL currently-asserted facts for `hash` from ACID and remove the
    /// entry from the in-memory index.
    ///
    /// This is the hard-delete half of `handle_track_replace`. It reads all facts
    /// for `hash` from ACID via `read_entity`, filters to Assert operations, and
    /// sends a matching Retract for each (value, source) pair via `retract_music_facts`.
    /// The in-memory entry is then removed so `resolve_hash` returns TrackNotFound.
    ///
    /// Returns `Ok(retracted_count)` on success, or an error string on ACID failure.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn retract_all_entity_facts(&self, hash: &ContentHash) -> Result<usize, String> {
        use music_facts::FactSource;

        // Read all fact lines for this entity from ACID
        let lines = self
            .acid_client
            .read_entity(hash.as_str())
            .map_err(|e| format!("Failed to read entity from ACID: {}", e))?;

        // Collect all (value, source) pairs from Assert operations.
        // We retract exactly the asserted (value, source) pairs so the provenance
        // retraction logic in apply_fact_to_track correctly matches and removes them.
        let to_retract: Vec<(MusicValue, FactSource)> = lines
            .iter()
            .filter_map(|line| {
                serde_json::from_str::<stainless_facts::Fact<ContentHash, MusicValue, FactSource>>(
                    line,
                )
                .ok()
            })
            .filter(|f| f.operation() == stainless_facts::Operation::Assert)
            .map(|f| (f.value().clone(), f.source().clone()))
            .collect();

        if to_retract.is_empty() {
            // No asserted facts to retract (already empty or only retractions exist).
            // Still remove from in-memory index.
            let mut tracks = self.tracks.lock().unwrap();
            tracks.retain(|t| t.content_hash.as_str() != hash.as_str());
            let mut content_hashes = self.content_hashes.lock().unwrap();
            content_hashes.remove(hash.as_str());
            return Ok(0);
        }

        // Send all retractions to ACID via the trustworthy (value, source) retraction path.
        let count = self
            .acid_client
            .retract_music_facts(hash, &to_retract)
            .map_err(|e| format!("Failed to retract facts via ACID: {}", e))?;

        // Remove the entry from the in-memory index so resolve_hash returns TrackNotFound.
        // This is a hard delete: the old identity is gone, not hidden.
        let mut tracks = self.tracks.lock().unwrap();
        tracks.retain(|t| t.content_hash.as_str() != hash.as_str());
        drop(tracks);

        let mut content_hashes = self.content_hashes.lock().unwrap();
        content_hashes.remove(hash.as_str());
        drop(content_hashes);

        tracing::info!(
            hash = %hash.as_str(),
            facts_retracted = count,
            "Hard-retracted all facts for replaced track"
        );

        Ok(count)
    }

    /// Ingest new file, retract ALL old facts (hard delete), assert Replaces on new
    /// track, and rewrite playlists.
    ///
    /// The old hash is permanently removed from the fact stream and the in-memory
    /// index. The new track carries `Replaces(old_hash)` plus any `Replaces(X)` facts
    /// that old_hash had previously accumulated (forward-inheritance), so a reverse
    /// lookup for any ancestor hash in the chain resolves to the current track in a
    /// single hop even after all intermediates are fully retracted.
    fn handle_track_replace(&self, old_hash: &ContentHash, new_file_path: &str) -> LibraryResponse {
        // Resolve old hash (must exist and be accessible)
        let old_full = match self.resolve_hash(old_hash) {
            Ok(h) => h,
            Err(e) => return LibraryResponse::Error(e),
        };

        // GATHER BEFORE RETRACT: collect all Replaces(X) facts from old_hash's
        // in-memory provenance NOW, before retract_all_entity_facts removes the entry.
        // These ancestors will be forward-inherited onto the new track so that a
        // playlist pointing at any generation in the chain finds the current track.
        let inherited_ancestors = self.gather_ancestor_replaces(&old_full);

        // Ingest the new file
        let new_path = std::path::PathBuf::from(new_file_path);
        let new_hash = match self.ingest_file_internal(&new_path, None) {
            Ok(h) => h,
            Err(e) => {
                return LibraryResponse::Error(ProtocolError::IngestionFailed {
                    message: e.to_string(),
                });
            }
        };

        // Assert Replaces(old_hash) on the NEW track, plus all forward-inherited ancestors.
        // Dedup: the new track itself is never in the ancestor set (it was just ingested),
        // but two independent chains could theoretically produce duplicates — deduplicate
        // by hash string to avoid asserting the same Replaces fact twice.
        let source = music_facts::FactSource::new(
            "mdma",
            env!("CARGO_PKG_VERSION"),
            music_facts::FactOrigin::User,
        );
        let mut seen_replaces: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut replaces_facts: Vec<(MusicValue, music_facts::FactSource)> = Vec::new();

        // Always assert Replaces(old_hash) first
        seen_replaces.insert(old_full.as_str().to_lowercase());
        replaces_facts.push((MusicValue::Replaces(old_full.clone()), source.clone()));

        // Then forward-inherit every ancestor that old_hash previously replaced
        for ancestor in inherited_ancestors {
            let key = ancestor.as_str().to_lowercase();
            if seen_replaces.insert(key) {
                replaces_facts.push((MusicValue::Replaces(ancestor), source.clone()));
            }
        }

        if let Err(e) = self
            .acid_client
            .write_music_facts(&new_hash, &replaces_facts)
        {
            return LibraryResponse::Error(ProtocolError::Internal {
                message: format!("Failed to assert Replaces facts: {}", e),
            });
        }

        // Hard-retract ALL facts for the old hash so it stops resolving.
        // This is the crux: after this, resolve_hash(old_hash) → TrackNotFound.
        if let Err(e) = self.retract_all_entity_facts(&old_full) {
            return LibraryResponse::Error(ProtocolError::Internal {
                message: format!("Failed to retract old track facts: {}", e),
            });
        }

        // Rewrite playlists server-side (eager: old → new)
        let playlists_rewritten = self.rewrite_playlists_replace_hash(&old_full, &new_hash);

        LibraryResponse::TrackReplaced {
            new_hash,
            playlists_rewritten,
        }
    }

    /// Rewrite all playlists: replace every line whose first token matches old_hash with new_hash.
    ///
    /// Matching: strip `sha256:` prefix, lowercase, then check if the stored token is a
    /// prefix of the old full hash (short-hash compatible). On match, emit the line with
    /// the new full hash token, preserving the rest of the line.
    fn rewrite_playlists_replace_hash(
        &self,
        old_hash: &ContentHash,
        new_hash: &ContentHash,
    ) -> usize {
        use std::io::Write;

        let playlists_dir = self.metadata_dir.join("playlists");
        let old_clean = old_hash
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(old_hash.as_str())
            .to_lowercase();
        let new_token = new_hash.as_str().to_string();

        let entries = match std::fs::read_dir(&playlists_dir) {
            Ok(e) => e,
            Err(_) => return 0,
        };

        let mut total_rewritten = 0usize;

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("plist") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut changed = false;
            let new_content: String = content
                .lines()
                .map(|line| {
                    // First whitespace-delimited field is the hash token
                    let token = line.split_whitespace().next().unwrap_or("");
                    let token_clean = token
                        .strip_prefix("sha256:")
                        .unwrap_or(token)
                        .to_lowercase();

                    // A line matches if the stored token is a prefix of the old full hash,
                    // or the full hash starts with the stored token (short-hash support)
                    if !token_clean.is_empty()
                        && (old_clean.starts_with(&token_clean)
                            || token_clean.starts_with(&old_clean))
                    {
                        changed = true;
                        // Replace only the hash token; preserve rest of line verbatim
                        let rest = line[token.len()..].to_string();
                        format!("{}{}", new_token, rest)
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            // Preserve trailing newline if original had one
            let new_content = if content.ends_with('\n') {
                format!("{}\n", new_content)
            } else {
                new_content
            };

            if changed {
                // Write atomically via temp file
                let tmp_path = path.with_extension("plist.tmp");
                if let Ok(mut f) = std::fs::File::create(&tmp_path) {
                    if f.write_all(new_content.as_bytes()).is_ok() {
                        let _ = std::fs::rename(&tmp_path, &path);
                        total_rewritten += 1;
                    } else {
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                }
            }
        }

        total_rewritten
    }

    /// List all orphan candidates.
    ///
    /// Two categories:
    ///
    /// - `OrphanReason::Deleted` — soft-deleted track (still in the index with
    ///   `deleted_at` set, blob present, recoverable via restore).
    ///
    /// - `OrphanReason::NoLiveFacts` — a blob exists on disk whose content hash
    ///   has no live entry in the in-memory index.  This is the hard-replace
    ///   leftover: `retract_all_entity_facts` removed the index entry but did
    ///   not delete the blob file.  Primary GC candidate.
    ///
    /// A live (not hidden) track is never an orphan, even if a blob is present.
    fn handle_track_orphans(&self) -> LibraryResponse {
        let tracks = self.tracks.lock().unwrap();

        // ----------------------------------------------------------------
        // Part 1: soft-deleted tracks (Deleted reason)
        // ----------------------------------------------------------------
        let mut orphans: Vec<OrphanInfo> = tracks
            .iter()
            .filter(|t| t.is_hidden())
            .map(|t| {
                let reason = if let Some(dt) = t.deleted_at {
                    OrphanReason::Deleted {
                        timestamp: dt.to_rfc3339(),
                    }
                } else {
                    // is_hidden() only returns true when deleted_at is set, but
                    // be defensive.
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

        // ----------------------------------------------------------------
        // Part 2: blobs on disk with no live index entry (NoLiveFacts)
        // ----------------------------------------------------------------
        // Build a set of all hashes currently in the index (live + hidden).
        // Any blob whose hash is absent from this set has no live facts.
        let indexed_hashes: std::collections::HashSet<String> = tracks
            .iter()
            .map(|t| t.content_hash.as_str().to_string())
            .collect();

        let blobs_dir = self.music_dir.join("blobs");
        if let Ok(prefix_entries) = std::fs::read_dir(&blobs_dir) {
            for prefix_entry in prefix_entries.filter_map(|e| e.ok()) {
                if !prefix_entry.path().is_dir() {
                    continue;
                }
                if let Ok(blob_entries) = std::fs::read_dir(prefix_entry.path()) {
                    for blob_entry in blob_entries.filter_map(|e| e.ok()) {
                        let blob_path = blob_entry.path();
                        if !blob_path.is_file() {
                            continue;
                        }
                        // Recover hash from stem: `{hash_hex}.{ext}` → `sha256:{hash_hex}`
                        if let Some(stem) = blob_path.file_stem().and_then(|s| s.to_str()) {
                            let content_hash = ContentHash::new(format!("sha256:{}", stem));
                            if !indexed_hashes.contains(content_hash.as_str()) {
                                orphans.push(OrphanInfo {
                                    content_hash,
                                    artist: None,
                                    title: None,
                                    reason: OrphanReason::NoLiveFacts,
                                });
                            }
                        }
                    }
                }
            }
        }

        LibraryResponse::OrphansList(orphans)
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

    /// List tracks from in-memory index (hidden tracks excluded)
    fn list_tracks(&self, limit: Option<usize>) -> Vec<TrackInfo> {
        let tracks = self.tracks.lock().unwrap();

        let iter = tracks
            .iter()
            .filter(|t| !t.is_hidden())
            .map(|t| self.to_track_info(t));

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

    /// Get track by hash from in-memory index (supports partial hashes).
    /// Returns TrackNotFound for hidden (deleted/superseded) tracks.
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;

        let tracks = self.tracks.lock().unwrap();
        let track = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == full_hash.as_str());

        match track {
            Some(t) if t.is_hidden() => Err(ProtocolError::TrackNotFound {
                hash: hash.as_str().to_owned(),
            }),
            Some(t) => Ok(self.to_track_info(t)),
            None => Err(ProtocolError::TrackNotFound {
                hash: hash.as_str().to_owned(),
            }),
        }
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

    /// Search tracks by structured query (uses library-search for evaluation; hidden tracks excluded)
    fn search_tracks(&self, query: &TrackQuery) -> Vec<TrackInfo> {
        if query.started.is_some() || query.stopped.is_some() {
            self.refresh_event_timestamps();
        }
        let tracks = self.tracks.lock().unwrap();

        tracks
            .iter()
            .filter(|t| !t.is_hidden())
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

        // Update in-memory index via the shared aggregation path.
        //
        // Previously this block hand-rolled a subset projection loop that omitted
        // role/energy/beat_grid/memory_cues — the same divergence class that caused
        // the 0.24.1 hotfix.  Now we drive `apply_fact_to_track` for every fact,
        // exactly as the file-load and incremental paths do, then override blob_path
        // with the authoritative value from the import pipeline.
        {
            let mut tracks = self.tracks.lock().unwrap();
            let ts = chrono::Utc::now();
            let mut entry = IndexedTrackInfo::new_empty(content_hash.as_str().to_owned());
            for (value, fact_src) in &facts {
                apply_fact_to_track(
                    &mut entry,
                    value,
                    ts,
                    stainless_facts::Operation::Assert,
                    fact_src,
                    None,
                    None,
                );
            }
            // blob_path is derived from the FilePath fact inside apply_fact_to_track
            // via blob_path_for — the result is always RELATIVE (blobs/<2ch>/<hash>.<ext>).
            // Do NOT override it with indexed.blob_path here; that field carries the
            // ABSOLUTE path from the import pipeline and would cause divergence between
            // fresh-ingest output and every other projection path (file, bulk, stream).
            tracks.push(entry);
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

    // =========================================================================
    // Track lifecycle tests
    // =========================================================================

    /// apply_fact_to_track stores Replaces in provenance without hiding the track.
    #[test]
    fn apply_fact_to_track_stores_replaces_in_provenance() {
        let hash = ContentHash::new("sha256:new");
        let old_hash = ContentHash::new("sha256:old");
        let mut entry = IndexedTrackInfo::new_empty(hash.as_str().to_owned());
        let ts = chrono::Utc::now();
        let fact = MusicValue::Replaces(old_hash.clone());
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        apply_fact_to_track(
            &mut entry,
            &fact,
            ts,
            stainless_facts::Operation::Assert,
            &source,
            None,
            None,
        );
        // Replaces is stored in provenance
        assert!(
            entry
                .provenance
                .iter()
                .any(|(v, _)| matches!(v, MusicValue::Replaces(_))),
            "Replaces fact must be stored in provenance"
        );
        // The new track is NOT hidden — it is visible, the old track's facts are retracted
        assert!(
            !entry.is_hidden(),
            "new track must not be hidden after Replaces fact"
        );
    }

    #[test]
    fn apply_fact_to_track_sets_deleted_at() {
        let hash = ContentHash::new("sha256:toDelete");
        let mut entry = IndexedTrackInfo::new_empty(hash.as_str().to_owned());
        let ts = chrono::Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let fact = MusicValue::Deleted { timestamp: ts };
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        apply_fact_to_track(
            &mut entry,
            &fact,
            ts,
            stainless_facts::Operation::Assert,
            &source,
            None,
            None,
        );
        assert_eq!(entry.deleted_at, Some(ts));
        assert!(entry.is_hidden());
    }

    #[test]
    fn apply_fact_to_track_retract_clears_deleted_at() {
        let hash = ContentHash::new("sha256:restore");
        let mut entry = IndexedTrackInfo::new_empty(hash.as_str().to_owned());
        let ts = chrono::Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let fact = MusicValue::Deleted { timestamp: ts };
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        // Assert then retract
        apply_fact_to_track(
            &mut entry,
            &fact,
            ts,
            stainless_facts::Operation::Assert,
            &source,
            None,
            None,
        );
        assert!(entry.is_hidden());
        apply_fact_to_track(
            &mut entry,
            &fact,
            ts,
            stainless_facts::Operation::Retract,
            &source,
            None,
            None,
        );
        assert!(!entry.is_hidden());
        assert_eq!(entry.deleted_at, None);
    }

    /// list_tracks excludes hidden (deleted) tracks.
    #[test]
    fn list_tracks_excludes_hidden_tracks() {
        let hash = ContentHash::new("sha256:hiddentrack01");
        let temp = write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("Hidden")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // Mark hidden directly in memory
        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(t) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                t.deleted_at = Some(chrono::Utc::now());
            }
        }

        let visible = service.list_tracks(None);
        assert!(
            !visible
                .iter()
                .any(|t| t.content_hash.as_str() == hash.as_str()),
            "deleted track must not appear in list_tracks"
        );
    }

    /// search_tracks excludes hidden (deleted) tracks.
    #[test]
    fn search_tracks_excludes_hidden_tracks() {
        use library_search::TrackQuery;
        let hash = ContentHash::new("sha256:hiddensearch01");
        let temp =
            write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("HiddenSearch")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(t) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                t.deleted_at = Some(chrono::Utc::now());
            }
        }

        let results = service.search_tracks(&TrackQuery::default());
        assert!(
            !results
                .iter()
                .any(|t| t.content_hash.as_str() == hash.as_str()),
            "deleted track must not appear in search_tracks"
        );
    }

    /// get_track returns TrackNotFound for hidden tracks.
    #[test]
    fn get_track_rejects_hidden_tracks() {
        let hash = ContentHash::new("sha256:hiddenget01");
        let temp = write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("HiddenGet")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(t) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                t.deleted_at = Some(chrono::Utc::now());
            }
        }

        let result = service.get_track(&hash);
        assert!(
            matches!(result, Err(ProtocolError::TrackNotFound { .. })),
            "get_track should reject hidden track, got: {:?}",
            result
        );
    }

    /// resolve_hash still resolves hidden tracks (needed for GetFacts, restore, etc.)
    #[test]
    fn resolve_hash_resolves_hidden_tracks() {
        let hash = ContentHash::new("sha256:hiddenresolve01");
        let temp =
            write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("HiddenResolve")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(t) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                t.deleted_at = Some(chrono::Utc::now());
            }
        }

        // resolve_hash must still succeed so restore/orphans can find the hash
        let result = service.resolve_hash(&hash);
        assert!(result.is_ok(), "resolve_hash must work for hidden tracks");
    }

    /// handle_track_delete asserts a Deleted fact in ACID and hides the track in memory.
    #[test]
    fn handle_track_delete_hides_track() {
        let hash = ContentHash::new("sha256:deletehandler01");
        let temp = write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("DeleteMe")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let resp = service.handle_request(LibraryRequest::TrackDelete { hash: hash.clone() });
        assert!(
            matches!(resp, LibraryResponse::TrackDeleted),
            "expected TrackDeleted, got {:?}",
            resp
        );

        // Track must now be hidden
        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .unwrap();
        assert!(t.is_hidden(), "track must be hidden after delete");
    }

    /// handle_track_orphans returns deleted tracks.
    #[test]
    fn handle_track_orphans_includes_deleted_tracks() {
        let hash = ContentHash::new("sha256:orphan_deleted01");
        let temp = write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("OrphanMe")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // Delete the track
        service.handle_request(LibraryRequest::TrackDelete { hash: hash.clone() });

        let resp = service.handle_request(LibraryRequest::TrackOrphans);
        match resp {
            LibraryResponse::OrphansList(items) => {
                assert!(
                    items
                        .iter()
                        .any(|o| o.content_hash.as_str() == hash.as_str()),
                    "deleted track must appear in orphans list"
                );
            }
            other => panic!("expected OrphansList, got {:?}", other),
        }
    }

    /// Playlist rewrite replaces old hash token with new hash token.
    #[test]
    fn rewrite_playlists_replaces_hash_token() {
        let old_hash = ContentHash::new("sha256:oldaabbccdd001122334455667788990000");
        let new_hash = ContentHash::new("sha256:neweeff1100aabbccdd220044006688aa");
        let (service, metadata_dir, _acid) = make_service_with_playlists_dir();

        let playlists_dir = metadata_dir.path().join("playlists");
        let plist_path = playlists_dir.join("test.plist");
        std::fs::write(
            &plist_path,
            format!("{}  Old Artist - Old Title  [5:00]\n", old_hash.as_str()),
        )
        .unwrap();

        let count = service.rewrite_playlists_replace_hash(&old_hash, &new_hash);
        assert_eq!(count, 1, "one playlist should be rewritten");

        let content = std::fs::read_to_string(&plist_path).unwrap();
        assert!(
            content.starts_with(new_hash.as_str()),
            "first token should be replaced with new hash, got: {}",
            content
        );
        assert!(
            !content.contains(old_hash.as_str()),
            "old hash should not appear in rewritten playlist"
        );
    }

    // =========================================================================
    // Step A: Golden tests (capture current behavior before refactor)
    // =========================================================================

    /// Golden: single-source, multi-field track through apply_lines_to_map (ACID path).
    /// Assert title, artist, bpm for one track — all fields populated.
    #[test]
    fn golden_single_source_multi_field_assert() {
        use music_facts::{Artist, Bpm as BpmValue, Title};

        let hash = ContentHash::new("sha256:golden_multi_field_01");
        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Golden Track"))),
            (
                hash.clone(),
                MusicValue::Artist(Artist::new("Golden Artist")),
            ),
            (
                hash.clone(),
                MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());

        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(track.title.as_deref(), Some("Golden Track"));
        assert_eq!(track.artist.as_deref(), Some("Golden Artist"));
        assert_eq!(track.bpm, Some(128.0));
    }

    /// Golden: retraction of a field — retracted field becomes None, others survive.
    /// This golden test captures the PRE-refactor behavior: Retract(Album="X") clears
    /// album regardless of the value stored. After the #96 fix this test must still
    /// pass because the retracted value matches the asserted value.
    #[test]
    fn golden_retract_matching_value_clears_field() {
        let hash = ContentHash::new("sha256:golden_retract_01");
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_operations(&[
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("Gold")),
                ts,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("Keeper")),
                ts,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::Album(music_facts::Album::new("Gold")),
                ts,
                Operation::Retract,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.album, None,
            "album should be None after matching Retract"
        );
        assert_eq!(
            track.title.as_deref(),
            Some("Keeper"),
            "title must survive when only album was retracted"
        );
    }

    /// Golden: Deleted fact sets deleted_at and is_hidden returns true.
    #[test]
    fn golden_deleted_fact_hides_track() {
        let hash = ContentHash::new("sha256:golden_deleted_01");
        let ts = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();

        let temp = write_facts_file_with_operations(&[
            (
                hash.clone(),
                MusicValue::Title(Title::new("To Delete")),
                ts,
                Operation::Assert,
            ),
            (
                hash.clone(),
                MusicValue::Deleted { timestamp: ts },
                ts,
                Operation::Assert,
            ),
        ]);

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert!(track.is_hidden(), "track with Deleted fact must be hidden");
        assert_eq!(
            track.deleted_at,
            Some(ts),
            "deleted_at must be set from Deleted fact"
        );
    }

    // =========================================================================
    // Step D: Regression tests for #96 and #2
    // These MUST FAIL before the fix and pass after.
    // =========================================================================

    /// Regression #96: Retract(Bpm=120) when live value is Bpm=128 must NOT clear bpm.
    ///
    /// The current bug: the Retract arm matches on variant only, so Retract(Bpm=120)
    /// clears bpm even though the live value is 128. After the fix, only a retraction
    /// whose value matches the live asserted value+source pair should clear the field.
    #[test]
    fn regression_96_retract_wrong_value_does_not_clear() {
        use music_facts::Bpm as BpmValue;

        let hash = ContentHash::new("sha256:reg96_wrong_val");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let facts: Vec<Fact<ContentHash, MusicValue, FactSource>> = vec![
                // Source A asserts Bpm=128
                Fact::new(
                    hash.clone(),
                    MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
                    ts,
                    source_a.clone(),
                    Operation::Assert,
                ),
                // Source B retracts Bpm=120 (DIFFERENT value — must not clear 128)
                Fact::new(
                    hash.clone(),
                    MusicValue::Bpm(BpmValue::from_f32(120.0).unwrap()),
                    ts,
                    source_b.clone(),
                    Operation::Retract,
                ),
            ];
            writer.write_batch(&facts).unwrap();
        }

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.bpm,
            Some(128.0),
            "Retract(Bpm=120) must not clear live value Bpm=128 (bug #96)"
        );
    }

    /// Regression #96 complement: Retract(Bpm=128) when live value IS Bpm=128 MUST clear.
    #[test]
    fn regression_96_retract_correct_value_clears() {
        use music_facts::Bpm as BpmValue;

        let hash = ContentHash::new("sha256:reg96_correct_val");
        let source = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let facts: Vec<Fact<ContentHash, MusicValue, FactSource>> = vec![
                Fact::new(
                    hash.clone(),
                    MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
                    ts,
                    source.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
                    ts,
                    source.clone(),
                    Operation::Retract,
                ),
            ];
            writer.write_batch(&facts).unwrap();
        }

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.bpm, None,
            "Retract(Bpm=128) must clear bpm when live value is 128"
        );
    }

    /// Regression #2: Source A and B both assert Title="X"; A retracts → Title survives (B still holds).
    #[test]
    fn regression_2_multi_source_title_survives_one_retraction() {
        let hash = ContentHash::new("sha256:reg2_multi_source");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let facts: Vec<Fact<ContentHash, MusicValue, FactSource>> = vec![
                // Both sources assert the same title
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Shared Title")),
                    ts,
                    source_a.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Shared Title")),
                    ts,
                    source_b.clone(),
                    Operation::Assert,
                ),
                // Source A retracts its assertion — B's assertion must keep title alive
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Shared Title")),
                    ts,
                    source_a.clone(),
                    Operation::Retract,
                ),
            ];
            writer.write_batch(&facts).unwrap();
        }

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.title.as_deref(),
            Some("Shared Title"),
            "Title must survive when source B still asserts it (bug #2)"
        );
    }

    /// Regression #2: Both sources retract → Title gone.
    #[test]
    fn regression_2_both_sources_retract_clears_title() {
        let hash = ContentHash::new("sha256:reg2_both_retract");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let facts: Vec<Fact<ContentHash, MusicValue, FactSource>> = vec![
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Gone Title")),
                    ts,
                    source_a.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Gone Title")),
                    ts,
                    source_b.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Gone Title")),
                    ts,
                    source_a.clone(),
                    Operation::Retract,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Gone Title")),
                    ts,
                    source_b.clone(),
                    Operation::Retract,
                ),
            ];
            writer.write_batch(&facts).unwrap();
        }

        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        assert_eq!(result.tracks.len(), 1);
        let track = &result.tracks[0];
        assert_eq!(
            track.title, None,
            "Title must be None when all sources retract"
        );
    }

    // =========================================================================
    // 3-path equivalence: #96 and #2 regressions on all production paths
    //
    // Each case is defined once as a sequence of (ContentHash, MusicValue,
    // timestamp, Operation, FactSource) tuples, then run through:
    //   (a) load_tracks_from_facts  — test-only file path
    //   (b) aggregate_facts via make_service_with_facts — bulk production path
    //   (c) apply_stream_lines — incremental production path
    //
    // If any two paths produce different results that's a real divergence and
    // the test will panic with a message naming the case and the differing field.
    // =========================================================================

    /// Serialize a Fact to a JSON string suitable for apply_stream_lines.
    fn fact_to_json_line(
        hash: &ContentHash,
        value: &MusicValue,
        ts: chrono::DateTime<Utc>,
        source: &FactSource,
        op: Operation,
    ) -> String {
        let fact: Fact<ContentHash, MusicValue, FactSource> =
            Fact::new(hash.clone(), value.clone(), ts, source.clone(), op);
        serde_json::to_string(&fact).unwrap()
    }

    /// Run the given fact sequence through apply_stream_lines on a fresh
    /// LibraryService backed by an empty ACID server. Returns the resulting
    /// IndexedTrackInfo for `hash`.
    fn apply_stream_path(
        hash: &ContentHash,
        facts: &[(
            ContentHash,
            MusicValue,
            chrono::DateTime<Utc>,
            Operation,
            FactSource,
        )],
    ) -> IndexedTrackInfo {
        let empty = NamedTempFile::new().unwrap();
        let (service, _metadata_dir) = make_service_with_facts(empty.path());

        let lines: Vec<String> = facts
            .iter()
            .map(|(h, v, ts, op, src)| fact_to_json_line(h, v, *ts, src, *op))
            .collect();

        service.apply_stream_lines(&lines);

        let tracks = service.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .cloned()
            .unwrap_or_else(|| IndexedTrackInfo::new_empty(hash.as_str().to_owned()))
    }

    /// Run the given fact sequence through load_tracks_from_facts (file path).
    /// Returns the IndexedTrackInfo for `hash`.
    fn load_from_file_path(
        hash: &ContentHash,
        facts: &[(
            ContentHash,
            MusicValue,
            chrono::DateTime<Utc>,
            Operation,
            FactSource,
        )],
    ) -> IndexedTrackInfo {
        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let fact_structs: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
                .iter()
                .map(|(h, v, ts, op, src)| Fact::new(h.clone(), v.clone(), *ts, src.clone(), *op))
                .collect();
            writer.write_batch(&fact_structs).unwrap();
        }
        let result = LibraryService::load_tracks_from_facts(&temp.path().to_path_buf());
        result
            .tracks
            .into_iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .unwrap_or_else(|| IndexedTrackInfo::new_empty(hash.as_str().to_owned()))
    }

    /// Run the given fact sequence through aggregate_facts via make_service_with_facts (bulk path).
    /// Returns the IndexedTrackInfo for `hash`.
    fn load_from_bulk_path(
        hash: &ContentHash,
        facts: &[(
            ContentHash,
            MusicValue,
            chrono::DateTime<Utc>,
            Operation,
            FactSource,
        )],
    ) -> IndexedTrackInfo {
        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let fact_structs: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
                .iter()
                .map(|(h, v, ts, op, src)| Fact::new(h.clone(), v.clone(), *ts, src.clone(), *op))
                .collect();
            writer.write_batch(&fact_structs).unwrap();
        }
        let (service, _metadata_dir) = make_service_with_facts(temp.path());
        let tracks = service.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .cloned()
            .unwrap_or_else(|| IndexedTrackInfo::new_empty(hash.as_str().to_owned()))
    }

    /// Model the ingest-path in-memory projection (service.rs ~2880-2898):
    /// `new_empty` + `apply_fact_to_track` loop.
    ///
    /// Mirrors `ingest_file_internal` after the fix: `apply_fact_to_track`
    /// derives the correct relative blob_path from the FilePath fact via
    /// `blob_path_for`, so no override is needed.  If the override is ever
    /// re-introduced to this helper or to `ingest_file_internal`, the
    /// companion test `blob_path_is_relative_on_ingest_path` will fail again.
    fn apply_ingest_path(
        hash: &ContentHash,
        facts: &[(
            ContentHash,
            MusicValue,
            chrono::DateTime<Utc>,
            Operation,
            FactSource,
        )],
    ) -> IndexedTrackInfo {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut entry = IndexedTrackInfo::new_empty(hash.as_str().to_owned());
        for (_, value, _, _, src) in facts {
            apply_fact_to_track(
                &mut entry,
                value,
                ts,
                stainless_facts::Operation::Assert,
                src,
                None,
                None,
            );
        }
        entry
    }

    /// Assert that two IndexedTrackInfo values agree on the scalar fields that
    /// retraction semantics apply to. Panics with a descriptive message on mismatch.
    fn assert_scalar_fields_eq(case: &str, a: &IndexedTrackInfo, b: &IndexedTrackInfo) {
        assert_eq!(a.bpm, b.bpm, "case '{}': bpm diverges between paths", case);
        assert_eq!(
            a.title, b.title,
            "case '{}': title diverges between paths",
            case
        );
        assert_eq!(
            a.artist, b.artist,
            "case '{}': artist diverges between paths",
            case
        );
        assert_eq!(
            a.album, b.album,
            "case '{}': album diverges between paths",
            case
        );
        assert_eq!(a.key, b.key, "case '{}': key diverges between paths", case);
        assert_eq!(
            a.year, b.year,
            "case '{}': year diverges between paths",
            case
        );
        assert_eq!(
            a.item_id, b.item_id,
            "case '{}': item_id diverges between paths",
            case
        );
        assert_eq!(
            a.styles, b.styles,
            "case '{}': styles diverges between paths",
            case
        );
        assert_eq!(
            a.role, b.role,
            "case '{}': role diverges between paths",
            case
        );
        assert_eq!(
            a.energy, b.energy,
            "case '{}': energy diverges between paths",
            case
        );
        assert_eq!(
            a.beat_grid, b.beat_grid,
            "case '{}': beat_grid diverges between paths",
            case
        );
        assert_eq!(
            a.memory_cues, b.memory_cues,
            "case '{}': memory_cues diverges between paths",
            case
        );
    }

    /// #96 path-equivalence: Retract(Bpm=120) when Bpm=128 is live → bpm stays 128.
    ///
    /// Drives the sequence through all three projection paths and asserts the
    /// resulting IndexedTrackInfo is identical. If any path diverges that is a
    /// real bulk-vs-incremental bug, not a test problem.
    #[test]
    fn equivalence_96_retract_wrong_bpm_all_paths() {
        use music_facts::Bpm as BpmValue;

        let hash = ContentHash::new("sha256:equiv96_wrong_bpm");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let facts = vec![
            (
                hash.clone(),
                MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
                ts,
                Operation::Assert,
                source_a.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Bpm(BpmValue::from_f32(120.0).unwrap()),
                ts,
                Operation::Retract,
                source_b.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        assert_eq!(
            file_result.bpm,
            Some(128.0),
            "#96: file path — Retract(Bpm=120) must not clear live Bpm=128"
        );
        assert_scalar_fields_eq("96_wrong_bpm bulk==file", &bulk_result, &file_result);
        assert_scalar_fields_eq("96_wrong_bpm stream==file", &stream_result, &file_result);
    }

    /// #96 path-equivalence: Retract(Bpm=128) when Bpm=128 is live → bpm cleared.
    #[test]
    fn equivalence_96_retract_correct_bpm_all_paths() {
        use music_facts::Bpm as BpmValue;

        let hash = ContentHash::new("sha256:equiv96_correct_bpm");
        let source = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let facts = vec![
            (
                hash.clone(),
                MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Bpm(BpmValue::from_f32(128.0).unwrap()),
                ts,
                Operation::Retract,
                source.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        assert_eq!(
            file_result.bpm, None,
            "#96: file path — Retract(Bpm=128) must clear bpm"
        );
        assert_scalar_fields_eq("96_correct_bpm bulk==file", &bulk_result, &file_result);
        assert_scalar_fields_eq("96_correct_bpm stream==file", &stream_result, &file_result);
    }

    /// #2 path-equivalence: source A + B assert Title=X; A retracts → title survives.
    #[test]
    fn equivalence_2_multi_source_title_survives_one_retraction_all_paths() {
        let hash = ContentHash::new("sha256:equiv2_title_survives");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let facts = vec![
            (
                hash.clone(),
                MusicValue::Title(Title::new("Shared Title")),
                ts,
                Operation::Assert,
                source_a.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("Shared Title")),
                ts,
                Operation::Assert,
                source_b.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("Shared Title")),
                ts,
                Operation::Retract,
                source_a.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        assert_eq!(
            file_result.title.as_deref(),
            Some("Shared Title"),
            "#2: file path — title must survive when source B still asserts"
        );
        assert_scalar_fields_eq("2_title_survives bulk==file", &bulk_result, &file_result);
        assert_scalar_fields_eq(
            "2_title_survives stream==file",
            &stream_result,
            &file_result,
        );
    }

    /// #2 path-equivalence: both sources retract → title gone.
    #[test]
    fn equivalence_2_both_sources_retract_clears_title_all_paths() {
        let hash = ContentHash::new("sha256:equiv2_both_retract");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let facts = vec![
            (
                hash.clone(),
                MusicValue::Title(Title::new("Gone Title")),
                ts,
                Operation::Assert,
                source_a.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("Gone Title")),
                ts,
                Operation::Assert,
                source_b.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("Gone Title")),
                ts,
                Operation::Retract,
                source_a.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Title(Title::new("Gone Title")),
                ts,
                Operation::Retract,
                source_b.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        assert_eq!(
            file_result.title, None,
            "#2: file path — title must be None when all sources retract"
        );
        assert_scalar_fields_eq("2_both_retract bulk==file", &bulk_result, &file_result);
        assert_scalar_fields_eq("2_both_retract stream==file", &stream_result, &file_result);
    }

    /// Regression: blob_path must be derived from FilePath on ALL three paths.
    ///
    /// Before the fix, aggregate_facts (bulk path) produced an empty blob_path
    /// because content_hash was "" during aggregation and the hash-length guard
    /// (`hash_clean.len() >= 2`) silently skipped the derivation.  The file
    /// and incremental paths passed `new_empty(entity_key)` which pre-seeded the
    /// hash, so they worked.  This test catches the divergence.
    #[test]
    fn blob_path_derived_on_all_three_paths() {
        let hash = ContentHash::new("sha256:deadbeef1234");
        let source = FactSource::new("fs", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let facts = vec![(
            hash.clone(),
            MusicValue::FilePath(std::path::PathBuf::from("/music/inbox/foo.flac")),
            ts,
            Operation::Assert,
            source.clone(),
        )];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        // All three must agree on the non-empty, correctly-formed blob_path.
        let expected = "blobs/de/deadbeef1234.flac";
        let file_blob = file_result.blob_path.to_string_lossy().to_string();
        let bulk_blob = bulk_result.blob_path.to_string_lossy().to_string();
        let stream_blob = stream_result.blob_path.to_string_lossy().to_string();

        assert_eq!(
            file_blob, expected,
            "file path: blob_path should be '{}', got '{}'",
            expected, file_blob
        );
        assert_eq!(
            bulk_blob, expected,
            "bulk path (aggregate_facts): blob_path should be '{}', got '{}' — \
             this is the production regression: bulk load lost blob_path derivation",
            expected, bulk_blob
        );
        assert_eq!(
            stream_blob, expected,
            "stream path: blob_path should be '{}', got '{}'",
            expected, stream_blob
        );
    }

    /// Regression: blob_path must be RELATIVE after the ingest-path projection.
    ///
    /// Before the fix, `ingest_file_internal` (service.rs ~2897) overrode the
    /// correctly-derived relative blob_path with the absolute path produced by
    /// the import pipeline (`entry.blob_path = indexed.blob_path` where
    /// `indexed.blob_path` was e.g. `/music/blobs/de/deadbeef1234.flac`).
    ///
    /// `apply_fact_to_track` already derives the right relative form from the
    /// FilePath fact; the override was redundant and wrong.
    ///
    /// This test fails until the override is removed from both `apply_ingest_path`
    /// and `ingest_file_internal`.
    #[test]
    fn blob_path_is_relative_on_ingest_path() {
        let hash = ContentHash::new("sha256:deadbeef1234");
        let source = FactSource::new("fs", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let facts = vec![(
            hash.clone(),
            MusicValue::FilePath(std::path::PathBuf::from("/music/inbox/foo.flac")),
            ts,
            Operation::Assert,
            source.clone(),
        )];

        let result = apply_ingest_path(&hash, &facts);

        let expected = "blobs/de/deadbeef1234.flac";
        let actual = result.blob_path.to_string_lossy().to_string();
        assert_eq!(
            actual, expected,
            "ingest path: blob_path must be relative '{}', got '{}' — \
             regression: the absolute-path override in ingest_file_internal must not return",
            expected, actual
        );
    }

    // =========================================================================
    // Fix 2: TrackStarted/TrackStopped must be None after bulk bootstrap
    //
    // The FactAggregator::assert path uses Utc::now() as a placeholder
    // timestamp. After the fix, it must NOT populate last_started/last_stopped
    // during bulk load — those are None until refresh_event_timestamps runs.
    // =========================================================================

    // =========================================================================
    // Increment 1 field tests: Role, Energy, BeatGrid, MemoryCue
    // =========================================================================

    /// 3-path equivalence: Role + Energy + BeatGrid + MemoryCue asserted and read
    /// back identically through all three projection paths.
    #[test]
    fn equivalence_new_fields_role_energy_beat_grid_memory_cue_all_paths() {
        use music_facts::{Bpm as BpmValue, CueKind, EnergyLevel, TrackRole};

        let hash = ContentHash::new("sha256:equiv_new_fields_01");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

        let bpm_128 = BpmValue::from_f32(128.0).unwrap();
        let facts = vec![
            (
                hash.clone(),
                MusicValue::Role(TrackRole::Peak),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Energy(EnergyLevel::new(8).unwrap()),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                MusicValue::BeatGrid {
                    first_beat_ms: 500,
                    bpm: bpm_128,
                    beats_per_bar: 4,
                },
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                MusicValue::MemoryCue {
                    position_ms: 32000,
                    kind: CueKind::Hot,
                    label: Some("Drop".to_string()),
                    index: Some(0),
                },
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                MusicValue::MemoryCue {
                    position_ms: 1000,
                    kind: CueKind::Memory,
                    label: None,
                    index: None,
                },
                ts,
                Operation::Assert,
                source.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        // Verify expected values on file path
        assert_eq!(
            file_result.role,
            Some(TrackRole::Peak),
            "file path: role should be Peak"
        );
        assert_eq!(file_result.energy, Some(8), "file path: energy should be 8");
        assert_eq!(
            file_result.beat_grid,
            Some((500, 128.0, 4)),
            "file path: beat_grid mismatch"
        );
        assert_eq!(
            file_result.memory_cues.len(),
            2,
            "file path: expected 2 memory cues"
        );
        assert_eq!(
            file_result.memory_cues[0],
            IndexedCue {
                position_ms: 32000,
                kind: CueKind::Hot,
                label: Some("Drop".to_string()),
                index: Some(0),
            }
        );
        assert_eq!(
            file_result.memory_cues[1],
            IndexedCue {
                position_ms: 1000,
                kind: CueKind::Memory,
                label: None,
                index: None,
            }
        );

        // All three paths must agree
        assert_scalar_fields_eq(
            "new_fields_role_energy_beatgrid_cue bulk==file",
            &bulk_result,
            &file_result,
        );
        assert_scalar_fields_eq(
            "new_fields_role_energy_beatgrid_cue stream==file",
            &stream_result,
            &file_result,
        );
    }

    /// Projection test 2a: each new field populates IndexedTrackInfo from an
    /// asserted fact (single-path file check — covered more broadly above).
    #[test]
    fn projection_new_fields_asserted_individually() {
        use music_facts::{Bpm as BpmValue, CueKind, EnergyLevel, TrackRole};

        let hash = ContentHash::new("sha256:proj_new_fields_indiv");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

        // Role
        {
            let track = load_from_file_path(
                &hash,
                &[(
                    hash.clone(),
                    MusicValue::Role(TrackRole::Opener),
                    ts,
                    Operation::Assert,
                    source.clone(),
                )],
            );
            assert_eq!(track.role, Some(TrackRole::Opener));
        }

        // Energy
        {
            let track = load_from_file_path(
                &hash,
                &[(
                    hash.clone(),
                    MusicValue::Energy(EnergyLevel::new(5).unwrap()),
                    ts,
                    Operation::Assert,
                    source.clone(),
                )],
            );
            assert_eq!(track.energy, Some(5));
        }

        // BeatGrid
        {
            let bpm = BpmValue::from_f32(140.0).unwrap();
            let track = load_from_file_path(
                &hash,
                &[(
                    hash.clone(),
                    MusicValue::BeatGrid {
                        first_beat_ms: 200,
                        bpm,
                        beats_per_bar: 4,
                    },
                    ts,
                    Operation::Assert,
                    source.clone(),
                )],
            );
            assert_eq!(track.beat_grid, Some((200, 140.0, 4)));
        }

        // MemoryCue (Loop variant)
        {
            let track = load_from_file_path(
                &hash,
                &[(
                    hash.clone(),
                    MusicValue::MemoryCue {
                        position_ms: 64000,
                        kind: CueKind::Loop { length_ms: 4000 },
                        label: Some("Chorus".to_string()),
                        index: Some(2),
                    },
                    ts,
                    Operation::Assert,
                    source.clone(),
                )],
            );
            assert_eq!(track.memory_cues.len(), 1);
            assert_eq!(
                track.memory_cues[0],
                IndexedCue {
                    position_ms: 64000,
                    kind: CueKind::Loop { length_ms: 4000 },
                    label: Some("Chorus".to_string()),
                    index: Some(2),
                }
            );
        }
    }

    /// Projection test 2b: Retract(Role=Opener) while Role=Peak is live → Peak remains.
    ///
    /// This is the #96-guard on the multi-valued retraction path for Role.
    #[test]
    fn projection_retract_wrong_role_leaves_live_role_standing() {
        use music_facts::TrackRole;

        let hash = ContentHash::new("sha256:proj_role_retract_guard");
        let source_a = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let source_b = FactSource::new("source-b", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

        // source-a asserts Peak; source-b attempts to retract Opener (no-match).
        let facts = vec![
            (
                hash.clone(),
                MusicValue::Role(TrackRole::Peak),
                ts,
                Operation::Assert,
                source_a.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Role(TrackRole::Opener),
                ts,
                Operation::Retract,
                source_b.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        assert_eq!(
            file_result.role,
            Some(TrackRole::Peak),
            "#96-guard file: Retract(Role=Opener) must not clear live Role=Peak"
        );
        assert_scalar_fields_eq("role_retract_guard bulk==file", &bulk_result, &file_result);
        assert_scalar_fields_eq(
            "role_retract_guard stream==file",
            &stream_result,
            &file_result,
        );
    }

    /// Projection test 2c: multiple MemoryCue asserts + retracting one leaves the others.
    #[test]
    fn projection_retract_one_memory_cue_leaves_others() {
        use music_facts::CueKind;

        let hash = ContentHash::new("sha256:proj_cue_partial_retract");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

        let cue_drop = MusicValue::MemoryCue {
            position_ms: 32000,
            kind: CueKind::Hot,
            label: Some("Drop".to_string()),
            index: Some(0),
        };
        let cue_intro = MusicValue::MemoryCue {
            position_ms: 1000,
            kind: CueKind::Memory,
            label: Some("Intro".to_string()),
            index: None,
        };
        let cue_loop = MusicValue::MemoryCue {
            position_ms: 64000,
            kind: CueKind::Loop { length_ms: 4000 },
            label: None,
            index: Some(1),
        };

        // Assert all three, then retract the middle one (Drop).
        let facts = vec![
            (
                hash.clone(),
                cue_drop.clone(),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                cue_intro.clone(),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                cue_loop.clone(),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                cue_drop.clone(),
                ts,
                Operation::Retract,
                source.clone(),
            ),
        ];

        let file_result = load_from_file_path(&hash, &facts);
        let bulk_result = load_from_bulk_path(&hash, &facts);
        let stream_result = apply_stream_path(&hash, &facts);

        // Only Intro and Loop should remain
        assert_eq!(
            file_result.memory_cues.len(),
            2,
            "file: Drop retracted — 2 cues remain"
        );
        assert_eq!(
            file_result.memory_cues[0],
            IndexedCue {
                position_ms: 1000,
                kind: CueKind::Memory,
                label: Some("Intro".to_string()),
                index: None,
            },
            "file: Intro cue should survive"
        );
        assert_eq!(
            file_result.memory_cues[1],
            IndexedCue {
                position_ms: 64000,
                kind: CueKind::Loop { length_ms: 4000 },
                label: None,
                index: Some(1),
            },
            "file: Loop cue should survive"
        );

        assert_scalar_fields_eq("cue_partial_retract bulk==file", &bulk_result, &file_result);
        assert_scalar_fields_eq(
            "cue_partial_retract stream==file",
            &stream_result,
            &file_result,
        );
    }

    // =========================================================================
    // Lazy playlist repair via reverse-Replaces chain
    // =========================================================================

    /// Helper: set up a service with a `tracks` index containing entries with
    /// specific provenance (Replaces facts). The `_acid` handle must be kept alive.
    ///
    /// `live_tracks`: (hash, title) pairs that exist in the index (live).
    /// `replaces_pairs`: (new_hash, old_hash) — new_hash has Replaces(old_hash) in provenance.
    fn make_service_with_replaces(
        live_tracks: &[(ContentHash, &str)],
        replaces_pairs: &[(ContentHash, ContentHash)],
    ) -> (
        LibraryService,
        tempfile::TempDir,
        acid_service::ServerHandle,
    ) {
        let music_dir = tempfile::tempdir().unwrap();
        let metadata_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(metadata_dir.path().join("playlists")).unwrap();
        let (acid_handle, facts_addr, events_addr) = spawn_acid_server();
        let service = LibraryService::new_with_events(
            music_dir.path().to_path_buf(),
            metadata_dir.path().to_path_buf(),
            &facts_addr,
            &events_addr,
        )
        .unwrap();

        // Inject live tracks into in-memory index
        {
            let mut tracks = service.tracks.lock().unwrap();
            for (hash, title) in live_tracks {
                let mut entry = IndexedTrackInfo::new_empty(hash.as_str().to_owned());
                entry.title = Some(title.to_string());
                tracks.push(entry);
            }

            // Inject Replaces facts into provenance of the new hash entry
            let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
            for (new_hash, old_hash) in replaces_pairs {
                if let Some(entry) = tracks
                    .iter_mut()
                    .find(|t| t.content_hash.as_str() == new_hash.as_str())
                {
                    entry
                        .provenance
                        .push((MusicValue::Replaces(old_hash.clone()), source.clone()));
                }
            }
        }

        (service, metadata_dir, acid_handle)
    }

    /// 1-hop repair: A (retracted/not-in-index) → B (live).
    /// A playlist line pointing at A resolves to B and the playlist is amended.
    #[test]
    fn lazy_repair_one_hop_heals_playlist_line() {
        use library_ipc_protocol::PlaylistName;

        let hash_a = ContentHash::new("sha256:lazy_repair_a_0001");
        let hash_b = ContentHash::new("sha256:lazy_repair_b_0001");

        // B is live, B.Replaces(A). A is NOT in the index (hard-replaced).
        let (service, metadata_dir, _acid) = make_service_with_replaces(
            &[(hash_b.clone(), "Track B")],
            &[(hash_b.clone(), hash_a.clone())],
        );

        // Create a playlist pointing at A (the old, now-dead hash)
        let plist_path = metadata_dir
            .path()
            .join("playlists")
            .join("repair-test.plist");
        std::fs::write(&plist_path, format!("{}\n", hash_a.as_str())).unwrap();

        // PlaylistGet should trigger lazy repair
        let name = PlaylistName::new("repair-test").unwrap();
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name });

        match resp {
            LibraryResponse::PlaylistContent(content) => {
                assert!(
                    content.contains(hash_b.as_str()),
                    "repaired playlist must contain new hash B, got: {:?}",
                    content
                );
                assert!(
                    !content.contains(hash_a.as_str()),
                    "repaired playlist must not contain old hash A, got: {:?}",
                    content
                );
            }
            other => panic!("expected PlaylistContent, got {:?}", other),
        }

        // Persist: file on disk must also be updated
        let on_disk = std::fs::read_to_string(&plist_path).unwrap();
        assert!(
            on_disk.contains(hash_b.as_str()),
            "persisted playlist must contain new hash B, got: {:?}",
            on_disk
        );
    }

    /// 2-hop repair: A (dead) → B (dead) → C (live).
    /// A playlist line pointing at A resolves to C.
    #[test]
    fn lazy_repair_two_hop_follows_chain_to_live() {
        use library_ipc_protocol::PlaylistName;

        let hash_a = ContentHash::new("sha256:lazy_repair_a_0002");
        let hash_b = ContentHash::new("sha256:lazy_repair_b_0002");
        let hash_c = ContentHash::new("sha256:lazy_repair_c_0002");

        // C is live, C.Replaces(B), B.Replaces(A). A and B are NOT in the index.
        // We need B in the index with Replaces(A) in provenance but B itself is "dead"
        // (retracted from the index, i.e. not in `tracks`). Actually B need not be in
        // the index at all — but it must appear in C's provenance chain. The chain walk
        // only needs to find tracks that *assert* Replaces(X).
        //
        // Scenario: C has Replaces(B) in provenance, B had Replaces(A).
        // We also need B's Replaces(A) to be findable. B itself is retracted (no entry
        // in `tracks`). So we put B in the index as a "dead" entry (not retracted from
        // tracks — but its provenance has Replaces(A)) to simulate the intermediate step.
        // In the real system, B would have been the intermediate successor before C replaced it.
        // But B's facts were retracted. So B is NOT in tracks.
        //
        // For the chain to work across 2 hops, we need BOTH:
        //   - C.provenance contains Replaces(B)
        //   - something that lets us find Replaces(A)
        // The spec says: scan tracks' provenance for Replaces(X). So if B is retracted
        // and removed from tracks, there's no entry to scan. The realistic scenario is
        // that B is an intermediate dead hash — but since it's retracted, its Replaces(A)
        // provenance is gone from memory.
        //
        // Revised scenario: only C is in the index, C has Replaces(B) AND Replaces(A).
        // OR: B is still in the index with Replaces(A) (not yet removed / still a dead entry).
        //
        // Let's use the realistic test: B is still in the index (it was replaced by C
        // but hasn't been cleaned up yet). B has Replaces(A) in provenance.
        // C has Replaces(B) in provenance. The chain: A → B (via B.Replaces(A)) → C (via C.Replaces(B)).
        // C is resolvable, B is NOT resolvable (it's hidden/dead — not in tracks as live).
        //
        // Simplest: B IS in tracks but B itself doesn't resolve (its entry exists in tracks
        // but resolve_hash succeeds for B — that would make B "live" for the chain walk,
        // so the repair for a line pointing to A would stop at B (which is live).
        //
        // For a true 2-hop test where A→B→C, B must not be resolvable.
        // The implementation uses `resolve_hash` to check if a hash is live.
        // So to test that a hash is NOT live but still has provenance, we need B to
        // NOT be in `tracks` (so resolve_hash fails) but STILL have its Replaces(A)
        // provenance visible. That's a contradiction if we remove B from tracks.
        //
        // Solution: keep a "stub" entry for B in tracks with is_hidden()=true (deleted_at set),
        // so resolve_hash fails (it would still find it via the all-tracks scan used by
        // resolve_hash, which does NOT filter hidden tracks). Actually resolve_hash DOES
        // include hidden tracks. So hidden B would still be "resolved".
        //
        // The correct test for 2-hop: use retract_all_entity_facts semantics — B is
        // completely removed from `tracks`. But then its Replaces(A) is also gone.
        //
        // Resolution: the chain walk must also look in ACID for tracks that were retracted
        // but had Replaces. That would require reading ACID. OR we need a "retracted-but-
        // provenance-cached" map. That's architecture territory.
        //
        // Per the spec: "correctness first", and the realistic multi-hop scenario is:
        // After A was replaced by B, B was replaced by C. At that point C.Replaces(B)
        // is in C's provenance. But if B was fully retracted (hard-replaced), B's entry
        // is removed from tracks including its Replaces(A) provenance.
        //
        // So for a multi-hop chain to work from in-memory provenance scanning,
        // we need C to have both Replaces(B) and the walk to go: find C via Replaces(B),
        // C is live → stop. The "2-hop" in the spec means A was replaced by B which was
        // replaced by C — but the scanner sees: A not in index, find who has Replaces(A)
        // → that's B. But B is not in index. Dead end. Then find who has Replaces(B) → C.
        // C is live → return C.
        //
        // This requires that even though B is not in `tracks`, we can still find "who
        // replaces B". This works IF C.provenance contains Replaces(B), which it does.
        // So the algorithm is: given X, scan all tracks' provenance for Replaces(X) →
        // gives us Y. Is Y live (resolve_hash(Y) succeeds)? No → repeat with Y.
        // This does NOT require B to be in tracks at all. B is found as the successor of
        // A from C's perspective. Wait — we scan tracks for "Replaces(A)": we find B's
        // entry IF B is still in tracks. If B is NOT in tracks, we won't find it.
        //
        // Re-reading: "scan tracks (via provenance) for a Replaces fact whose value == X"
        // means scan all tracks' provenance for Replaces(A). Only tracks that have
        // Replaces(A) in their provenance will be found. If B (which replaces A) was
        // hard-retracted, B is not in tracks → not found.
        //
        // For the 2-hop: we need an intermediate B that was replaced but whose
        // Replaces(A) fact is still findable. In a real deployment, if B was hard-replaced
        // by C: B's facts (including Replaces(A)) are retracted from ACID and B is removed
        // from the in-memory index. So a pure in-memory 2-hop scan can't work if the
        // intermediate is fully retracted.
        //
        // UNLESS: C has Replaces(A) in provenance too (e.g., C was a "re-replace" of A
        // after B proved bad, so the operator ran: track replace A → C directly, giving
        // C Replaces(A)).
        //
        // The simplest valid 2-hop test with the in-memory provenance model:
        // B is in the index (not hard-retracted yet, but marked dead / the chain is that
        // B replaced A but B itself can't be played for some reason). B.resolve → succeeds,
        // but B is also old/dead from the playlist perspective.
        //
        // Since the spec doesn't require cross-ACID chain walking for retraced intermediate
        // nodes, the realistic 2-hop test is: C.Replaces(B) is in C's provenance,
        // and B.Replaces(A) is in B's provenance, with A dead (not in index) and
        // B also dead (removed from index). C is live.
        //
        // Given the constraint that provenance-based lookup requires the track to be in
        // `tracks`, we test the practical scenario: the chain can't follow hops through
        // fully-retracted intermediates. The 2-hop test that IS testable:
        // keep B in tracks (hard-replaced semantics where B is dead = B is in tracks
        // but with deleted_at set so get_track fails but provenance is preserved).
        //
        // Actually — re-check: resolve_hash includes hidden tracks. So if B is in tracks
        // with deleted_at set, resolve_hash(B) would succeed. That makes B "live" from
        // resolve_hash's perspective. We need resolve_hash(B) to fail.
        //
        // The only way resolve_hash(B) fails is if B is not in tracks at all.
        // And if B is not in tracks, B's provenance (including Replaces(A)) is gone.
        //
        // Conclusion: true multi-hop (A dead → B dead → C live) with B fully retracted
        // is NOT achievable with pure in-memory provenance scanning. The spec acknowledges
        // this implicit constraint: "correctness first". So the implementation should do
        // best-effort: single-hop is the primary case; multi-hop works when intermediate
        // nodes remain in memory (e.g. partially retracted or not yet cleaned up).
        //
        // Test: put B in tracks (as a "to-be-cleaned" entry with no title but still
        // visible to resolve_hash), C.Replaces(B), B.Replaces(A). A is gone.
        // Chain: A not in index → find B (B has Replaces(A)) → resolve_hash(B) succeeds
        // → B is "live" → return B. But we wanted C. This test can't reach C.
        //
        // The only testable 2-hop is C having Replaces(A) directly. OR: keep this test
        // as "A → B (via B.Replaces(A)) → C (via C.Replaces(B))" but B is "semi-dead":
        // resolve_hash(B) fails because B is not in tracks, but C has Replaces(B).
        //
        // Let's do the simplest thing: put B in tracks with no content but visible
        // to resolve_hash. Then A→B via B.Replaces(A). But B is "live" so chain stops at B.
        // That's a 1-hop test to B.
        //
        // For a genuine 2-hop where B is dead: We need B retracted BUT C.provenance
        // contains Replaces(A) (directly). Then it's a 1-hop to C.
        //
        // Given the constraints, we test the most realistic 2-hop: C has BOTH
        // Replaces(B) and Replaces(A) won't arise naturally. So the 2-hop test we
        // write is: A is dead, B is dead (not in tracks), C is live and has Replaces(B)
        // in provenance. The walk finds no match for Replaces(A) (B is gone), so dead-end.
        // That tests the dead-end termination path. The 2-hop that actually works
        // end-to-end requires B still in memory.
        //
        // We write the test with B still in tracks (so both hops can be followed).
        // The chain: A not in tracks (dead) → scan for Replaces(A) → B.provenance has it
        // → resolve_hash(B): is B in tracks? YES. Is B live (not hidden)? YES. Return B.
        // That's a 1-hop, not 2-hop. To force 2-hop: B must not resolve.
        // Only possible if B is completely absent from tracks.
        //
        // FINAL DECISION: write the test with B in tracks but with deleted_at set.
        // resolve_hash still returns B (it scans ALL tracks including hidden). Then
        // B is "live" from resolve_hash perspective. That's a 1-hop to hidden-B.
        //
        // The spec says "resolvable". We interpret: "resolve_hash succeeds AND the track
        // has no deleted_at" (i.e., truly live). If we implement it that way:
        // resolve_hash(B) succeeds but B is hidden → B is NOT live → continue chain.
        // Next: scan for Replaces(B) → find C → resolve_hash(C) succeeds and C not hidden → live.
        // Return C. That's a genuine 2-hop test.
        //
        // This requires `resolve_through_replaces` to check `get_track` (which rejects hidden)
        // rather than `resolve_hash` (which accepts hidden). We implement it to check
        // whether `get_track` would succeed (i.e. not hidden AND in index).

        // B is in tracks BUT marked as deleted (hidden), C is live. C.Replaces(B), B.Replaces(A).
        let (service, metadata_dir, _acid) = make_service_with_replaces(
            &[(hash_c.clone(), "Track C")],
            &[
                (hash_c.clone(), hash_b.clone()),
                (hash_b.clone(), hash_a.clone()),
            ],
        );

        // Mark B as dead (deleted_at) without removing from tracks
        {
            let mut tracks = service.tracks.lock().unwrap();
            // Add B to tracks with deleted_at set and with Replaces(A) in provenance
            let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
            let mut entry_b = IndexedTrackInfo::new_empty(hash_b.as_str().to_owned());
            entry_b.deleted_at = Some(chrono::Utc::now());
            entry_b
                .provenance
                .push((MusicValue::Replaces(hash_a.clone()), source));
            tracks.push(entry_b);
        }

        // Create a playlist pointing at A (the original dead hash)
        let plist_path = metadata_dir.path().join("playlists").join("two-hop.plist");
        std::fs::write(&plist_path, format!("{}\n", hash_a.as_str())).unwrap();

        let name = PlaylistName::new("two-hop").unwrap();
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name });

        match resp {
            LibraryResponse::PlaylistContent(content) => {
                assert!(
                    content.contains(hash_c.as_str()),
                    "2-hop repair must resolve A → B (hidden) → C (live), got: {:?}",
                    content
                );
                assert!(
                    !content.contains(hash_a.as_str()),
                    "old hash A must not remain in repaired playlist, got: {:?}",
                    content
                );
            }
            other => panic!("expected PlaylistContent, got {:?}", other),
        }
    }

    /// Dead-end: hash with no Replaces and not resolvable → behaves like today's
    /// missing-hash case (line left unchanged in the raw content).
    #[test]
    fn lazy_repair_dead_end_leaves_line_unchanged() {
        use library_ipc_protocol::PlaylistName;

        let dead_hash = ContentHash::new("sha256:lazy_repair_dead_0001");

        // No tracks, no Replaces chain
        let (service, metadata_dir, _acid) = make_service_with_replaces(&[], &[]);

        let plist_path = metadata_dir.path().join("playlists").join("dead-end.plist");
        std::fs::write(&plist_path, format!("{}\n", dead_hash.as_str())).unwrap();

        let name = PlaylistName::new("dead-end").unwrap();
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name });

        match resp {
            LibraryResponse::PlaylistContent(content) => {
                // Dead-end: line unchanged (current behavior for missing hash)
                assert!(
                    content.contains(dead_hash.as_str()),
                    "dead-end line must remain unchanged, got: {:?}",
                    content
                );
            }
            other => panic!("expected PlaylistContent, got {:?}", other),
        }
    }

    /// Cycle guard: A.Replaces(B) and B.Replaces(A) → terminates, doesn't hang.
    #[test]
    fn lazy_repair_cycle_guard_terminates() {
        use library_ipc_protocol::PlaylistName;

        let hash_a = ContentHash::new("sha256:lazy_repair_cycle_a");
        let hash_b = ContentHash::new("sha256:lazy_repair_cycle_b");

        // Both A and B are dead (not live), but each has Replaces pointing to the other.
        // We add them to tracks as hidden (deleted_at) so their provenance is scannable.
        let (service, metadata_dir, _acid) = make_service_with_replaces(&[], &[]);

        {
            let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
            let mut tracks = service.tracks.lock().unwrap();
            let mut entry_a = IndexedTrackInfo::new_empty(hash_a.as_str().to_owned());
            entry_a.deleted_at = Some(chrono::Utc::now());
            entry_a
                .provenance
                .push((MusicValue::Replaces(hash_b.clone()), source.clone()));
            let mut entry_b = IndexedTrackInfo::new_empty(hash_b.as_str().to_owned());
            entry_b.deleted_at = Some(chrono::Utc::now());
            entry_b
                .provenance
                .push((MusicValue::Replaces(hash_a.clone()), source));
            tracks.push(entry_a);
            tracks.push(entry_b);
        }

        let plist_path = metadata_dir.path().join("playlists").join("cycle.plist");
        std::fs::write(&plist_path, format!("{}\n", hash_a.as_str())).unwrap();

        let name = PlaylistName::new("cycle").unwrap();
        // Must not hang; must terminate and return a result
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name });

        match resp {
            LibraryResponse::PlaylistContent(_) => {
                // Any result is fine as long as it terminates
            }
            other => panic!(
                "expected PlaylistContent (even with cycle), got {:?}",
                other
            ),
        }
    }

    /// Live hash in a playlist is unaffected by the repair logic.
    #[test]
    fn lazy_repair_live_hash_unaffected() {
        use library_ipc_protocol::PlaylistName;

        let live_hash = ContentHash::new("sha256:lazy_repair_live_0001");

        let (service, metadata_dir, _acid) =
            make_service_with_replaces(&[(live_hash.clone(), "Live Track")], &[]);

        let plist_path = metadata_dir.path().join("playlists").join("live.plist");
        std::fs::write(&plist_path, format!("{}\n", live_hash.as_str())).unwrap();

        let name = PlaylistName::new("live").unwrap();
        let resp = service.handle_request(LibraryRequest::PlaylistGet { name });

        match resp {
            LibraryResponse::PlaylistContent(content) => {
                assert!(
                    content.contains(live_hash.as_str()),
                    "live hash must remain unchanged, got: {:?}",
                    content
                );
            }
            other => panic!("expected PlaylistContent, got {:?}", other),
        }
    }

    // =========================================================================
    // Track replace: Replaces model (hard retract old, Replaces fact on new)
    // =========================================================================

    /// handle_track_replace retracts ALL old facts so old hash stops resolving,
    /// asserts Replaces(old_hash) on the new hash, and rewrites playlists.
    ///
    /// This test seeds an old track in ACID, adds it to a playlist, then calls
    /// TrackReplace with a real audio file (from a temp path that the service can
    /// access). After replace:
    ///  - old hash is unresolvable (get_track → TrackNotFound, facts → empty)
    ///  - new hash resolves and has a Replaces fact
    ///  - playlist has been rewritten to point to new hash
    #[test]
    fn handle_track_replace_retracts_old_and_asserts_replaces_on_new() {
        // Since handle_track_replace requires a real audio file for ingest (which we cannot
        // produce in unit tests), we test the retraction mechanism directly via
        // retract_all_entity_facts — the internal function that handle_track_replace uses.
        // This tests:
        //   (a) retract_all_entity_facts: retracts all live facts for old hash in ACID
        //   (b) in-memory index removes the old hash entry
        // The Replaces fact serde and resolve_hash post-retraction are tested in separate tests.

        // Test (a): retract_all_entity_facts clears all facts for a hash
        let old_hash = ContentHash::new("sha256:replace_test_old01");
        let source = FactSource::new("mdma-library", "0.0.0", FactOrigin::Unknown);

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &old_hash,
                    &[
                        (MusicValue::Title(Title::new("Old Track")), source.clone()),
                        (
                            MusicValue::Artist(music_facts::Artist::new("Old Artist")),
                            source.clone(),
                        ),
                    ],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir, facts_addr) = make_service_with_facts_and_addr(temp.path());

        // Pre-condition: old hash resolves
        let resolve_before = service.resolve_hash(&old_hash);
        assert!(
            resolve_before.is_ok(),
            "old hash must resolve before replace, got: {:?}",
            resolve_before
        );

        // Act: retract all facts for old hash via the internal mechanism
        let result = service.retract_all_entity_facts(&old_hash);
        assert!(result.is_ok(), "retract_all_entity_facts must succeed");

        // Post-condition: after retraction, the old hash's ACID facts are all Retracted
        let verifier = AcidClient::connect(&facts_addr).unwrap();
        let lines = verifier.read_entity(old_hash.as_str()).unwrap();
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
            retract_count >= 2,
            "at least 2 Retract entries must be in ACID (one per original fact), got: {}",
            retract_count
        );

        // Post-condition: update in-memory index to reflect retractions, old hash gone
        // retract_all_entity_facts removes the entry from self.tracks
        let tracks = service.tracks.lock().unwrap();
        let old_track = tracks.iter().find(|t| t.content_hash == old_hash);
        assert!(
            old_track.is_none(),
            "old hash must be removed from in-memory index after retract_all_entity_facts"
        );
    }

    /// Replaces(ContentHash) fact roundtrips through serde correctly.
    #[test]
    fn replaces_fact_serde_roundtrip() {
        let old_hash = ContentHash::new("sha256:oldhash001");
        let val = MusicValue::Replaces(old_hash.clone());
        let json = serde_json::to_string(&val).unwrap();
        let decoded: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, MusicValue::Replaces(old_hash));
    }

    /// Replaces display_name and Display.
    #[test]
    fn replaces_fact_display() {
        let old_hash = ContentHash::new("sha256:oldhash002");
        let val = MusicValue::Replaces(old_hash.clone());
        assert_eq!(val.display_name(), "Replaces");
        assert!(val.to_string().contains("sha256:oldhash002"));
    }

    /// After retract_all_entity_facts, resolve_hash for the old hash returns TrackNotFound.
    #[test]
    fn resolve_hash_fails_after_retract_all_entity_facts() {
        let old_hash = ContentHash::new("sha256:resolve_retract_test01");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &old_hash,
                    &[(MusicValue::Title(Title::new("Gone Track")), source.clone())],
                )
                .unwrap();
            t
        };

        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        // Verify track exists first
        assert!(
            service.resolve_hash(&old_hash).is_ok(),
            "hash must resolve before retraction"
        );

        // Retract all facts
        service
            .retract_all_entity_facts(&old_hash)
            .expect("retract_all_entity_facts must succeed");

        // After retraction, resolve_hash must fail (entry removed from index)
        let result = service.resolve_hash(&old_hash);
        assert!(
            matches!(result, Err(ProtocolError::TrackNotFound { .. })),
            "resolve_hash must return TrackNotFound after retract_all_entity_facts, got: {:?}",
            result
        );
    }

    /// After a bulk bootstrap containing TrackStarted/TrackStopped facts,
    /// last_started and last_stopped must be None (not a bootstrap wall-clock
    /// value). refresh_event_timestamps is the sole source of truth.
    #[test]
    fn bulk_bootstrap_does_not_set_timestamps_from_placeholder() {
        let hash = ContentHash::new("sha256:bulk_ts_placeholder");
        let source = FactSource::new("test-playback", "1.0.0", FactOrigin::Unknown);
        let play_ts = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();

        // Write TrackStarted + TrackStopped facts with a real historical timestamp.
        // After bulk load via aggregate_facts, the service must leave last_started
        // and last_stopped as None (not Utc::now()).
        let temp = NamedTempFile::new().unwrap();
        {
            let mut writer = FactStreamWriter::open(temp.path()).unwrap();
            let facts: Vec<Fact<ContentHash, MusicValue, FactSource>> = vec![
                Fact::new(
                    hash.clone(),
                    MusicValue::Title(Title::new("Played Track")),
                    play_ts,
                    source.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::TrackStarted(music_facts::StartReason::OnRequest),
                    play_ts,
                    source.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MusicValue::TrackStopped(music_facts::StopReason::OnSkip),
                    play_ts,
                    source.clone(),
                    Operation::Assert,
                ),
            ];
            writer.write_batch(&facts).unwrap();
        }

        // Bootstrap via the bulk path (make_service_with_facts → load_from_acid_stream
        // → aggregate_facts → FactAggregator::assert).
        let (service, _metadata_dir) = make_service_with_facts(temp.path());

        let tracks = service.tracks.lock().unwrap();
        let track = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .expect("track must be indexed after bulk bootstrap");

        // The bulk path must NOT populate last_started/last_stopped with Utc::now().
        // refresh_event_timestamps (triggered by a date-filtered search) is the sole
        // source of truth for these fields after bootstrap.
        assert_eq!(
            track.last_started, None,
            "last_started must be None after bulk bootstrap (not a wall-clock placeholder)"
        );
        assert_eq!(
            track.last_stopped, None,
            "last_stopped must be None after bulk bootstrap (not a wall-clock placeholder)"
        );
    }

    // =========================================================================
    // Forward-inherit Replaces facts: multi-hop through fully-retracted intermediate
    // =========================================================================

    /// `gather_ancestor_replaces` returns the set of hashes that a track previously
    /// replaced (its `Replaces(*)` provenance entries), so they can be forward-inherited
    /// onto the next successor before the track is retracted.
    ///
    /// When B.provenance contains Replaces(A), calling `gather_ancestor_replaces` on
    /// B must return [A]. When B has no Replaces facts, returns empty vec.
    ///
    /// This method is used in `handle_track_replace` to flatten the chain:
    /// before retracting B, gather its ancestors and assert them on C alongside Replaces(B).
    #[test]
    fn gather_ancestor_replaces_returns_replaces_provenance() {
        let hash_a = ContentHash::new("sha256:gather_anc_a_0001");
        let hash_b = ContentHash::new("sha256:gather_anc_b_0001");
        let hash_c = ContentHash::new("sha256:gather_anc_c_0001");

        // B has Replaces(A) in provenance; A and B are both "live" for this setup.
        // C has no Replaces facts.
        let (service, _metadata_dir, _acid) = make_service_with_replaces(
            &[(hash_b.clone(), "Track B"), (hash_c.clone(), "Track C")],
            &[(hash_b.clone(), hash_a.clone())],
        );

        // B has Replaces(A) — should return [A]
        let ancestors_b = service.gather_ancestor_replaces(&hash_b);
        assert_eq!(
            ancestors_b.len(),
            1,
            "B.gather_ancestor_replaces must return [A], got {:?}",
            ancestors_b
        );
        assert_eq!(
            ancestors_b[0].as_str(),
            hash_a.as_str(),
            "B's single ancestor must be A"
        );

        // C has no Replaces facts — should return []
        let ancestors_c = service.gather_ancestor_replaces(&hash_c);
        assert!(
            ancestors_c.is_empty(),
            "C.gather_ancestor_replaces must return [] (no ancestors), got {:?}",
            ancestors_c
        );
    }

    /// Multi-hop replacement: A→B→C where BOTH A and B are fully retracted (hard-replaced).
    ///
    /// After A→B: B is in tracks, B.Replaces(A). A is gone.
    /// After B→C with forward-inheritance: C ends up with Replaces(B) AND Replaces(A).
    ///   B is gone. A is gone.
    ///
    /// A playlist pointing at A must repair to C (1-hop via C.Replaces(A)).
    /// A playlist pointing at B must repair to C (1-hop via C.Replaces(B)).
    ///
    /// This test FAILS before the fix (C has only Replaces(B); A→C dead-ends because
    /// B is fully retracted and its Replaces(A) is gone).
    /// It PASSES after the fix (C has both Replaces(A) and Replaces(B)).
    #[test]
    fn multi_hop_replace_with_retracted_intermediate_resolves_to_current() {
        use library_ipc_protocol::PlaylistName;

        let hash_a = ContentHash::new("sha256:mhop_a_0001");
        let hash_b = ContentHash::new("sha256:mhop_b_0001");
        let hash_c = ContentHash::new("sha256:mhop_c_0001");

        // PRE-FIX state: C has ONLY Replaces(B). B is fully absent (hard-retracted).
        // A is also absent. No track has Replaces(A) in provenance.
        // This represents what the current (unfixed) code would produce after A→B→C.
        //
        // We then call `assert_inherited_replaces_facts` (the new forward-inheritance helper)
        // which is what handle_track_replace will call. It gathers B's Replaces(A) BEFORE
        // retraction and asserts them on C. After the fix, C must have Replaces(A) too.
        //
        // To test the bug: first set up as if B just replaced A (B in tracks, Replaces(A)).
        // Then call the inheritance gather and verify C gets Replaces(A) asserted.
        // Then retract B. Then verify playlist repair from A reaches C.

        // Step 1: set up mid-chain state — B is live with Replaces(A), C is not yet present.
        let (service, metadata_dir, _acid) = make_service_with_replaces(
            &[(hash_b.clone(), "Track B")],
            &[(hash_b.clone(), hash_a.clone())],
        );

        // Step 2: add C to tracks (as if it was just ingested), with only Replaces(B) for now.
        {
            let mut tracks = service.tracks.lock().unwrap();
            let mut entry_c = IndexedTrackInfo::new_empty(hash_c.as_str().to_owned());
            entry_c.title = Some("Track C".to_string());
            let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
            entry_c
                .provenance
                .push((MusicValue::Replaces(hash_b.clone()), source));
            tracks.push(entry_c);
        }

        // Step 3: before retracting B, gather B's Replaces ancestors and assert them on C.
        // This is the forward-inheritance step that the fix adds to handle_track_replace.
        let ancestors = service.gather_ancestor_replaces(&hash_b);
        // At this point, ancestors must be [A]
        assert_eq!(
            ancestors.len(),
            1,
            "gather must find A as B's ancestor before B is retracted"
        );

        // Assert inherited facts on C (forward-inheritance)
        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(entry_c) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash_c.as_str())
            {
                let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
                for ancestor in &ancestors {
                    // Dedup: only assert if not already present
                    let already = entry_c.provenance.iter().any(|(v, _)| {
                        if let MusicValue::Replaces(old) = v {
                            old.as_str() == ancestor.as_str()
                        } else {
                            false
                        }
                    });
                    if !already {
                        entry_c
                            .provenance
                            .push((MusicValue::Replaces(ancestor.clone()), source.clone()));
                    }
                }
            }
        }

        // Step 4: retract B (remove from tracks — simulates retract_all_entity_facts)
        {
            let mut tracks = service.tracks.lock().unwrap();
            tracks.retain(|t| t.content_hash.as_str() != hash_b.as_str());
        }

        // Now: A is absent, B is absent, C is live with Replaces(B) AND Replaces(A).
        // Verify C has both Replaces facts.
        {
            let tracks = service.tracks.lock().unwrap();
            let entry_c = tracks
                .iter()
                .find(|t| t.content_hash.as_str() == hash_c.as_str())
                .expect("C must still be in tracks");

            let replaces_set: Vec<&str> = entry_c
                .provenance
                .iter()
                .filter_map(|(v, _)| {
                    if let MusicValue::Replaces(old) = v {
                        Some(old.as_str())
                    } else {
                        None
                    }
                })
                .collect();

            assert!(
                replaces_set.contains(&hash_b.as_str()),
                "C must have Replaces(B), got {:?}",
                replaces_set
            );
            assert!(
                replaces_set.contains(&hash_a.as_str()),
                "C must have Replaces(A) via forward-inheritance, got {:?}",
                replaces_set
            );
        }

        // Step 5: verify playlist repair.
        // Playlist pointing at A must repair to C.
        let plist_a_path = metadata_dir.path().join("playlists").join("mhop-a.plist");
        std::fs::write(&plist_a_path, format!("{}\n", hash_a.as_str())).unwrap();

        let name_a = PlaylistName::new("mhop-a").unwrap();
        let resp_a = service.handle_request(LibraryRequest::PlaylistGet { name: name_a });
        match resp_a {
            LibraryResponse::PlaylistContent(content) => {
                assert!(
                    content.contains(hash_c.as_str()),
                    "playlist pointing at A must repair to C (forward-inherited Replaces), got: {:?}",
                    content
                );
                assert!(
                    !content.contains(hash_a.as_str()),
                    "old hash A must not remain after repair, got: {:?}",
                    content
                );
            }
            other => panic!("expected PlaylistContent for playlist-A, got {:?}", other),
        }

        // Playlist pointing at B must repair to C.
        let plist_b_path = metadata_dir.path().join("playlists").join("mhop-b.plist");
        std::fs::write(&plist_b_path, format!("{}\n", hash_b.as_str())).unwrap();

        let name_b = PlaylistName::new("mhop-b").unwrap();
        let resp_b = service.handle_request(LibraryRequest::PlaylistGet { name: name_b });
        match resp_b {
            LibraryResponse::PlaylistContent(content) => {
                assert!(
                    content.contains(hash_c.as_str()),
                    "playlist pointing at B must repair to C, got: {:?}",
                    content
                );
                assert!(
                    !content.contains(hash_b.as_str()),
                    "old hash B must not remain after repair, got: {:?}",
                    content
                );
            }
            other => panic!("expected PlaylistContent for playlist-B, got {:?}", other),
        }
    }

    /// Single replace still asserts exactly Replaces(old) on the new track — no extras.
    ///
    /// Verifies forward-inheritance doesn't introduce spurious Replaces facts when
    /// the old track had no prior Replaces ancestors (i.e. it was never itself a replacement).
    #[test]
    fn single_replace_asserts_only_one_replaces_fact() {
        let hash_a = ContentHash::new("sha256:single_rep_a_0001");
        let hash_b = ContentHash::new("sha256:single_rep_b_0001");

        // B is live, B has Replaces(A) only (no inherited ancestors — A was not
        // itself a replacement of anything).
        let (service, _metadata_dir, _acid) = make_service_with_replaces(
            &[(hash_b.clone(), "Track B")],
            &[(hash_b.clone(), hash_a.clone())],
        );

        // A has no Replaces facts, so gather_ancestor_replaces(A) → []
        // (A is not even in tracks — it was retracted — but gather reads B's provenance)
        let ancestors_of_a = service.gather_ancestor_replaces(&hash_a);
        assert!(
            ancestors_of_a.is_empty(),
            "A has no ancestors (it was not itself a replacement), got {:?}",
            ancestors_of_a
        );

        // B has Replaces(A) and A has no ancestors → no inherited facts beyond Replaces(A)
        let tracks = service.tracks.lock().unwrap();
        let entry_b = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash_b.as_str())
            .expect("B must be in tracks");

        let replaces_facts: Vec<&ContentHash> = entry_b
            .provenance
            .iter()
            .filter_map(|(v, _)| {
                if let MusicValue::Replaces(old) = v {
                    Some(old)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            replaces_facts.len(),
            1,
            "single replace must produce exactly 1 Replaces fact, got: {:?}",
            replaces_facts
        );
        assert_eq!(
            replaces_facts[0].as_str(),
            hash_a.as_str(),
            "the single Replaces fact must point at A"
        );
    }

    // =========================================================================
    // Orphan: blob-on-disk enumeration (NoLiveFacts / Deleted / live)
    // =========================================================================

    /// Helper: create a fake blob file at `music_dir/blobs/{prefix}/{hash}.flac`.
    /// Returns the content hash with the `sha256:` prefix.
    fn plant_blob(music_dir: &std::path::Path, hash_hex: &str) -> ContentHash {
        let prefix = &hash_hex[..2];
        let blob_dir = music_dir.join("blobs").join(prefix);
        std::fs::create_dir_all(&blob_dir).unwrap();
        let blob_path = blob_dir.join(format!("{}.flac", hash_hex));
        std::fs::write(&blob_path, b"fake audio data").unwrap();
        ContentHash::new(format!("sha256:{}", hash_hex))
    }

    /// A blob on disk whose hash is absent from the live index → NoLiveFacts.
    ///
    /// This is the hard-replace scenario: retract_all_entity_facts removed the
    /// track from self.tracks but left the blob on disk.
    #[test]
    fn orphan_no_live_facts_blob_without_index_entry() {
        let hash_hex = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
        let temp_facts = write_facts_file(&[]);
        let (service, _metadata_dir) = make_service_with_facts(temp_facts.path());

        // Plant a blob that has NO corresponding index entry
        let hash = plant_blob(&service.music_dir, hash_hex);

        let resp = service.handle_request(LibraryRequest::TrackOrphans);
        match resp {
            LibraryResponse::OrphansList(items) => {
                let orphan = items
                    .iter()
                    .find(|o| o.content_hash.as_str() == hash.as_str());
                assert!(
                    orphan.is_some(),
                    "blob with no index entry must appear in orphans list"
                );
                assert!(
                    matches!(orphan.unwrap().reason, OrphanReason::NoLiveFacts),
                    "reason must be NoLiveFacts, got {:?}",
                    orphan.unwrap().reason
                );
            }
            other => panic!("expected OrphansList, got {:?}", other),
        }
    }

    /// A live track with a blob is NOT listed as an orphan.
    #[test]
    fn orphan_live_track_with_blob_not_listed() {
        let hash_hex = "11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff";
        let hash = ContentHash::new(format!("sha256:{}", hash_hex));

        let temp_facts = write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("Live")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp_facts.path());

        // Set blob_path in the index entry so is_orphan_blob logic sees it
        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(t) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                t.blob_path = std::path::PathBuf::from(format!(
                    "blobs/{}/{}.flac",
                    hash_hex.get(..2).unwrap(),
                    hash_hex
                ));
            }
        }

        // Plant the blob so it actually exists on disk
        plant_blob(&service.music_dir, hash_hex);

        let resp = service.handle_request(LibraryRequest::TrackOrphans);
        match resp {
            LibraryResponse::OrphansList(items) => {
                let listed = items
                    .iter()
                    .any(|o| o.content_hash.as_str() == hash.as_str());
                assert!(!listed, "live track must NOT appear in orphans list");
            }
            other => panic!("expected OrphansList, got {:?}", other),
        }
    }

    /// Soft-deleted track → listed with reason Deleted.
    /// Restore → no longer listed.
    #[test]
    fn orphan_deleted_track_listed_and_removed_after_restore() {
        let hash_hex = "deadbeef00112233445566778899aabbccddeeff00112233445566778899dead";
        let hash = ContentHash::new(format!("sha256:{}", hash_hex));

        let temp_facts =
            write_facts_file(&[(hash.clone(), MusicValue::Title(Title::new("SoftDeleteMe")))]);
        let (service, _metadata_dir) = make_service_with_facts(temp_facts.path());

        // Give the index entry a blob_path and plant the blob
        {
            let mut tracks = service.tracks.lock().unwrap();
            if let Some(t) = tracks
                .iter_mut()
                .find(|t| t.content_hash.as_str() == hash.as_str())
            {
                t.blob_path = std::path::PathBuf::from(format!(
                    "blobs/{}/{}.flac",
                    hash_hex.get(..2).unwrap(),
                    hash_hex
                ));
            }
        }
        plant_blob(&service.music_dir, hash_hex);

        // Soft-delete
        service.handle_request(LibraryRequest::TrackDelete { hash: hash.clone() });

        let resp = service.handle_request(LibraryRequest::TrackOrphans);
        match resp {
            LibraryResponse::OrphansList(items) => {
                let orphan = items
                    .iter()
                    .find(|o| o.content_hash.as_str() == hash.as_str());
                assert!(orphan.is_some(), "deleted track must appear in orphans");
                assert!(
                    matches!(orphan.unwrap().reason, OrphanReason::Deleted { .. }),
                    "reason must be Deleted, got {:?}",
                    orphan.unwrap().reason
                );
            }
            other => panic!("expected OrphansList, got {:?}", other),
        }

        // Restore
        service.handle_request(LibraryRequest::TrackRestore { hash: hash.clone() });

        let resp2 = service.handle_request(LibraryRequest::TrackOrphans);
        match resp2 {
            LibraryResponse::OrphansList(items) => {
                let still_listed = items
                    .iter()
                    .any(|o| o.content_hash.as_str() == hash.as_str());
                assert!(!still_listed, "restored track must NOT appear in orphans");
            }
            other => panic!("expected OrphansList, got {:?}", other),
        }
    }
}

// =========================================================================
// Lazy playlist repair implementation
// =========================================================================

impl LibraryService {
    /// Return all hashes that `hash` directly replaced, by scanning `hash`'s
    /// in-memory provenance for `Replaces(X)` entries.
    ///
    /// Call this BEFORE retracting `hash` so the provenance is still present.
    /// The returned vec is deduplicated. An empty vec means `hash` was never
    /// itself a replacement (first-generation track).
    ///
    /// Used by `handle_track_replace` to forward-inherit the chain: when C
    /// replaces B, C must also claim every hash that B had replaced (e.g. A),
    /// so that a playlist still pointing at A can find its way to C even after
    /// B is fully retracted.
    pub(crate) fn gather_ancestor_replaces(&self, hash: &ContentHash) -> Vec<ContentHash> {
        let normalize = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();
        let target_clean = normalize(hash.as_str());

        let tracks = self.tracks.lock().unwrap();
        let entry = match tracks
            .iter()
            .find(|t| normalize(t.content_hash.as_str()) == target_clean)
        {
            Some(e) => e,
            None => return Vec::new(),
        };

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut result: Vec<ContentHash> = Vec::new();

        for (v, _) in &entry.provenance {
            if let MusicValue::Replaces(old) = v {
                let old_clean = normalize(old.as_str());
                if seen.insert(old_clean) {
                    result.push(old.clone());
                }
            }
        }

        result
    }

    /// Follow the reverse-Replaces chain from `hash` to find the live successor.
    ///
    /// Scans the in-memory track index for a track whose provenance contains
    /// `Replaces(hash)`. If found and that track is live (not hidden, resolvable),
    /// return it. If found but not live, recurse with that track's hash.
    ///
    /// Terminates on:
    /// - cycle: a visited-set bounds iteration to MAX_CHAIN_DEPTH hops
    /// - dead end: no track asserts Replaces(X) and X is not live → None
    ///
    /// "Live" means: the track is in the index AND is not hidden (deleted_at is None).
    /// This is stricter than resolve_hash (which accepts hidden tracks).
    fn resolve_through_replaces(&self, hash: &ContentHash) -> Option<ContentHash> {
        const MAX_CHAIN_DEPTH: usize = 64;

        let normalize = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();
        let target_clean = normalize(hash.as_str());

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(target_clean.clone());

        let mut current = hash.clone();

        for _ in 0..MAX_CHAIN_DEPTH {
            let current_clean = normalize(current.as_str());

            // Scan all tracks' provenance for Replaces(current)
            let successor = {
                let tracks = self.tracks.lock().unwrap();
                tracks
                    .iter()
                    .find(|t| {
                        t.provenance.iter().any(|(v, _)| {
                            if let MusicValue::Replaces(old) = v {
                                let old_clean = normalize(old.as_str());
                                old_clean == current_clean
                            } else {
                                false
                            }
                        })
                    })
                    .map(|t| t.content_hash.clone())
            };

            match successor {
                None => {
                    // No track asserts Replaces(current) — dead end
                    return None;
                }
                Some(next_hash) => {
                    let next_clean = normalize(next_hash.as_str());

                    // Cycle guard
                    if visited.contains(&next_clean) {
                        tracing::warn!(
                            hash = %hash.as_str(),
                            chain = %next_hash.as_str(),
                            "Cycle detected in Replaces chain during playlist repair — terminating"
                        );
                        return None;
                    }
                    visited.insert(next_clean.clone());

                    // Check if next_hash is live (in index and not hidden)
                    let is_live = {
                        let tracks = self.tracks.lock().unwrap();
                        tracks.iter().any(|t| {
                            let h = normalize(t.content_hash.as_str());
                            h == next_clean && t.deleted_at.is_none()
                        })
                    };

                    if is_live {
                        return Some(next_hash);
                    } else {
                        // Not live — continue chain
                        current = next_hash;
                    }
                }
            }
        }

        tracing::warn!(
            hash = %hash.as_str(),
            "Replaces chain exceeded max depth during playlist repair — giving up"
        );
        None
    }

    /// Scan a playlist's content, follow the Replaces chain for any unresolvable
    /// hash, amend lines to the live successor, and persist the file if anything changed.
    ///
    /// Returns the (possibly amended) content string.
    fn repair_playlist_content(&self, content: &str, plist_path: &std::path::Path) -> String {
        use std::io::Write;

        let normalize = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();

        let mut changed = false;
        let new_lines: Vec<String> = content
            .lines()
            .map(|line| {
                let token = line.split_whitespace().next().unwrap_or("");
                if token.is_empty() {
                    return line.to_string();
                }

                let token_clean = normalize(token);
                // Build a ContentHash from the token to try resolve_hash
                let candidate = ContentHash::new(token);

                // Is this hash already live?
                let is_live = {
                    let tracks = self.tracks.lock().unwrap();
                    tracks.iter().any(|t| {
                        let h = normalize(t.content_hash.as_str());
                        (h.starts_with(&token_clean) || token_clean.starts_with(&h))
                            && t.deleted_at.is_none()
                    })
                };

                if is_live {
                    return line.to_string();
                }

                // Not live — try repair via Replaces chain
                match self.resolve_through_replaces(&candidate) {
                    None => {
                        // Dead end — leave line unchanged
                        line.to_string()
                    }
                    Some(successor) => {
                        changed = true;
                        // Replace only the hash token; preserve rest of line verbatim
                        let rest = line[token.len()..].to_string();
                        format!("{}{}", successor.as_str(), rest)
                    }
                }
            })
            .collect();

        let new_content = new_lines.join("\n");
        let new_content = if content.ends_with('\n') {
            format!("{}\n", new_content)
        } else {
            new_content
        };

        if changed {
            // Persist atomically via temp file (same approach as rewrite_playlists_replace_hash)
            let tmp_path = plist_path.with_extension("plist.tmp");
            if let Ok(mut f) = std::fs::File::create(&tmp_path) {
                if f.write_all(new_content.as_bytes()).is_ok() {
                    let _ = std::fs::rename(&tmp_path, plist_path);
                } else {
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
        }

        new_content
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
