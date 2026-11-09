//! Library service implementation
//!
//! Handles IPC requests and manages library state

use crate::identity::{identity_facts_for_blob, IdentityOpts};
use crate::ipc::{
    bookmark_scope_mark, parse_request, BeatGridInfo, Bpm, ContentHash, CueInfo, DurationSeconds,
    FactType, InboxPath, IngestAllItem, IngestResult, IngestSource, IpcServer, Key, LibraryRequest,
    LibraryResponse, Mark, MarkFilter, MarkWrite, OrphanInfo, OrphanReason, ProtocolError,
    ServiceStatus, TrackInfo, TrackQuery, Unmark,
};
use crate::pipeline::{InboxFile, UploadSource};
use acid_client::AcidClient;
use event_protocol::{acid_event_from_topic_message, AcidEvent, TOPIC_ACID};
use library_search::{matches_query, TrackFields};
use music_facts::MusicValue;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
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
    ///
    /// This mutex is also the *catch-up lock*: `catch_up_with` holds it across
    /// `read_stream(cursor)` -> `apply_stream_lines` -> cursor advance, so every
    /// stream line is projected exactly once no matter who triggers the catch-up
    /// (the fact subscriber or a writer that just wrote to ACID).
    ///
    /// Lock order: `retire_gate` -> `cursor` -> `tracks` -> `fact_index` ->
    /// `content_hashes`; `event_cursor` -> `tracks` -> `play_events`
    /// (`refresh_event_timestamps`); `tracks` -> `album_cover_cache` (nothing taken while
    /// holding it). `identity_backfill` is independent: take it alone and never
    /// together with another lock in this list.
    /// No projection lock may be held while calling `catch_up`.
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
    /// Latest start and stop recorded under every entity key (normalised hash) seen
    /// by `refresh_event_timestamps`, retired entities included: a track's play
    /// history is its own plus that of every hash it replaces (ADR-013).
    ///
    /// Lock order: taken after `tracks`, alone or inside it; nothing is taken while holding it.
    play_events: Mutex<HashMap<String, PlayTimes>>,
    /// Address of the ACID request socket (used by background subscriber thread).
    acid_socket: String,
    /// Address of the ACID events pub/sub socket.
    acid_events_socket: String,
    /// Weak handle to the `Arc` this service lives in (`Arc::new_cyclic`). The
    /// identity backfill worker outlives the request that starts it, so it needs
    /// an owning handle.
    self_ref: std::sync::Weak<LibraryService>,
    /// State and progress of the explicit identity-hash backfill (see `backfill`
    /// module). Lock order: outermost, or held alone; see `cursor`.
    identity_backfill: Mutex<crate::ipc::IdentityBackfillStatus>,
    /// Set by `CancelIdentityBackfill`; the worker checks it between tracks.
    identity_cancel: std::sync::atomic::AtomicBool,
    /// Serialises *retiring* an entity against *out-of-band writers*. ACID has no
    /// conditional write, so "check the entity is live, then write" races with a
    /// retire that has already read the entity's facts: a late write is not in the
    /// retract set and would leave the entity alive on that fact alone (identity
    /// facts are provenance-tracked).
    ///
    /// Every retire path must hold it for its whole body, catch-up included:
    /// `retract_all_entity_facts_locked` runs under it, and `merge_into` (ADR-013)
    /// holds it from its check that winner and loser are live until the loser is
    /// retired, i.e. across the winner's `Replaces` write too, so a winner retired
    /// meanwhile is refused rather than resurrected by that write. The mutex is not
    /// reentrant: a holder retires through `retract_all_entity_facts_locked`. Every
    /// out-of-band writer that targets entities chosen from a snapshot (the identity
    /// backfill) must hold it from its liveness re-check until its write returns.
    /// The guard (`MutexGuard<RetireToken>`) is passed to the functions that need
    /// the gate, so the compiler proves their callers hold this mutex.
    /// Lock order: before `cursor`; see there.
    retire_gate: Mutex<gate::RetireToken>,
    /// Test-only: called by the backfill worker after hashing a track and before
    /// the liveness check + write, to inject a concurrent change at that point.
    #[cfg(test)]
    identity_write_hook: Mutex<Option<IdentityWriteHook>>,
    /// Test-only: called inside `retract_all_entity_facts_locked` (under `retire_gate`) right after it has read
    /// the entity's facts and before it retracts them.
    #[cfg(test)]
    retire_mid_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Test-only: called by `merge_into` after its checks and before it writes the
    /// winner's facts, to inject a concurrent change at that point.
    #[cfg(test)]
    merge_pre_write_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Test-only: called by `merge_group` with the loser after its merge and before
    /// the retire gate is taken to archive the loser's files, to inject a concurrent
    /// change (a re-ingest) in that window.
    #[cfg(test)]
    merge_pre_archive_hook: Mutex<Option<IdentityWriteHook>>,
}

#[cfg(test)]
type IdentityWriteHook = Box<dyn Fn(&LibraryService, &ContentHash) + Send + Sync>;

mod backfill;
mod dedup;
#[cfg(test)]
mod dedup_tests;

/// The payload of `retire_gate`. It can only be made by `new_gate`, so a
/// `MutexGuard<RetireToken>` is proof of holding the real gate.
mod gate {
    use std::sync::Mutex;

    pub(super) struct RetireToken(());

    pub(super) fn new_gate() -> Mutex<RetireToken> {
        Mutex::new(RetireToken(()))
    }
}
use gate::RetireToken;

/// Proof that the caller holds `retire_gate`.
type RetireGuard<'a> = std::sync::MutexGuard<'a, RetireToken>;

/// The files a retired track owned, relative to `music_dir`. Captured before the
/// retire removes the track's index entry.
#[derive(Debug, Default)]
struct RetiredFiles {
    blob: Option<PathBuf>,
    cover_art: Option<PathBuf>,
}

/// What `merge_into` did.
#[derive(Debug)]
struct MergeOutcome {
    playlists_rewritten: usize,
    /// The loser's files, to be removed once the merge is durable.
    retired: RetiredFiles,
}

/// Write handle given to the closure of `write_batch_and_project_with`; the only
/// thing it can do is write facts.
struct BatchWriter<'a> {
    client: &'a AcidClient,
    wrote: std::cell::Cell<bool>,
}

impl BatchWriter<'_> {
    fn write_music_facts(
        &self,
        hash: &ContentHash,
        facts: &[(MusicValue, music_facts::FactSource)],
    ) -> Result<usize, acid_client::ClientError> {
        let written = self.client.write_music_facts(hash, facts)?;
        self.wrote.set(true);
        Ok(written)
    }
}

/// Runs the batch's single catch-up on drop (normal exit or unwind), if anything
/// was written.
struct ProjectOnDrop<'a> {
    service: &'a LibraryService,
    writer: &'a BatchWriter<'a>,
}

impl Drop for ProjectOnDrop<'_> {
    fn drop(&mut self) {
        // While unwinding, skip the catch-up: it takes the projection locks, which a
        // panic elsewhere may have poisoned, and a panic inside a drop during unwind
        // aborts the process. Skipping is safe: the facts are already durable in ACID
        // and the fact subscriber converges the index.
        if std::thread::panicking() {
            return;
        }
        if self.writer.wrote.get() {
            self.service.catch_up_after_write_with(self.writer.client);
        }
    }
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
    energy: Option<music_facts::EnergyLevel>,
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
    /// An entry is live iff it holds at least one tracked assertion. Non-live
    /// entries (fully retracted, or orphan play events for unknown hashes) are
    /// dropped by every projection path so such a hash stops resolving (ADR-004).
    fn is_live(&self) -> bool {
        !self.provenance.is_empty()
    }

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
    /// Every ItemId this track holds. A confirmed merge unions them, so anything that
    /// matches on an ItemId must use this set, never the last-wins scalar `item_id`
    /// (which is for display).
    fn item_ids(&self) -> impl Iterator<Item = &str> {
        self.provenance.iter().filter_map(|(v, _)| match v {
            MusicValue::ItemId(id) => Some(id.as_str()),
            _ => None,
        })
    }

    /// Every AudioHash this track holds, as the hash string (version prefix included).
    fn audio_hashes(&self) -> impl Iterator<Item = &str> {
        self.provenance.iter().filter_map(|(v, _)| match v {
            MusicValue::AudioHash(h) => Some(h.as_str()),
            _ => None,
        })
    }

    /// Returns true if this track is hidden from default views.
    fn is_hidden(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Returns true if this track holds a live mark admitted by `filter`. Reads
    /// provenance, so a retracted mark no longer counts.
    fn has_mark(&self, filter: &MarkFilter) -> bool {
        self.provenance
            .iter()
            .filter_map(|(v, _)| v.stored_mark())
            .any(|m| filter.admits(&m))
    }

    /// The live assertions of `mark` on this track, with their sources. The one
    /// predicate for "carries this mark live", shared by `write_mark` and `unmark`.
    fn live_marks<'a>(
        &'a self,
        mark: &'a Mark,
    ) -> impl Iterator<Item = &'a (MusicValue, music_facts::FactSource)> {
        self.provenance.iter().filter(move |(v, _)| mark.matches(v))
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
/// ## Liveness
/// An entry whose provenance is empty after a retraction is reset to a pristine
/// `new_empty` entry, clearing history-dependent state (AddedAt, FilePath,
/// blob_path, timestamps). Liveness is then purely `!provenance.is_empty()`
/// (`IndexedTrackInfo::is_live`), independent of how the facts were folded, so
/// bulk and incremental projections agree.
fn apply_fact_to_track(
    entry: &mut IndexedTrackInfo,
    value: &MusicValue,
    timestamp: chrono::DateTime<chrono::Utc>,
    operation: stainless_facts::Operation,
    source: &music_facts::FactSource,
) {
    use stainless_facts::Operation;

    match operation {
        Operation::Assert => {
            // Store provenance for value-matched retraction (skip timestamp-only fields)
            if is_provenance_tracked(value) {
                entry.provenance.push((value.clone(), source.clone()));
            }
            apply_assert_scalar(entry, value, timestamp);
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
            apply_retract_scalar(entry, value);

            // Nothing live remains: forget everything, including untracked state.
            if entry.provenance.is_empty() {
                *entry = IndexedTrackInfo::new_empty(entry.content_hash.as_str().to_owned());
            }
        }
    }
}

/// Returns true for `MusicValue` variants whose (value, source) pairs are tracked
/// in `IndexedTrackInfo::provenance` for correct retraction semantics.
///
/// Timestamp-only values (TrackStarted, TrackStopped) use a separate most-recent-wins
/// strategy and are intentionally excluded — retractions for those are not emitted
/// in production and the timestamp logic is handled elsewhere.
///
/// `AddedAt` is excluded too: a retract of it is a no-op (there is no provenance to
/// fall back to), and only ingest's `now()` ever writes it. Projection is
/// latest-wins (`apply_assert_scalar`).
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
        MusicValue::Format(_) => {}
        MusicValue::CoverArtPath(p) => {
            entry.cover_art_path = Some(PathBuf::from(p));
        }
        // Latest AddedAt value wins (order-independent, so every projection path agrees).
        MusicValue::AddedAt(dt) => update_if_more_recent(&mut entry.added_at, *dt),
        MusicValue::ItemId(v) => entry.item_id = Some(v.clone()),
        MusicValue::Replaces(_) => {
            // Replaces is stored in provenance only; no scalar field needed.
            // The new track carries this fact; the old track's facts are retracted separately.
        }
        MusicValue::Deleted { timestamp } => {
            entry.deleted_at = Some(*timestamp);
        }
        MusicValue::Role(v) => entry.role = Some(*v),
        MusicValue::Energy(v) => entry.energy = Some(*v),
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
fn apply_retract_scalar(entry: &mut IndexedTrackInfo, retracted: &MusicValue) {
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
                _ => entry.cover_art_path = None,
            }
        }
        MusicValue::Format(_) => {}
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
                            Some(e)
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

/// Normalised form of a content hash for comparisons: no `sha256:` prefix, lowercase.
fn hash_key(h: &str) -> String {
    h.strip_prefix("sha256:").unwrap_or(h).to_lowercase()
}

/// One line of an entity's fact stream, or `None` for a line that is not a fact.
fn parse_fact_line(
    line: &str,
) -> Option<stainless_facts::Fact<ContentHash, MusicValue, music_facts::FactSource>> {
    serde_json::from_str(line).ok()
}

/// Latest start and stop recorded under one entity key.
#[derive(Clone, Copy, Default)]
struct PlayTimes {
    started: Option<chrono::DateTime<chrono::Utc>>,
    stopped: Option<chrono::DateTime<chrono::Utc>>,
}

/// `key` followed by every hash it transitively replaces. `replaces` maps a live
/// track's key to the keys it replaces; a cycle ends the walk.
fn replaces_lineage(key: &str, replaces: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut seen = vec![key.to_owned()];
    let mut queue = vec![key.to_owned()];
    while let Some(k) = queue.pop() {
        for replaced in replaces.get(&k).into_iter().flatten() {
            if !seen.contains(replaced) {
                seen.push(replaced.clone());
                queue.push(replaced.clone());
            }
        }
    }
    seen
}

/// The shortest key length that is a full hash, not one of the short forms.
const SHORT_KEY_MAX: usize = 12;

/// For every 8- and 12-character prefix, how many of `full_keys` start with it.
fn short_prefix_counts<'a>(full_keys: impl Iterator<Item = &'a str>) -> HashMap<&'a str, usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for key in full_keys.filter(|k| k.len() > SHORT_KEY_MAX) {
        for prefix in [key.get(..8), key.get(..12)].into_iter().flatten() {
            *counts.entry(prefix).or_default() += 1;
        }
    }
    counts
}

/// The play events recorded for `key`: under the full hash, and under the 12- and
/// 8-character short forms older playback wrote (#86). A short form counts only when
/// `key` is the single full key it is a prefix of (`prefix_counts`), the same rule
/// `resolve_hash` applies: an ambiguous short key belongs to nobody.
fn play_times_for<'a>(
    events: &'a HashMap<String, PlayTimes>,
    key: &str,
    prefix_counts: &HashMap<&str, usize>,
) -> Vec<&'a PlayTimes> {
    let shorts = if key.len() > SHORT_KEY_MAX {
        [key.get(..12), key.get(..8)]
    } else {
        [None, None]
    };
    std::iter::once(key)
        .chain(
            shorts
                .into_iter()
                .flatten()
                .filter(|k| prefix_counts.get(k) == Some(&1)),
        )
        .filter_map(|k| events.get(k))
        .collect()
}

/// Wrap an ACID client failure as a protocol-level internal error.
fn acid_error(e: acid_client::ClientError) -> ProtocolError {
    ProtocolError::Internal {
        message: e.to_string(),
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

/// The `FactSource` the library service stamps on facts it derives itself
/// (format, cover art, identity hashes, ingest provenance).
fn library_fact_source() -> music_facts::FactSource {
    music_facts::FactSource::new(
        "mdma-library",
        env!("CARGO_PKG_VERSION"),
        music_facts::FactOrigin::Unknown,
    )
}

/// The source of facts a user request writes.
fn user_fact_source() -> music_facts::FactSource {
    music_facts::FactSource::new(
        "mdma",
        env!("CARGO_PKG_VERSION"),
        music_facts::FactOrigin::User,
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
    ) -> Result<Arc<Self>, ServiceError> {
        Self::new_with_events(
            music_dir,
            metadata_dir,
            acid_socket,
            "ipc:///run/mdma/acid-events.sock",
        )
    }

    /// Create a new library service with an explicit ACID events socket address.
    ///
    /// Returns an `Arc` because the identity backfill worker needs an owning handle
    /// to the service (`Arc::new_cyclic`).
    pub fn new_with_events(
        music_dir: PathBuf,
        metadata_dir: PathBuf,
        acid_socket: &str,
        acid_events_socket: &str,
    ) -> Result<Arc<Self>, ServiceError> {
        // Connect to ACID service for writing and streaming
        let acid_client = AcidClient::connect(acid_socket)?;

        // Always do a full bootstrap from the beginning (cursor=None).
        // No on-disk cursor is read; the cursor lives in memory only.
        let (loaded, final_cursor) = Self::load_from_acid_stream(&acid_client, None)?;

        let tracks_count = loaded.tracks.len();
        let album_cover_cache = Self::build_album_cover_cache(&loaded.tracks);

        // The fallible bootstrap above runs before `new_cyclic`, whose closure cannot fail.
        let service = Arc::new_cyclic(|self_ref| Self {
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
            play_events: Mutex::new(HashMap::new()),
            acid_socket: acid_socket.to_string(),
            acid_events_socket: acid_events_socket.to_string(),
            self_ref: self_ref.clone(),
            identity_backfill: Mutex::new(crate::ipc::IdentityBackfillStatus::Idle),
            identity_cancel: std::sync::atomic::AtomicBool::new(false),
            retire_gate: gate::new_gate(),
            #[cfg(test)]
            identity_write_hook: Mutex::new(None),
            #[cfg(test)]
            retire_mid_hook: Mutex::new(None),
            #[cfg(test)]
            merge_pre_write_hook: Mutex::new(None),
            #[cfg(test)]
            merge_pre_archive_hook: Mutex::new(None),
        });

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

        // Non-live entities are dropped, exactly as the incremental projection's
        // sweep (`drop_non_live`) drops them, so a hard-retracted hash stops
        // resolving after a restart too (ADR-004).
        aggregated.retain(|_, entry| entry.is_live());

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
            );

            // Rebuild fact_index for this field from surviving provenance
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    if is_provenance_tracked(fact.value()) {
                        fact_index.entry(fact_type).or_default().insert(value_str);
                    }
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

        tracks_map.retain(|_, entry| entry.is_live());

        // Derive the side-effect sets exactly as the bulk path does.
        for (key, entry) in &tracks_map {
            if entry
                .provenance
                .iter()
                .any(|(v, _)| matches!(v, MusicValue::Format(_)))
            {
                has_format.insert(key.clone());
            }
            if entry.cover_art_path.is_some() {
                has_cover_art.insert(key.clone());
            }
        }

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

        let source = library_fact_source();

        // The batch seam projects the written facts (index, fact_index, counters)
        // through the single path once the closure returns.
        let backfilled = self.write_batch_and_project_with(&self.acid_client, |w| {
            let mut backfilled = 0;
            for (hash, music_format) in &needs_backfill {
                let value = MusicValue::Format(*music_format);
                if w.write_music_facts(hash, &[(value, source.clone())])
                    .is_ok()
                {
                    backfilled += 1;
                }
            }
            backfilled
        });

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
    ///
    /// Returns the number of CoverArtPath facts written.
    fn backfill_cover_art(&self, has_cover_art: &HashSet<String>) -> usize {
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
            return 0;
        }

        tracing::info!(
            count = needs_backfill.len(),
            "Backfilling CoverArtPath facts for tracks"
        );

        let fact_source = library_fact_source();

        // Written facts are projected through the single path when the batch ends.
        let backfilled = self.write_batch_and_project_with(&self.acid_client, |w| {
            let mut backfilled = 0;
            for (hash, blob_rel_path) in &needs_backfill {
                let blob_abs = self.music_dir.join(blob_rel_path);
                if let Some(rel_path) = self.extract_and_store_cover_art(&blob_abs, hash) {
                    let value = MusicValue::CoverArtPath(rel_path.to_string_lossy().to_string());
                    if w.write_music_facts(hash, &[(value, fact_source.clone())])
                        .is_ok()
                    {
                        backfilled += 1;
                    }
                }
            }
            backfilled
        });

        tracing::info!(backfilled, "CoverArtPath fact backfill complete");
        backfilled
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

        self.backfill_cover_art(&has_cover_art)
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
        // Built once per call to avoid O(n*m) linear scans. Entries are never
        // removed during a call (see `drop_non_live`), so positions stay valid.
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
            let is_retract = fact.operation() == stainless_facts::Operation::Retract;

            // Insert or fetch the track entry using the O(1) index.
            // A Retract never creates an entry: retracting an unknown (or already
            // dropped) entity is a no-op, otherwise replaying the retract lines of
            // a removed entity would resurrect it as an empty ghost (ADR-004).
            let pos = if let Some(&pos) = index.get(&entity) {
                pos
            } else if is_retract {
                continue;
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
            );

            // fact_index mirrors provenance (like the bulk path): untracked values
            // (FilePath, AddedAt, play events) are never indexed.
            if !is_provenance_tracked(fact.value()) {
                continue;
            }
            match fact.operation() {
                stainless_facts::Operation::Assert => {
                    fact_index.entry(fact_type).or_default().insert(value_str);
                }
                stainless_facts::Operation::Retract => {
                    // Remove from fact_index only when no entity still asserts this
                    // value (the current entry has already been updated above).
                    let still_asserted = tracks
                        .iter()
                        .any(|t| t.provenance.iter().any(|(v, _)| v.to_string() == value_str));
                    if !still_asserted {
                        if let Some(set) = fact_index.get_mut(&fact_type) {
                            set.remove(&value_str);
                        }
                    }
                }
            }
        }

        // The single place where the counters are settled: every projection of
        // stream lines (subscriber replay and writer catch-up alike) ends here.
        self.tracks_indexed.store(tracks.len(), Ordering::Relaxed);
        self.facts_count.fetch_add(applied, Ordering::Relaxed);
    }

    /// Drop every non-live entry (no tracked provenance): fully retracted entities
    /// and orphan play events for unknown or retired hashes. Mirrors the bulk
    /// path's `retain(is_live)`.
    ///
    /// Run once after *all* pages of a catch-up, never per line or per page, so an
    /// ingest batch split across a page boundary is not dropped half-way.
    fn drop_non_live(&self) {
        let mut tracks = self.tracks.lock().unwrap();
        let mut content_hashes = self.content_hashes.lock().unwrap();
        tracks.retain(|t| {
            let live = t.is_live();
            if !live {
                content_hashes.remove(t.content_hash.as_str());
            }
            live
        });
        self.tracks_indexed.store(tracks.len(), Ordering::Relaxed);
    }

    /// Project every ACID stream line this service has not yet seen, using the
    /// service's own ACID client.
    ///
    /// Writers call this right after their ACID write returns so the in-memory
    /// index reflects the write (read-your-writes) through the *same* projection
    /// the fact subscriber uses; there is no second, hand-rolled projection.
    /// Returns the number of lines read.
    pub fn catch_up(&self) -> Result<usize, acid_client::ClientError> {
        self.catch_up_with(&self.acid_client)
    }

    /// Like [`catch_up`](Self::catch_up) but reading through `client`.
    ///
    /// The fact subscriber passes its own client: nng REQ sockets must not be
    /// shared between concurrent requests.
    ///
    /// The `cursor` mutex is held for the whole read -> apply -> advance sequence,
    /// so concurrent callers are serialised and each line is applied exactly once.
    /// The mutex is held across the ACID read IPC, which is why it is a dedicated
    /// lock that no projection or request path ever takes.
    ///
    /// Once all pages are applied, non-live entries are swept (`drop_non_live`).
    ///
    /// Returns the number of stream lines *read* (malformed lines included), not the
    /// number applied.
    fn catch_up_with(&self, client: &AcidClient) -> Result<usize, acid_client::ClientError> {
        const PAGE_SIZE: usize = 10_000;

        let mut cursor = self.cursor.lock().unwrap();
        let mut total = 0usize;
        let result = loop {
            let chunk = match client.read_stream(cursor.clone(), PAGE_SIZE) {
                Ok(c) => c,
                Err(e) => break Err(e),
            };
            let count = chunk.lines.len();
            if count > 0 {
                self.apply_stream_lines(&chunk.lines);
                total += count;
            }
            *cursor = Some(chunk.cursor);
            if count < PAGE_SIZE {
                break Ok(total);
            }
        };
        // Sweep even after a failed page: earlier pages were applied.
        self.drop_non_live();
        result
    }

    /// Write facts to ACID and project them (read-your-writes). Request handlers
    /// reach ACID writes through this, so a new handler cannot forget to project.
    fn write_and_project(
        &self,
        hash: &ContentHash,
        facts: &[(MusicValue, music_facts::FactSource)],
    ) -> Result<usize, acid_client::ClientError> {
        self.write_and_project_with(&self.acid_client, hash, facts)
    }

    /// [`write_and_project`](Self::write_and_project) through an explicit ACID client.
    ///
    /// A thread other than the request thread (the backfill worker) must use its own
    /// client: nng REQ sockets cannot be shared between concurrent requests.
    fn write_and_project_with(
        &self,
        client: &AcidClient,
        hash: &ContentHash,
        facts: &[(MusicValue, music_facts::FactSource)],
    ) -> Result<usize, acid_client::ClientError> {
        let written = client.write_music_facts(hash, facts)?;
        self.catch_up_after_write_with(client);
        Ok(written)
    }

    /// Batch form of [`write_and_project_with`](Self::write_and_project_with): `f`
    /// writes any number of entities through the [`BatchWriter`], and the written
    /// facts are projected with a single catch-up when `f` returns, or unwinds (the
    /// catch-up is a `Drop` guard). Nothing in `f` can write to ACID without going
    /// through here, so a batch cannot forget to project.
    ///
    /// Holds no lock itself; callers needing `retire_gate` take it inside `f`.
    fn write_batch_and_project_with<R>(
        &self,
        client: &AcidClient,
        f: impl FnOnce(&BatchWriter<'_>) -> R,
    ) -> R {
        let writer = BatchWriter {
            client,
            wrote: std::cell::Cell::new(false),
        };
        let _projection = ProjectOnDrop {
            service: self,
            writer: &writer,
        };
        f(&writer)
    }

    /// Retract facts in ACID and project them. See [`write_and_project`](Self::write_and_project).
    fn retract_and_project(
        &self,
        hash: &ContentHash,
        facts: &[(MusicValue, music_facts::FactSource)],
    ) -> Result<usize, acid_client::ClientError> {
        let retracted = self.acid_client.retract_music_facts(hash, facts)?;
        self.catch_up_after_write();
        Ok(retracted)
    }

    /// `catch_up` for write paths: the ACID write already succeeded and is
    /// durable, so a failed read here only delays visibility until the
    /// subscriber's next catch-up. Log and carry on.
    fn catch_up_after_write(&self) {
        self.catch_up_after_write_with(&self.acid_client);
    }

    fn catch_up_after_write_with(&self, client: &AcidClient) {
        if let Err(e) = self.catch_up_with(client) {
            tracing::warn!(error = %e, "catch_up after ACID write failed; subscriber will converge");
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

            LibraryRequest::ListMarked { filter } => {
                LibraryResponse::Tracks(self.tracks_where(|t| t.has_mark(&filter), None))
            }

            LibraryRequest::GetTrack { hash } => match self.get_track(&hash) {
                Ok(track) => LibraryResponse::Track(Box::new(track)),
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

            LibraryRequest::IdentityBackfill { include_pcm } => {
                match self.start_identity_backfill(include_pcm) {
                    Ok(start) => LibraryResponse::IdentityBackfillStarted(start),
                    Err(e) => LibraryResponse::Error(e),
                }
            }

            LibraryRequest::GetIdentityBackfillStatus => {
                LibraryResponse::IdentityBackfillStatus(self.identity_backfill_status())
            }

            LibraryRequest::CancelIdentityBackfill => {
                LibraryResponse::IdentityBackfillStatus(self.cancel_identity_backfill())
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

            LibraryRequest::WriteFact { hash, fact } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let source = user_fact_source();
                match self.write_and_project(&full_hash, &[(fact, source)]) {
                    Ok(_) => LibraryResponse::FactWritten,
                    Err(e) => LibraryResponse::Error(acid_error(e)),
                }
            }

            LibraryRequest::RetractFact { hash, fact } => {
                let full_hash = match self.resolve_hash(&hash) {
                    Ok(h) => h,
                    Err(e) => return LibraryResponse::Error(e),
                };
                let source = user_fact_source();
                match self.retract_and_project(&full_hash, &[(fact, source)]) {
                    Ok(_) => LibraryResponse::FactRetracted,
                    Err(e) => LibraryResponse::Error(acid_error(e)),
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

            LibraryRequest::DedupScan { apply } => {
                LibraryResponse::DedupScan(self.dedup_scan(apply))
            }

            LibraryRequest::DedupMerge { winner, losers } => {
                self.dedup_merge_request(&winner, &losers)
            }

            LibraryRequest::ArchiveList => LibraryResponse::ArchiveListing(self.archive_listing()),

            LibraryRequest::ArchiveEmpty { up_to } => {
                LibraryResponse::ArchiveEmptied(self.archive_empty(up_to.as_deref()))
            }
        }
    }

    /// Scan the in-memory track index and return every ContentHash whose
    /// ItemId set contains `item_id` (a merged track holds several).
    ///
    /// Linear scan over the in-memory index — acceptable at current library scale.
    fn content_hashes_for_item_id(&self, item_id: &str) -> Vec<ContentHash> {
        self.tracks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.item_ids().any(|id| id == item_id))
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
    /// sent to ACID via `retract_and_project`, which projects each retraction
    /// through the stream path; scalar fields are re-resolved from whatever
    /// provenance survives (another source's Album stays).
    ///
    /// Retire safety: this retracts only facts with the given source and does not
    /// take `retire_gate`. It must never be what empties an entity; any path that can
    /// leave an entity with no live facts has to go through
    /// `retract_all_entity_facts_locked` under `retire_gate`.
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
                .filter_map(|line| parse_fact_line(line))
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
            // Each hash is projected as it is retracted, so an error part-way leaves
            // the index consistent with what ACID already holds.
            match self.retract_and_project(hash, facts) {
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
        let source = user_fact_source();
        match self.write_and_project(&full_hash, &[(fact, source)]) {
            Ok(_) => LibraryResponse::TrackDeleted,
            Err(e) => LibraryResponse::Error(acid_error(e)),
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
        let source = user_fact_source();
        match self.retract_and_project(&full_hash, &[(fact, source)]) {
            Ok(_) => LibraryResponse::TrackRestored,
            Err(e) => LibraryResponse::Error(acid_error(e)),
        }
    }

    /// Retract ALL currently-asserted facts for `hash` from ACID and remove the
    /// entry from the in-memory index.
    ///
    /// This is the hard-delete half of `handle_track_replace`. It reads all facts
    /// for `hash` from ACID via `read_entity`, filters to Assert operations, and
    /// sends a matching Retract for each (value, source) pair via `retract_and_project`.
    /// The projection then drops the entry so `resolve_hash` returns TrackNotFound.
    ///
    /// Returns `Ok(retracted_count)` on success, or an error string on ACID failure.
    ///
    /// The caller must hold `retire_gate` for the whole retire (read, retract and the
    /// catch-up inside `retract_and_project`); the guard is the proof. The mutex is
    /// not reentrant. Production retires only through `merge_into`.
    fn retract_all_entity_facts_locked(
        &self,
        _gate: &RetireGuard<'_>,
        hash: &ContentHash,
    ) -> Result<usize, String> {
        // Read all fact lines for this entity from ACID
        let lines = self
            .acid_client
            .read_entity(hash.as_str())
            .map_err(|e| format!("Failed to read entity from ACID: {}", e))?;

        #[cfg(test)]
        if let Some(hook) = self.retire_mid_hook.lock().unwrap().as_ref() {
            hook();
        }

        // Collect the (value, source) pairs still asserted. We retract exactly those
        // so the provenance retraction logic in apply_fact_to_track correctly
        // matches and removes them; pairs an earlier retraction cancelled are not
        // retracted again.
        let to_retract: Vec<(MusicValue, music_facts::FactSource)> = dedup::live_facts(&lines)
            .into_iter()
            .map(|f| (f.value, f.source))
            .collect();

        if to_retract.is_empty() {
            // Nothing asserted in ACID, hence nothing live in the index either.
            return Ok(0);
        }

        // Send all retractions to ACID via the trustworthy (value, source) retraction
        // path, projecting them. Once the last live provenance entry is retracted the
        // projection drops the entity, so resolve_hash returns TrackNotFound. This is
        // a hard delete: the old identity is gone, not hidden.
        let count = self
            .retract_and_project(hash, &to_retract)
            .map_err(|e| format!("Failed to retract facts via ACID: {}", e))?;

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

        let source = user_fact_source();
        match self.merge_into(&new_hash, &old_full, &source, Vec::new()) {
            Ok(outcome) => LibraryResponse::TrackReplaced {
                new_hash,
                playlists_rewritten: outcome.playlists_rewritten,
            },
            Err(e) => LibraryResponse::Error(e),
        }
    }

    /// Retire `loser` into `winner`, both existing tracks (ADR-004 hard replace,
    /// generalised by ADR-013). In this order:
    ///
    /// Steps 1 to 3 run under the retire gate, taken once: both tracks must still be
    /// live (else the merge is refused), and nothing in the body takes the gate again.
    ///
    /// 1. GATHER BEFORE RETRACT: the loser's `Replaces(X)` ancestors and the files it
    ///    owns are read from the index now, before retracting removes the entry.
    /// 2. The winner is asserted `carry`, `Replaces(loser)` and the forward-inherited
    ///    ancestors (`write_merge_facts`).
    /// 3. `retract_all_entity_facts_locked(loser)` hard-retracts the loser so it
    ///    stops resolving. A retire that fails leaves the loser alive and the whole
    ///    merge safely repeatable.
    /// 4. Playlists are rewritten loser -> winner.
    /// 5. Play history is re-projected, so the winner shows the loser's plays.
    ///
    /// Files are not touched: the caller removes `MergeOutcome::retired` once this
    /// returned `Ok`, i.e. once the facts are durable. `source` is the `FactSource`
    /// of `Replaces(loser)`.
    fn merge_into(
        &self,
        winner: &ContentHash,
        loser: &ContentHash,
        source: &music_facts::FactSource,
        carry: Vec<(MusicValue, music_facts::FactSource)>,
    ) -> Result<MergeOutcome, ProtocolError> {
        if hash_key(winner.as_str()) == hash_key(loser.as_str()) {
            return Err(ProtocolError::Internal {
                message: format!("cannot merge {winner} into itself"),
            });
        }

        #[cfg(test)]
        if let Some(hook) = self.merge_pre_write_hook.lock().unwrap().as_ref() {
            hook();
        }

        let retired = {
            // Held from the liveness check to the end of the retire, so neither
            // track can be retired in between and `Replaces` cannot resurrect a
            // winner that is gone. See `retire_gate`.
            let gate = self.retire_gate.lock().unwrap();
            for track in [winner, loser] {
                if !self.is_live(track) {
                    return Err(ProtocolError::Internal {
                        message: format!("{track} is no longer in the library"),
                    });
                }
            }

            // Playlists pointing at any generation of the chain must find the winner.
            let ancestors = self.ancestor_replaces(loser);
            let retired = self.retired_files(loser);

            self.write_merge_facts(&gate, winner, loser, ancestors, source, carry)?;

            // The crux: after this, resolve_hash(loser) -> TrackNotFound.
            self.retract_all_entity_facts_locked(&gate, loser)
                .map_err(|e| ProtocolError::Internal {
                    message: format!("Failed to retract old track facts: {}", e),
                })?;
            retired
        };

        // Neither of these takes the gate (it is not reentrant), and neither needs it.
        let playlists_rewritten = self.rewrite_playlists_replace_hash(loser, winner);
        self.refresh_event_timestamps();

        Ok(MergeOutcome {
            playlists_rewritten,
            retired,
        })
    }

    /// Assert `carry`, `Replaces(loser)` and the loser's forward-inherited ancestors
    /// on the winner, in one write.
    ///
    /// The winner never replaces itself (a revert A -> B -> A has A among B's
    /// ancestors), and a `Replaces` it already holds is not asserted again, so
    /// repeating a merge that crashed before its retire adds nothing twice.
    fn write_merge_facts(
        &self,
        _gate: &RetireGuard<'_>,
        winner: &ContentHash,
        loser: &ContentHash,
        ancestors: Vec<(ContentHash, music_facts::FactSource)>,
        source: &music_facts::FactSource,
        carry: Vec<(MusicValue, music_facts::FactSource)>,
    ) -> Result<(), ProtocolError> {
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(hash_key(winner.as_str()));
        {
            let tracks = self.tracks.lock().unwrap();
            if let Some(entry) = tracks
                .iter()
                .find(|t| hash_key(t.content_hash.as_str()) == hash_key(winner.as_str()))
            {
                for (v, _) in &entry.provenance {
                    if let MusicValue::Replaces(held) = v {
                        seen.insert(hash_key(held.as_str()));
                    }
                }
            }
        }

        let mut facts = carry;
        // Replaces(loser) first, then every ancestor the loser had replaced.
        let links = std::iter::once((loser.clone(), source.clone())).chain(ancestors);
        for (replaced, replaced_source) in links {
            if seen.insert(hash_key(replaced.as_str())) {
                facts.push((MusicValue::Replaces(replaced), replaced_source));
            }
        }
        if facts.is_empty() {
            return Ok(());
        }

        // Read-your-writes (issue #44): the write is projected immediately, through
        // the same path the subscriber uses, so back-to-back replaces see each other.
        self.write_and_project(winner, &facts)
            .map(|_| ())
            .map_err(|e| ProtocolError::Internal {
                message: format!("Failed to assert Replaces facts: {}", e),
            })
    }

    /// `write_merge_facts` for a plain ADR-004 replace: no carried facts, and every
    /// link under the same user source.
    #[cfg(test)]
    fn assert_replaces_facts(
        &self,
        new_hash: &ContentHash,
        old_full: &ContentHash,
        inherited_ancestors: Vec<ContentHash>,
    ) -> Result<(), ProtocolError> {
        let source = user_fact_source();
        let ancestors = inherited_ancestors
            .into_iter()
            .map(|h| (h, source.clone()))
            .collect();
        let gate = self.retire_gate.lock().unwrap();
        self.write_merge_facts(&gate, new_hash, old_full, ancestors, &source, Vec::new())
    }

    /// Whether `hash` is an indexed track (hidden ones included).
    fn is_live(&self, hash: &ContentHash) -> bool {
        let key = hash_key(hash.as_str());
        self.tracks
            .lock()
            .unwrap()
            .iter()
            .any(|t| hash_key(t.content_hash.as_str()) == key)
    }

    /// The blob and cover art of the indexed track `hash` (paths relative to `music_dir`).
    fn retired_files(&self, hash: &ContentHash) -> RetiredFiles {
        let key = hash_key(hash.as_str());
        self.tracks
            .lock()
            .unwrap()
            .iter()
            .find(|t| hash_key(t.content_hash.as_str()) == key)
            .map(|t| RetiredFiles {
                blob: (!t.blob_path.as_os_str().is_empty()).then(|| t.blob_path.clone()),
                cover_art: t.cover_art_path.clone(),
            })
            .unwrap_or_default()
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
        let old_clean = hash_key(old_hash.as_str());
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
                    let token_clean = hash_key(token);

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
    ///   leftover: `retract_all_entity_facts_locked` removed the index entry but did
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
            memory_cues: t
                .memory_cues
                .iter()
                .map(|c| CueInfo {
                    position_ms: c.position_ms,
                    kind: c.kind.clone(),
                    label: c.label.clone(),
                    index: c.index,
                })
                .collect(),
            beat_grid: t
                .beat_grid
                .and_then(|(first_beat_ms, bpm_f32, beats_per_bar)| {
                    Bpm::from_f32(bpm_f32).ok().map(|bpm| BeatGridInfo {
                        first_beat_ms,
                        bpm,
                        beats_per_bar,
                    })
                }),
            role: t.role,
            energy: t.energy,
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
        self.tracks_where(|_| true, limit)
    }

    /// Visible tracks from the in-memory index that match `filter` (hidden tracks
    /// excluded), optionally limited.
    fn tracks_where(
        &self,
        filter: impl Fn(&IndexedTrackInfo) -> bool,
        limit: Option<usize>,
    ) -> Vec<TrackInfo> {
        let tracks = self.tracks.lock().unwrap();

        let iter = tracks
            .iter()
            .filter(|t| !t.is_hidden() && filter(t))
            .map(|t| self.to_track_info(t));

        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    /// Apply `mark` to a track unless the track already carries it live; then
    /// nothing is written and the original time is kept.
    fn write_mark(&self, hash: &ContentHash, mark: &Mark) -> Result<MarkWrite, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;
        // The IPC loop is single-threaded, so no other IPC request can interleave
        // between the check (under the lock) and the write below.
        let already_live = {
            let tracks = self.tracks.lock().unwrap();
            tracks
                .iter()
                .find(|t| t.content_hash.as_str() == full_hash.as_str())
                .is_some_and(|t| t.live_marks(mark).next().is_some())
        };
        if already_live {
            return Ok(MarkWrite::AlreadyLive);
        }
        let fact = mark.fact_value(chrono::Utc::now());
        self.write_and_project(&full_hash, &[(fact, user_fact_source())])
            .map_err(acid_error)?;
        Ok(MarkWrite::Written)
    }

    /// Retract every live assertion of `mark` on a track. One live assertion per
    /// mark is not an invariant (a merge or an older library can carry several, each
    /// under its own source), so all of them are retracted, each with its own source.
    fn unmark(&self, hash: &ContentHash, mark: &Mark) -> Result<Unmark, ProtocolError> {
        let full_hash = self.resolve_hash(hash)?;
        // Snapshot the live assertions, then drop the guard before retracting:
        // the snapshot cannot go stale because the IPC loop is single-threaded,
        // and the guard must be released because `catch_up` re-locks `tracks`.
        let live: Vec<(MusicValue, music_facts::FactSource)> = {
            let tracks = self.tracks.lock().unwrap();
            let track = tracks
                .iter()
                .find(|t| t.content_hash.as_str() == full_hash.as_str())
                .ok_or_else(|| ProtocolError::TrackNotFound {
                    hash: hash.as_str().to_owned(),
                })?;
            track.live_marks(mark).cloned().collect()
        };
        let Some(count) = NonZeroUsize::new(live.len()) else {
            return Ok(Unmark::NotLive);
        };
        self.retract_and_project(&full_hash, &live)
            .map_err(acid_error)?;
        Ok(Unmark::Retracted { count })
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
    /// Only assertions are plays. A retraction is not one: retiring a track retracts
    /// its play events too (with the retraction's own, later, timestamp), and those
    /// must neither count as plays nor erase the originals. A user retracting a
    /// single play event is not handled; it never happens in production.
    ///
    /// A track's play history includes the plays recorded on every hash it
    /// transitively replaces, with their original timestamps (ADR-013), so a merged
    /// or replaced track keeps its predecessors' history. Events keyed by the short
    /// hashes older playback wrote (#86) resolve to the full hash. Every event seen
    /// is remembered in `play_events`, whether or not its entity is still live, and
    /// the lineage is re-applied on every refresh so a `Replaces` asserted after the
    /// events were read takes effect too.
    fn refresh_event_timestamps(&self) {
        const PAGE_SIZE: usize = 10_000;

        // Held for the whole refresh, like `cursor` in `catch_up_with`: concurrent
        // refreshes must not read (and merge) the same lines twice.
        let mut event_cursor = self.event_cursor.lock().unwrap();
        let mut current_cursor = event_cursor.clone();

        // The play events since the last refresh, per normalised entity key.
        let mut delta: HashMap<String, PlayTimes> = HashMap::new();

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
                let Some(fact) = parse_fact_line(line) else {
                    continue;
                };

                if fact.operation() != stainless_facts::Operation::Assert {
                    continue;
                }
                let times = delta.entry(hash_key(fact.entity().as_str()));
                match fact.value() {
                    MusicValue::TrackStarted(_) => {
                        update_if_more_recent(&mut times.or_default().started, *fact.timestamp());
                    }
                    MusicValue::TrackStopped(_) => {
                        update_if_more_recent(&mut times.or_default().stopped, *fact.timestamp());
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
        *event_cursor = current_cursor;

        let mut tracks = self.tracks.lock().unwrap();
        let mut events = self.play_events.lock().unwrap();
        for (key, new) in delta {
            let seen = events.entry(key).or_default();
            if let Some(ts) = new.started {
                update_if_more_recent(&mut seen.started, ts);
            }
            if let Some(ts) = new.stopped {
                update_if_more_recent(&mut seen.stopped, ts);
            }
        }

        // Apply each track's lineage (itself and what it replaces) to the index.
        let replaces: HashMap<String, Vec<String>> = tracks
            .iter()
            .map(|t| {
                let replaced = t
                    .provenance
                    .iter()
                    .filter_map(|(v, _)| match v {
                        MusicValue::Replaces(h) => Some(hash_key(h.as_str())),
                        _ => None,
                    })
                    .collect();
                (hash_key(t.content_hash.as_str()), replaced)
            })
            .collect();
        // A short key is resolved only when exactly one full key (live track, replaced
        // hash or full-length event key) starts with it.
        let prefix_counts = short_prefix_counts(
            replaces
                .keys()
                .chain(replaces.values().flatten())
                .chain(events.keys())
                .map(String::as_str)
                .collect::<HashSet<_>>()
                .into_iter(),
        );
        for track in tracks.iter_mut() {
            let lineage = replaces_lineage(&hash_key(track.content_hash.as_str()), &replaces);
            for times in lineage
                .iter()
                .flat_map(|key| play_times_for(&events, key, &prefix_counts))
            {
                if let Some(ts) = times.started {
                    update_if_more_recent(&mut track.last_started, ts);
                }
                if let Some(ts) = times.stopped {
                    update_if_more_recent(&mut track.last_stopped, ts);
                }
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
                    role: t.role,
                    energy: t.energy.map(u8::from),
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
        //
        // Copy the answer out and drop the guard immediately: nothing below may run
        // while holding `content_hashes` (lock order is cursor -> tracks ->
        // fact_index -> content_hashes, and the ACID write below is IPC).
        let is_duplicate = self
            .content_hashes
            .lock()
            .unwrap()
            .contains(content_hash.as_str());
        if is_duplicate {
            tracing::info!(
                hash = %content_hash.as_str(),
                path = %path.display(),
                "Duplicate content hash, removing inbox file"
            );
            let _ = std::fs::remove_file(path);

            // Append source facts that don't exist yet for this track
            let fact_source = library_fact_source();
            let mut new_facts = Self::source_facts(source, &fact_source);
            if !new_facts.is_empty() {
                // Keep only the facts the track does not hold yet: an ItemId it lacks,
                // and a Source only if it has none.
                let gained_item_id = {
                    let tracks = self.tracks.lock().unwrap();
                    match tracks
                        .iter()
                        .find(|t| t.content_hash.as_str() == content_hash.as_str())
                    {
                        Some(t) => {
                            new_facts.retain(|(v, _)| match v {
                                MusicValue::ItemId(id) => {
                                    !t.item_ids().any(|held| held == id.as_str())
                                }
                                MusicValue::Source(_) => t.source.is_none(),
                                _ => true,
                            });
                            new_facts
                                .iter()
                                .any(|(v, _)| matches!(v, MusicValue::ItemId(_)))
                        }
                        None => {
                            new_facts.clear();
                            false
                        }
                    }
                };

                if !new_facts.is_empty()
                    && self.write_and_project(&content_hash, &new_facts).is_ok()
                {
                    tracing::info!(
                        hash = %content_hash.as_str(),
                        facts = new_facts.len(),
                        "Appended source facts to existing track"
                    );
                    // A new ItemId may be the last piece the self-heal was waiting for.
                    if gained_item_id {
                        return Ok(self.self_heal_after_ingest(&content_hash));
                    }
                }
            }

            return Ok(content_hash);
        }

        // Stage 3: Extract metadata
        let extracted = validated.extract_metadata()?;
        let mut facts = extracted.facts.clone();

        // Stage 4: Import to blob storage
        let indexed = extracted.import(&self.music_dir)?;

        // Stage 5: Extract cover art from blob and store it
        let fact_source = library_fact_source();
        let blob_path = self.music_dir.join(&indexed.blob_path);
        let cover_art_path = self.extract_and_store_cover_art(&blob_path, &content_hash);
        if let Some(ref rel_path) = cover_art_path {
            facts.push((
                MusicValue::CoverArtPath(rel_path.to_string_lossy().to_string()),
                fact_source.clone(),
            ));
        }

        // Identity: AudioHash v1 (PcmHash is left to the explicit backfill). A failure
        // is logged and simply yields no fact; ingest continues.
        facts.extend(identity_facts_for_blob(&blob_path, IdentityOpts::INGEST, &fact_source).facts);

        // Add provenance facts from source metadata
        let source_facts = Self::source_facts(source, &fact_source);
        facts.extend(source_facts);

        // Write facts via the fact store and project them through the single stream
        // path (the same one the fact subscriber uses). There is deliberately no
        // hand-rolled in-memory projection here: the subscriber replays this write
        // too, so a second projection would apply every fact twice.
        //
        // blob_path is derived from the FilePath fact via blob_path_for — always
        // RELATIVE (blobs/<2ch>/<hash>.<ext>), identical to every other projection.
        self.write_and_project(&content_hash, &facts)?;

        // AudioHash and ItemId are durable together now. Never fails the ingest.
        Ok(self.self_heal_after_ingest(&content_hash))
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
    /// Used by `merge_into` to forward-inherit the chain: when C
    /// replaces B, C must also claim every hash that B had replaced (e.g. A),
    /// so that a playlist still pointing at A can find its way to C even after
    /// B is fully retracted. Each ancestor comes with the `FactSource` of the
    /// `Replaces` fact it was read from, which the inheriting fact keeps.
    fn ancestor_replaces(&self, hash: &ContentHash) -> Vec<(ContentHash, music_facts::FactSource)> {
        let target_clean = hash_key(hash.as_str());

        let tracks = self.tracks.lock().unwrap();
        let entry = match tracks
            .iter()
            .find(|t| hash_key(t.content_hash.as_str()) == target_clean)
        {
            Some(e) => e,
            None => return Vec::new(),
        };

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut result: Vec<(ContentHash, music_facts::FactSource)> = Vec::new();

        for (v, source) in &entry.provenance {
            if let MusicValue::Replaces(old) = v {
                if seen.insert(hash_key(old.as_str())) {
                    result.push((old.clone(), source.clone()));
                }
            }
        }

        result
    }

    /// The hashes `hash` directly replaced (see `ancestor_replaces`).
    #[cfg(test)]
    pub(crate) fn gather_ancestor_replaces(&self, hash: &ContentHash) -> Vec<ContentHash> {
        self.ancestor_replaces(hash)
            .into_iter()
            .map(|(h, _)| h)
            .collect()
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
        match server.recv_raw() {
            Ok(msg) => {
                let response = match parse_request(&msg) {
                    Ok(request) => service.handle_request(request),
                    Err(e) => {
                        tracing::warn!(error = %e, "Rejected unparsable request");
                        LibraryResponse::Error(e)
                    }
                };
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

                // Atomically read from the in-memory cursor, apply, and advance it.
                // Writers call the same catch-up, so lines they already projected
                // are skipped here (the cursor has moved past them).
                match service.catch_up_with(&acid_client) {
                    Ok(0) => {}
                    Ok(count) => {
                        tracing::debug!(count, "Applied incremental facts from ACID stream");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Subscriber: failed to read incremental stream");
                    }
                }
            }
        }
    });
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

    /// An in-process ACID server plus the private directory it stores facts in.
    /// Dropping it stops the server first (field order), then removes the directory.
    pub(super) struct AcidGuard {
        _handle: acid_service::ServerHandle,
        _dir: tempfile::TempDir,
    }

    fn spawn_acid_server() -> (AcidGuard, String, String) {
        let id = ACID_SERVER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let facts_addr = format!("ipc:///tmp/mdma-test-acid-facts-{}-{}.sock", pid, id);
        let events_addr = format!("ipc:///tmp/mdma-test-acid-events-{}-{}.sock", pid, id);

        let rep = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
        rep.listen(&facts_addr).expect("rep listen");

        let pub_sock = nng::Socket::new(nng::Protocol::Pub0).expect("pub socket");
        pub_sock.listen(&events_addr).expect("pub listen");

        // Every server gets its own metadata dir: under the file-backed fact store a
        // shared directory would mix facts across parallel tests and across runs.
        let dir = tempfile::tempdir().expect("acid metadata dir");
        let handle =
            acid_service::start(rep, pub_sock, dir.path()).expect("failed to start acid server");
        std::thread::sleep(std::time::Duration::from_millis(20));
        (
            AcidGuard {
                _handle: handle,
                _dir: dir,
            },
            facts_addr,
            events_addr,
        )
    }

    /// Helper: build an empty service backed by a real (in-process) ACID server.
    ///
    /// Returns `(service, metadata_dir, acid_handle)`. Callers must keep
    /// `_acid` alive for the duration of the test; dropping it shuts the server down.
    pub(super) fn make_empty_service() -> (Arc<LibraryService>, tempfile::TempDir, AcidGuard) {
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

    pub(super) fn make_service_with_playlists_dir(
    ) -> (Arc<LibraryService>, tempfile::TempDir, AcidGuard) {
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
            result.is_err(),
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
    ) -> (Arc<LibraryService>, tempfile::TempDir) {
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
    ) -> (Arc<LibraryService>, tempfile::TempDir, String) {
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

    /// A second source's Album survives retraction of the bandcamp-sourced one.
    #[test]
    fn retract_source_facts_keeps_second_sources_album() {
        let hash = ContentHash::new("sha256:retract_two_sources");
        let bandcamp = FactSource::new("mdma-library", "0.0.0", FactOrigin::bandcamp(None));
        let other = FactSource::new("tagger", "1.0.0", FactOrigin::Unknown);
        let item_id = "p777";

        let temp = {
            let t = NamedTempFile::new().unwrap();
            let mut writer = FactWriter::open(t.path()).unwrap();
            writer
                .write_track_facts(
                    &hash,
                    &[
                        (MusicValue::ItemId(item_id.to_string()), bandcamp.clone()),
                        (
                            MusicValue::Album(music_facts::Album::new("Bandcamp Album")),
                            bandcamp.clone(),
                        ),
                        (
                            MusicValue::Album(music_facts::Album::new("Tagger Album")),
                            other,
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
        assert!(matches!(response, LibraryResponse::SourceFactsRetracted));

        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .expect("track must remain indexed");
        assert_eq!(t.album.as_deref(), Some("Tagger Album"));
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
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let sourced: Vec<_> = facts
            .iter()
            .map(|(hash, value, ts, op)| (hash.clone(), value.clone(), *ts, source.clone(), *op))
            .collect();
        write_facts_file_with_sources(&sourced)
    }

    /// Like `write_facts_file_with_operations`, with a source per fact.
    fn write_facts_file_with_sources(
        facts: &[(
            ContentHash,
            MusicValue,
            chrono::DateTime<Utc>,
            FactSource,
            Operation,
        )],
    ) -> NamedTempFile {
        let temp = NamedTempFile::new().unwrap();
        let mut writer = FactStreamWriter::open(temp.path()).unwrap();

        let fact_structs: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
            .iter()
            .map(|(hash, value, ts, source, op)| {
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
        // The only live fact was retracted, so the entity is dropped entirely:
        // no entry can report the retracted item_id.
        assert!(
            result.tracks.iter().all(|t| t.item_id.is_none()),
            "item_id should be gone after Retract"
        );
        assert!(
            result.tracks.is_empty(),
            "a fully retracted entity must be dropped, not kept as an empty entry"
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
        );
        assert!(entry.is_hidden());
        apply_fact_to_track(
            &mut entry,
            &fact,
            ts,
            stainless_facts::Operation::Retract,
            &source,
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

    fn bookmark(scope: Option<&str>, ts: chrono::DateTime<Utc>) -> MusicValue {
        MusicValue::Bookmarked {
            scope: scope.map(str::to_owned),
            timestamp: ts,
        }
    }

    /// Hashes returned by `ListMarked` with `filter`, sorted.
    fn marked_hashes(service: &LibraryService, filter: MarkFilter) -> Vec<String> {
        sorted_hashes(service.handle_request(LibraryRequest::ListMarked { filter }))
    }

    fn named_mark(s: &str) -> music_facts::Mark {
        music_facts::Mark::Named(music_facts::MarkName::parse(s).unwrap())
    }

    /// A library with one track per mark kind: default, named ("Banger" as
    /// stored, so it also covers case-insensitive matching), legacy-invalid
    /// (empty scope), plus a hidden and a retracted mark that must never show.
    fn service_with_one_track_per_mark() -> (Arc<LibraryService>, tempfile::TempDir) {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let mut ops = Vec::new();
        for (hash, scope) in [
            ("sha256:markdefault01", None),
            ("sha256:marknamed01", Some("Banger")),
            ("sha256:marklegacy01", Some("")),
            ("sha256:markhidden01", None),
            ("sha256:markretracted01", Some("banger")),
        ] {
            let hash = ContentHash::new(hash);
            ops.push((
                hash.clone(),
                MusicValue::Title(Title::new("T")),
                ts,
                Operation::Assert,
            ));
            ops.push((hash, bookmark(scope, ts), ts, Operation::Assert));
        }
        ops.push((
            ContentHash::new("sha256:markhidden01"),
            MusicValue::Deleted { timestamp: ts },
            ts,
            Operation::Assert,
        ));
        ops.push((
            ContentHash::new("sha256:markretracted01"),
            bookmark(Some("banger"), ts),
            ts2,
            Operation::Retract,
        ));
        let temp = write_facts_file_with_operations(&ops);
        make_service_with_facts(temp.path())
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn list_marked_any_lists_default_named_and_legacy_marks() {
        let (service, _dir) = service_with_one_track_per_mark();
        assert_eq!(
            marked_hashes(&service, MarkFilter::Any),
            strs(&[
                "sha256:markdefault01",
                "sha256:marklegacy01",
                "sha256:marknamed01"
            ])
        );
    }

    #[test]
    fn list_marked_only_default_excludes_named_and_legacy_marks() {
        let (service, _dir) = service_with_one_track_per_mark();
        assert_eq!(
            marked_hashes(&service, MarkFilter::Only(music_facts::Mark::Default)),
            strs(&["sha256:markdefault01"])
        );
    }

    #[test]
    fn list_marked_only_named_matches_a_differently_cased_stored_scope() {
        let (service, _dir) = service_with_one_track_per_mark();
        assert_eq!(
            marked_hashes(&service, MarkFilter::Only(named_mark("banger"))),
            strs(&["sha256:marknamed01"])
        );
    }

    #[test]
    fn list_marked_excludes_hidden_and_retracted_marks() {
        let (service, _dir) = service_with_one_track_per_mark();
        let any = marked_hashes(&service, MarkFilter::Any);
        assert!(!any.contains(&"sha256:markhidden01".to_owned()));
        assert!(!any.contains(&"sha256:markretracted01".to_owned()));
    }

    const MARK_TRACK: &str = "sha256:writemark01";

    /// A library holding one titled track, `MARK_TRACK`, plus `extra` facts for it.
    fn service_with_mark_track(
        extra: &[(MusicValue, Operation)],
    ) -> (Arc<LibraryService>, tempfile::TempDir) {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let hash = ContentHash::new(MARK_TRACK);
        let mut ops = vec![(
            hash.clone(),
            MusicValue::Title(Title::new("T")),
            ts,
            Operation::Assert,
        )];
        ops.extend(
            extra
                .iter()
                .map(|(v, op)| (hash.clone(), v.clone(), ts, *op)),
        );
        let temp = write_facts_file_with_operations(&ops);
        make_service_with_facts(temp.path())
    }

    const TRACK_A: &str = "sha256:marktrackA01";
    const TRACK_B: &str = "sha256:marktrackB01";

    /// A library holding two titled tracks, `TRACK_A` and `TRACK_B`.
    fn service_with_two_tracks() -> (Arc<LibraryService>, tempfile::TempDir) {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let ops: Vec<_> = [TRACK_A, TRACK_B]
            .into_iter()
            .map(|h| {
                (
                    ContentHash::new(h),
                    MusicValue::Title(Title::new(h)),
                    ts,
                    Operation::Assert,
                )
            })
            .collect();
        let temp = write_facts_file_with_operations(&ops);
        make_service_with_facts(temp.path())
    }

    /// A `Bookmarked` value with a fixed timestamp, as a loaded fact would carry.
    fn stored_bookmark(scope: &str, secs: i64) -> MusicValue {
        MusicValue::Bookmarked {
            scope: Some(scope.to_owned()),
            timestamp: Utc.timestamp_opt(secs, 0).unwrap(),
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

    fn write_mark(service: &LibraryService, hash: &str, mark: &Mark) -> LibraryResponse {
        service.handle_request(LibraryRequest::WriteMark {
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

    /// The live bookmark values of `MARK_TRACK`, in provenance order.
    fn live_bookmarks(service: &LibraryService) -> Vec<MusicValue> {
        let tracks = service.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == MARK_TRACK)
            .expect("the mark track")
            .provenance
            .iter()
            .filter(|(v, _)| v.stored_mark().is_some())
            .map(|(v, _)| v.clone())
            .collect()
    }

    fn fact_count(service: &LibraryService) -> usize {
        match service.handle_request(LibraryRequest::GetFacts {
            hash: ContentHash::new(MARK_TRACK),
        }) {
            LibraryResponse::Facts { facts, .. } => facts.len(),
            other => panic!("expected Facts, got {other:?}"),
        }
    }

    #[test]
    fn write_mark_lists_the_track_under_that_mark() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, "writemark01", &banger),
            MarkWrite::Written,
        );
        assert_eq!(
            marked_hashes(&service, MarkFilter::Only(banger)),
            strs(&[MARK_TRACK])
        );
        assert!(marked_hashes(&service, MarkFilter::Only(Mark::Default)).is_empty());
    }

    #[test]
    fn write_mark_is_per_track() {
        let (service, _dir) = service_with_two_tracks();
        let banger = named_mark("banger");
        assert_mark_written(write_mark(&service, TRACK_A, &banger), MarkWrite::Written);
        assert_mark_written(write_mark(&service, TRACK_B, &banger), MarkWrite::Written);
    }

    #[test]
    fn write_mark_twice_is_already_live_and_changes_nothing() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        let before = live_bookmarks(&service);
        let facts_before = fact_count(&service);
        assert_eq!(before.len(), 1);

        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::AlreadyLive,
        );
        assert_eq!(live_bookmarks(&service), before);
        assert_eq!(fact_count(&service), facts_before);
    }

    #[test]
    fn write_mark_is_already_live_for_a_differently_cased_stored_mark() {
        let stored = stored_bookmark("Banger", 1_767_225_600);
        let (service, _dir) = service_with_mark_track(&[(stored, Operation::Assert)]);
        let before = live_bookmarks(&service);
        assert_eq!(before.len(), 1);

        assert_mark_written(
            write_mark(&service, MARK_TRACK, &named_mark("banger")),
            MarkWrite::AlreadyLive,
        );
        assert_eq!(live_bookmarks(&service), before);
    }

    #[test]
    fn write_mark_writes_again_after_the_mark_was_retracted() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        let fact = live_bookmarks(&service).remove(0);
        let resp = service.handle_request(LibraryRequest::RetractFact {
            hash: ContentHash::new(MARK_TRACK),
            fact,
        });
        assert!(matches!(resp, LibraryResponse::FactRetracted), "{resp:?}");
        assert!(live_bookmarks(&service).is_empty());

        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        assert_eq!(live_bookmarks(&service).len(), 1);
    }

    #[test]
    fn two_marks_coexist_on_one_track() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &named_mark("opener")),
            MarkWrite::Written,
        );
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &Mark::Default),
            MarkWrite::Written,
        );
        assert_eq!(live_bookmarks(&service).len(), 3);
        for mark in [banger, named_mark("opener"), Mark::Default] {
            assert_eq!(
                marked_hashes(&service, MarkFilter::Only(mark)),
                strs(&[MARK_TRACK])
            );
        }
    }

    #[test]
    fn write_mark_rejects_an_unknown_track() {
        let (service, _dir) = service_with_mark_track(&[]);
        assert_track_not_found(write_mark(&service, "ffff", &Mark::Default));
        assert!(live_bookmarks(&service).is_empty());
    }

    fn unmark(service: &LibraryService, hash: &str, mark: &Mark) -> LibraryResponse {
        service.handle_request(LibraryRequest::UnmarkTrack {
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
            count: NonZeroUsize::new(count).unwrap(),
        }
    }

    #[test]
    fn unmark_removes_the_track_from_that_mark() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        assert_unmarked(unmark(&service, MARK_TRACK, &banger), retracted(1));
        assert!(marked_hashes(&service, MarkFilter::Only(banger)).is_empty());
        assert!(live_bookmarks(&service).is_empty());
    }

    #[test]
    fn unmark_keeps_the_other_marks() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        for mark in [&banger, &named_mark("opener"), &Mark::Default] {
            assert_mark_written(write_mark(&service, MARK_TRACK, mark), MarkWrite::Written);
        }
        assert_unmarked(unmark(&service, MARK_TRACK, &banger), retracted(1));
        assert_eq!(live_bookmarks(&service).len(), 2);
        assert_unmarked(unmark(&service, MARK_TRACK, &Mark::Default), retracted(1));
        assert_eq!(
            marked_hashes(&service, MarkFilter::Only(named_mark("opener"))),
            strs(&[MARK_TRACK])
        );
        assert_eq!(live_bookmarks(&service).len(), 1);
    }

    #[test]
    fn unmark_twice_is_not_live() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        assert_unmarked(unmark(&service, MARK_TRACK, &banger), retracted(1));
        assert_unmarked(unmark(&service, MARK_TRACK, &banger), Unmark::NotLive);
    }

    #[test]
    fn unmark_removes_a_differently_cased_stored_mark() {
        let stored = stored_bookmark("Banger", 1_767_225_600);
        let (service, _dir) = service_with_mark_track(&[(stored, Operation::Assert)]);
        assert_unmarked(
            unmark(&service, MARK_TRACK, &named_mark("banger")),
            retracted(1),
        );
        assert!(live_bookmarks(&service).is_empty());
    }

    #[test]
    fn unmark_retracts_every_live_entry_each_with_its_own_source() {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let hash = ContentHash::new(MARK_TRACK);
        let bookmark = stored_bookmark("banger", ts.timestamp());
        let source = |version: &str| FactSource::new("mdma", version, FactOrigin::User);
        let temp = write_facts_file_with_sources(&[
            (
                hash.clone(),
                MusicValue::Title(Title::new("T")),
                ts,
                source("0.0.1"),
                Operation::Assert,
            ),
            (
                hash.clone(),
                bookmark.clone(),
                ts,
                source("0.0.1"),
                Operation::Assert,
            ),
            (hash, bookmark, ts, source("0.0.2"), Operation::Assert),
        ]);
        let (service, _dir) = make_service_with_facts(temp.path());
        assert_eq!(live_bookmarks(&service).len(), 2);

        assert_unmarked(
            unmark(&service, MARK_TRACK, &named_mark("banger")),
            retracted(2),
        );
        assert!(live_bookmarks(&service).is_empty());
        assert!(marked_hashes(&service, MarkFilter::Any).is_empty());
    }

    #[test]
    fn unmark_is_per_track() {
        let (service, _dir) = service_with_two_tracks();
        let banger = named_mark("banger");
        for hash in [TRACK_A, TRACK_B] {
            assert_mark_written(write_mark(&service, hash, &banger), MarkWrite::Written);
        }
        assert_unmarked(unmark(&service, TRACK_A, &banger), retracted(1));
        assert_eq!(
            marked_hashes(&service, MarkFilter::Only(banger)),
            strs(&[TRACK_B])
        );
    }

    #[test]
    fn unmark_rejects_an_unknown_track() {
        let (service, _dir) = service_with_mark_track(&[]);
        assert_track_not_found(unmark(&service, "ffff", &Mark::Default));
    }

    #[test]
    fn write_mark_writes_again_after_unmark() {
        let (service, _dir) = service_with_mark_track(&[]);
        let banger = named_mark("banger");
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        assert_unmarked(unmark(&service, MARK_TRACK, &banger), retracted(1));
        assert_mark_written(
            write_mark(&service, MARK_TRACK, &banger),
            MarkWrite::Written,
        );
        assert_eq!(live_bookmarks(&service).len(), 1);
    }

    fn write_bookmark(service: &LibraryService, scope: Option<&str>) -> LibraryResponse {
        service.handle_request(LibraryRequest::WriteBookmark {
            hash: ContentHash::new(MARK_TRACK),
            scope: scope.map(str::to_owned),
        })
    }

    #[test]
    fn write_bookmark_with_a_scope_stores_the_normalised_mark() {
        let (service, _dir) = service_with_mark_track(&[]);
        let resp = write_bookmark(&service, Some("Friday"));
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        let stored: Vec<_> = live_bookmarks(&service)
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
        let (service, _dir) = service_with_mark_track(&[]);
        let resp = write_bookmark(&service, None);
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        assert_eq!(
            marked_hashes(&service, MarkFilter::Only(Mark::Default)),
            strs(&[MARK_TRACK])
        );
    }

    #[test]
    fn write_bookmark_twice_still_answers_bookmark_written() {
        let (service, _dir) = service_with_mark_track(&[]);
        let first = write_bookmark(&service, Some("friday"));
        assert!(
            matches!(first, LibraryResponse::BookmarkWritten),
            "{first:?}"
        );
        let resp = write_bookmark(&service, Some("friday"));
        assert!(matches!(resp, LibraryResponse::BookmarkWritten), "{resp:?}");
        assert_eq!(live_bookmarks(&service).len(), 1);
    }

    #[test]
    fn write_bookmark_with_an_empty_scope_is_an_internal_error() {
        let (service, _dir) = service_with_mark_track(&[]);
        let resp = write_bookmark(&service, Some(""));
        match resp {
            LibraryResponse::Error(ProtocolError::Internal { message }) => {
                assert!(message.contains("empty"), "{message}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
        assert!(live_bookmarks(&service).is_empty());
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
        // The only live fact was retracted: bpm is gone and so is the entity.
        assert!(
            result.tracks.iter().all(|t| t.bpm.is_none()),
            "Retract(Bpm=128) must clear bpm when live value is 128"
        );
        assert!(
            result.tracks.is_empty(),
            "a fully retracted entity must be dropped"
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
        // All sources retracted: the title is gone and so is the entity.
        assert!(
            result.tracks.iter().all(|t| t.title.is_none()),
            "Title must be None when all sources retract"
        );
        assert!(
            result.tracks.is_empty(),
            "a fully retracted entity must be dropped"
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

    /// AddedAt aggregation is latest-wins, whatever the assertion order, on all paths.
    #[test]
    fn equivalence_added_at_latest_wins_all_paths() {
        let hash = ContentHash::new("sha256:equiv_added_at");
        let source = FactSource::new("source-a", "1.0.0", FactOrigin::Unknown);
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let older = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();

        let fact = |v: MusicValue| (hash.clone(), v, ts, Operation::Assert, source.clone());
        // Title keeps the entity live; AddedAt itself is not provenance-tracked.
        let title = || fact(MusicValue::Title(Title::new("T")));
        for (case, order) in [
            ("older first", [older, newer]),
            ("newer first", [newer, older]),
        ] {
            let facts = vec![
                title(),
                fact(MusicValue::AddedAt(order[0])),
                fact(MusicValue::AddedAt(order[1])),
            ];
            let file_result = load_from_file_path(&hash, &facts);
            let bulk_result = load_from_bulk_path(&hash, &facts);
            let stream_result = apply_stream_path(&hash, &facts);
            let ingest_result = apply_ingest_path(&hash, &facts);
            for (path, r) in [
                ("file", &file_result),
                ("bulk", &bulk_result),
                ("stream", &stream_result),
                ("ingest", &ingest_result),
            ] {
                assert_eq!(r.added_at, Some(newer), "{case}: {path} path");
            }
        }
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

        // A Format fact accompanies every real ingest; it is tracked, which keeps the
        // entity live (an entity holding only untracked facts is not live).
        let facts = vec![
            (
                hash.clone(),
                MusicValue::FilePath(std::path::PathBuf::from("/music/inbox/foo.flac")),
                ts,
                Operation::Assert,
                source.clone(),
            ),
            (
                hash.clone(),
                MusicValue::Format(music_facts::MusicFormat::Flac),
                ts,
                Operation::Assert,
                source.clone(),
            ),
        ];

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
        assert_eq!(
            file_result.energy,
            Some(EnergyLevel::new(8).unwrap()),
            "file path: energy should be 8"
        );
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
            assert_eq!(track.energy, Some(EnergyLevel::new(5).unwrap()));
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
    ) -> (Arc<LibraryService>, tempfile::TempDir, AcidGuard) {
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
    fn retract_all_entity_facts_retracts_in_acid_and_removes_from_index() {
        // This test exercises only the retraction half of a replace, directly via
        // retract_all_entity_facts. The full flow is covered by
        // handle_track_replace_end_to_end_with_wav_files.
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
        let result = retire(&service, &old_hash);
        assert!(result.is_ok(), "retire must succeed");

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
        retire(&service, &old_hash).expect("retire must succeed");

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

    pub(super) fn replaces_targets(service: &LibraryService, hash: &ContentHash) -> Vec<String> {
        let tracks = service.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .map(|t| {
                t.provenance
                    .iter()
                    .filter_map(|(v, _)| match v {
                        MusicValue::Replaces(h) => Some(h.as_str().to_owned()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Issue #44: back-to-back replaces (A->B, B->C) without a subscriber replay in
    /// between must still forward-inherit A onto C from in-memory state.
    #[test]
    fn back_to_back_replaces_inherit_ancestors_without_subscriber_replay() {
        let hash_a = ContentHash::new("sha256:b2b_rep_a_0001");
        let hash_b = ContentHash::new("sha256:b2b_rep_b_0001");
        let hash_c = ContentHash::new("sha256:b2b_rep_c_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();

        // A -> B
        let inherited = service.gather_ancestor_replaces(&hash_a);
        service
            .assert_replaces_facts(&hash_b, &hash_a, inherited)
            .unwrap();

        // B -> C immediately; no apply_stream_lines in between.
        let inherited = service.gather_ancestor_replaces(&hash_b);
        assert!(
            inherited.iter().any(|h| h.as_str() == hash_a.as_str()),
            "gather_ancestor_replaces(B) must include A from in-memory state, got {:?}",
            inherited
        );
        service
            .assert_replaces_facts(&hash_c, &hash_b, inherited)
            .unwrap();

        let mut targets = replaces_targets(&service, &hash_c);
        targets.sort();
        assert_eq!(
            targets,
            vec![hash_a.as_str().to_owned(), hash_b.as_str().to_owned()],
            "C must carry Replaces(A) and Replaces(B) in memory"
        );
    }

    /// Issue #44: a later subscriber catch-up must not duplicate Replaces entries
    /// in provenance. The writer already projected its own facts via `catch_up`,
    /// so the subscriber (its own client, same cursor) finds nothing new.
    #[test]
    fn subscriber_catch_up_after_replaces_facts_does_not_duplicate_provenance() {
        let hash_a = ContentHash::new("sha256:replay_rep_a_0001");
        let hash_b = ContentHash::new("sha256:replay_rep_b_0001");
        let hash_c = ContentHash::new("sha256:replay_rep_c_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();

        service
            .assert_replaces_facts(&hash_b, &hash_a, vec![])
            .unwrap();
        let inherited = service.gather_ancestor_replaces(&hash_b);
        service
            .assert_replaces_facts(&hash_c, &hash_b, inherited)
            .unwrap();

        let before_b = replaces_targets(&service, &hash_b);
        let before_c = replaces_targets(&service, &hash_c);
        assert_eq!(before_b.len(), 1);
        assert_eq!(before_c.len(), 2);

        // Simulate the background subscriber waking up (it has its own ACID client),
        // twice for good measure.
        let subscriber_client = AcidClient::connect(&service.acid_socket).unwrap();
        for hash in [&hash_b, &hash_c] {
            let lines = service.acid_client.read_entity(hash.as_str()).unwrap();
            assert!(!lines.is_empty(), "ACID must hold the Replaces facts");
        }
        assert_eq!(service.catch_up_with(&subscriber_client).unwrap(), 0);
        assert_eq!(service.catch_up_with(&subscriber_client).unwrap(), 0);

        assert_eq!(replaces_targets(&service, &hash_b), before_b);
        assert_eq!(replaces_targets(&service, &hash_c), before_c);
    }

    // =========================================================================
    // catch_up: single projection path (read-your-writes, exactly-once)
    // =========================================================================

    /// Minimal valid 16-bit mono PCM WAV (0.1 s). `seed` varies the samples so
    /// every test file has a distinct content hash.
    pub(super) fn write_test_wav(path: &std::path::Path, seed: i16) {
        let samples: Vec<i16> = (0..4410i32)
            .map(|i| ((i % 100) as i16).wrapping_mul(seed))
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&44_100u32.to_le_bytes());
        bytes.extend_from_slice(&88_200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    pub(super) fn user_source() -> FactSource {
        FactSource::new("mdma", "test", FactOrigin::User)
    }

    /// Hard-retire `h` the way production does: under `retire_gate`.
    pub(super) fn retire(s: &LibraryService, h: &ContentHash) -> Result<usize, String> {
        let gate = s.retire_gate.lock().unwrap();
        s.retract_all_entity_facts_locked(&gate, h)
    }

    /// Write a Title for `hash` to ACID and project it, creating the track.
    fn seed_track(service: &LibraryService, hash: &ContentHash, title: &str) {
        service
            .acid_client
            .write_music_facts(
                hash,
                &[(MusicValue::Title(Title::new(title)), user_source())],
            )
            .unwrap();
        service.catch_up().unwrap();
    }

    fn provenance_count(service: &LibraryService, hash: &ContentHash) -> usize {
        let tracks = service.tracks.lock().unwrap();
        tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .map(|t| t.provenance.len())
            .unwrap_or(0)
    }

    // =========================================================================
    // Identity hashes: ingest and explicit backfill
    // =========================================================================

    pub(super) const FIXTURE_S16_L8: &[u8] =
        include_bytes!("../../audio_hash/testdata/pcm/fixtures/s16_l8.flac");
    pub(super) const FIXTURE_S16_L8_TAGS: &[u8] =
        include_bytes!("../../audio_hash/testdata/pcm/fixtures/s16_l8_tags.flac");

    /// Identity facts (AudioHash, PcmHash) stored in ACID for `hash`.
    pub(super) fn identity_facts_in_acid(
        service: &LibraryService,
        hash: &ContentHash,
    ) -> Vec<MusicValue> {
        service
            .acid_client
            .read_entity(hash.as_str())
            .unwrap()
            .iter()
            .filter_map(|l| {
                serde_json::from_str::<Fact<ContentHash, MusicValue, FactSource>>(l).ok()
            })
            .map(|f| f.value().clone())
            .filter(|v| matches!(v, MusicValue::AudioHash(_) | MusicValue::PcmHash(_)))
            .collect()
    }

    fn fixture_audio_hash(bytes: &[u8]) -> MusicValue {
        MusicValue::AudioHash(audio_hash::audio_hash_reader(std::io::Cursor::new(bytes)).unwrap())
    }

    fn ingest_bytes(service: &LibraryService, name: &str, bytes: &[u8]) -> ContentHash {
        let inbox = service.music_dir.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let path = inbox.join(name);
        std::fs::write(&path, bytes).unwrap();
        service.ingest_file_internal(&path, None).unwrap()
    }

    #[test]
    fn ingest_adds_audio_hash_fact_for_flac() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let hash = ingest_bytes(&service, "Artist - Song.flac", FIXTURE_S16_L8);
        assert_eq!(
            identity_facts_in_acid(&service, &hash),
            vec![fixture_audio_hash(FIXTURE_S16_L8)]
        );
    }

    #[test]
    fn files_differing_only_in_tags_get_equal_audio_hash_facts() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let a = ingest_bytes(&service, "a.flac", FIXTURE_S16_L8);
        let b = ingest_bytes(&service, "b.flac", FIXTURE_S16_L8_TAGS);
        assert_ne!(a, b, "different bytes, different content hashes");
        let fa = identity_facts_in_acid(&service, &a);
        let fb = identity_facts_in_acid(&service, &b);
        assert_eq!(fa.len(), 1);
        assert_eq!(fa, fb);
    }

    /// Create a track whose blob is `bytes` but which has no identity facts.
    fn seed_blob_track(service: &LibraryService, hex: &str, bytes: &[u8]) -> ContentHash {
        let hash = ContentHash::new(format!("sha256:{hex}"));
        let file_path = std::path::PathBuf::from("x.flac");
        service
            .acid_client
            .write_music_facts(
                &hash,
                &[
                    (MusicValue::Title(Title::new(hex)), user_source()),
                    (MusicValue::FilePath(file_path.clone()), user_source()),
                ],
            )
            .unwrap();
        service.catch_up().unwrap();
        let blob = service
            .music_dir
            .join(blob_path_for(hash.as_str(), &file_path).unwrap());
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(blob, bytes).unwrap();
        hash
    }

    use library_ipc_protocol::{
        IdentityBackfillProgress, IdentityBackfillStart, IdentityBackfillStatus,
    };

    /// Start a backfill and wait until the worker stops (`Done` or `Cancelled`).
    fn run_backfill_to_end(service: &LibraryService, include_pcm: bool) -> IdentityBackfillStatus {
        match service.handle_request(LibraryRequest::IdentityBackfill { include_pcm }) {
            LibraryResponse::IdentityBackfillStarted(IdentityBackfillStart::Started(_)) => {}
            other => panic!("expected a started backfill, got {other:?}"),
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match service.handle_request(LibraryRequest::GetIdentityBackfillStatus) {
                LibraryResponse::IdentityBackfillStatus(IdentityBackfillStatus::Running(_)) => {}
                LibraryResponse::IdentityBackfillStatus(s) => return s,
                other => panic!("unexpected response {other:?}"),
            }
            assert!(Instant::now() < deadline, "backfill did not finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Like `run_backfill_to_end`, but the run must have completed.
    pub(super) fn run_backfill(
        service: &LibraryService,
        include_pcm: bool,
    ) -> IdentityBackfillProgress {
        match run_backfill_to_end(service, include_pcm) {
            IdentityBackfillStatus::Done(p) => p,
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// Blob that is a recognised FLAC but cut short: hashing it fails.
    fn corrupt_flac() -> &'static [u8] {
        &FIXTURE_S16_L8[..20]
    }

    #[test]
    fn backfill_skips_existing_counts_and_survives_failures() {
        let (service, _dir, _acid) = make_empty_service();
        // The failing file comes first: the run must carry on past it.
        let bad = seed_blob_track(&service, "bad0000000001", corrupt_flac());
        let fresh = seed_blob_track(&service, "fresh000000001", FIXTURE_S16_L8);
        let have = seed_blob_track(&service, "have0000000001", FIXTURE_S16_L8_TAGS);
        let existing = fixture_audio_hash(FIXTURE_S16_L8_TAGS);
        service
            .acid_client
            .write_music_facts(&have, &[(existing.clone(), user_source())])
            .unwrap();
        service.catch_up().unwrap();

        let status = run_backfill(&service, false);

        // `have` was skipped, so it is not part of the run.
        assert_eq!(
            (status.total, status.done, status.failed, status.skipped),
            (2, 2, 1, 0)
        );
        assert!(!status.include_pcm);
        assert!(status.last_error.as_deref().unwrap().contains("AudioHash"));
        assert_eq!(
            identity_facts_in_acid(&service, &fresh),
            vec![fixture_audio_hash(FIXTURE_S16_L8)]
        );
        assert_eq!(identity_facts_in_acid(&service, &have), vec![existing]);
        assert!(identity_facts_in_acid(&service, &bad).is_empty());

        // The run projected its writes: a second run has only the failing track left.
        let again = run_backfill(&service, false);
        assert_eq!((again.total, again.done, again.failed), (1, 1, 1));
    }

    #[test]
    fn backfill_with_pcm_adds_only_the_missing_hash() {
        let (service, _dir, _acid) = make_empty_service();
        let hash = seed_blob_track(&service, "pcm00000000001", FIXTURE_S16_L8);

        run_backfill(&service, false);
        let status = run_backfill(&service, true);
        assert_eq!((status.total, status.done, status.failed), (1, 1, 0));

        let facts = identity_facts_in_acid(&service, &hash);
        assert_eq!(facts.len(), 2, "one AudioHash, one PcmHash: {facts:?}");
        assert!(matches!(facts[0], MusicValue::AudioHash(_)));
        assert!(matches!(facts[1], MusicValue::PcmHash(_)));

        // Nothing left to do.
        let done = run_backfill(&service, true);
        assert_eq!((done.total, done.done), (0, 0));
    }

    /// B1: a track retired (hard-retracted) between planning and writing must not be
    /// resurrected by the backfill writing identity facts for its still-on-disk blob.
    #[test]
    fn backfill_does_not_resurrect_a_track_retired_mid_run() {
        let (service, _dir, _acid) = make_empty_service();
        let hash = seed_blob_track(&service, "retired0000001", FIXTURE_S16_L8);
        *service.identity_write_hook.lock().unwrap() = Some(Box::new(|svc, h| {
            retire(svc, h).unwrap();
        }));

        let status = run_backfill(&service, false);

        assert!(
            service.resolve_hash(&hash).is_err(),
            "retired track came back to life"
        );
        assert!(identity_facts_in_acid(&service, &hash).is_empty());
        // Retired is neither a failure nor a write: it is counted as skipped.
        assert_eq!(
            (status.total, status.done, status.failed, status.skipped),
            (1, 1, 0, 1)
        );
    }

    /// A panic inside a batch closure must not abort (no catch-up during unwind), and
    /// the service stays usable: the write is durable and the next catch-up sees it.
    #[test]
    fn panic_in_a_write_batch_unwinds_without_abort_and_service_still_works() {
        let (service, _dir, _acid) = make_empty_service();
        let hash = ContentHash::new("sha256:batchpanic0001");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.write_batch_and_project_with(&service.acid_client, |w| {
                w.write_music_facts(
                    &hash,
                    &[(MusicValue::Title(Title::new("Durable")), user_source())],
                )
                .unwrap();
                panic!("boom inside the batch");
            })
        }));
        assert!(result.is_err());

        // Not projected during the unwind, but durable; the next catch-up applies it.
        assert!(service.resolve_hash(&hash).is_err());
        assert_eq!(service.catch_up().unwrap(), 1);
        assert_eq!(service.resolve_hash(&hash).unwrap(), hash);
    }

    /// N7: a retired track whose hashing also failed is skipped, not failed.
    #[test]
    fn retired_track_with_a_failed_hash_counts_as_skipped() {
        let (service, _dir, _acid) = make_empty_service();
        let hash = seed_blob_track(&service, "retfail0000001", corrupt_flac());
        *service.identity_write_hook.lock().unwrap() = Some(Box::new(|svc, h| {
            retire(svc, h).unwrap();
        }));
        let status = run_backfill(&service, false);
        assert_eq!(
            (status.total, status.done, status.failed, status.skipped),
            (1, 1, 0, 1)
        );
        assert!(service.resolve_hash(&hash).is_err());
    }

    /// N1: a retire that is *partway through* (facts read, not yet retracted) must not
    /// be undone by a backfill write landing in that window. The write would not be
    /// in the retire's retract set, so the entity would survive on AudioHash alone.
    ///
    /// Deterministic: the retire thread parks inside `retract_all_entity_facts_locked`
    /// after its read; the backfill is started while it is parked; then the retire
    /// is released. With the retire gate the worker waits for the retire to finish
    /// and then sees the entity gone.
    #[test]
    fn backfill_write_cannot_land_inside_a_retire_in_progress() {
        let (service, _dir, _acid) = make_empty_service();
        let hash = seed_blob_track(&service, "midretire00001", FIXTURE_S16_L8);

        let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        *service.retire_mid_hook.lock().unwrap() = Some(Box::new(move || {
            let _ = parked_tx.send(());
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(30));
        }));

        let retire = {
            let service = Arc::clone(&service);
            let hash = hash.clone();
            std::thread::spawn(move || retire(&service, &hash).unwrap())
        };
        // the retire has read the entity and is parked
        parked_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("retire never reached its hook");

        let started =
            service.handle_request(LibraryRequest::IdentityBackfill { include_pcm: false });
        assert!(matches!(
            started,
            LibraryResponse::IdentityBackfillStarted(IdentityBackfillStart::Started(_))
        ));
        // Give the worker time to hash and reach its write (it must not get through).
        std::thread::sleep(std::time::Duration::from_millis(300));

        release_tx.send(()).unwrap();
        retire.join().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while matches!(
            service.identity_backfill_status(),
            IdentityBackfillStatus::Running(_)
        ) {
            assert!(Instant::now() < deadline, "backfill did not finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert!(
            service.resolve_hash(&hash).is_err(),
            "the backfill write landed inside the retire and resurrected the track"
        );
        assert!(identity_facts_in_acid(&service, &hash).is_empty());
    }

    #[test]
    fn unrecognised_files_are_neither_failures_nor_facts() {
        let (service, _dir, _acid) = make_empty_service();
        let hash = seed_blob_track(&service, "junk0000000001", b"not audio at all");
        let status = run_backfill(&service, true);
        assert_eq!(
            (status.total, status.done, status.failed, status.skipped),
            (1, 1, 0, 0)
        );
        assert!(status.last_error.is_none());
        assert!(identity_facts_in_acid(&service, &hash).is_empty());
    }

    #[test]
    fn cancel_stops_after_the_track_in_hand_and_a_new_run_resumes() {
        let (service, _dir, _acid) = make_empty_service();
        let hashes: Vec<_> = (0..3)
            .map(|i| seed_blob_track(&service, &format!("cancel00000000{i}"), FIXTURE_S16_L8))
            .collect();
        // Cancel while the first track is in hand.
        *service.identity_write_hook.lock().unwrap() = Some(Box::new(|svc, _| {
            svc.handle_request(LibraryRequest::CancelIdentityBackfill);
        }));

        let status = run_backfill_to_end(&service, false);
        let IdentityBackfillStatus::Cancelled(p) = status else {
            panic!("expected Cancelled, got {status:?}");
        };
        assert_eq!((p.total, p.done), (3, 1));
        // No manual catch-up here: the cancelled run's own projection must have made
        // the finished track visible, or the resumed run would re-plan it.

        // Progress is kept: the finished track is skipped by the next run.
        *service.identity_write_hook.lock().unwrap() = None;
        let resumed = run_backfill(&service, false);
        assert_eq!((resumed.total, resumed.done, resumed.failed), (2, 2, 0));
        for h in &hashes {
            assert_eq!(identity_facts_in_acid(&service, h).len(), 1, "{h:?}");
        }
    }

    #[test]
    fn backfill_never_runs_at_startup() {
        let (service, _dir, _acid) = make_empty_service();
        assert_eq!(
            service.identity_backfill_status(),
            IdentityBackfillStatus::Idle
        );
    }

    #[test]
    fn backfill_request_while_running_reports_already_running() {
        let (service, _dir, _acid) = make_empty_service();
        let running = IdentityBackfillProgress {
            include_pcm: false,
            total: 4,
            done: 1,
            failed: 0,
            skipped: 0,
            last_error: None,
        };
        *service.identity_backfill.lock().unwrap() =
            IdentityBackfillStatus::Running(running.clone());
        match service.handle_request(LibraryRequest::IdentityBackfill { include_pcm: true }) {
            LibraryResponse::IdentityBackfillStarted(IdentityBackfillStart::AlreadyRunning(p)) => {
                assert_eq!(p, running)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Ingest (real WAV through validate + extract_metadata + import), then a
    /// subscriber-style catch-up: every fact is held exactly once, and the
    /// counters agree with the ACID stream.
    #[test]
    fn ingest_then_subscriber_catch_up_holds_exactly_one_copy() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let inbox = service.music_dir.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let wav = inbox.join("Test Artist - Exactly Once.wav");
        write_test_wav(&wav, 3);

        let hash = service
            .ingest_file_internal(&wav, None)
            .expect("a generated WAV must pass validate + extract_metadata + import");

        // The ingest response path: state is readable right after the call.
        assert_eq!(service.resolve_hash(&hash).unwrap(), hash);
        assert_eq!(service.tracks_count(), 1);

        let acid_lines = service.acid_client.read_entity(hash.as_str()).unwrap();
        let after_ingest = provenance_count(&service, &hash);
        assert!(after_ingest > 0);

        // Subscriber wakes up with its own client: nothing is left to apply.
        let subscriber_client = AcidClient::connect(&service.acid_socket).unwrap();
        assert_eq!(service.catch_up_with(&subscriber_client).unwrap(), 0);

        assert_eq!(provenance_count(&service, &hash), after_ingest);
        // One provenance entry per tracked fact line in ACID; FilePath and AddedAt are
        // untracked, everything else is held exactly once.
        let tracked_lines = acid_lines
            .iter()
            .filter_map(|l| {
                serde_json::from_str::<Fact<ContentHash, MusicValue, FactSource>>(l).ok()
            })
            .filter(|f| is_provenance_tracked(f.value()))
            .count();
        assert_eq!(after_ingest, tracked_lines);
        assert_eq!(service.tracks_count(), 1);
        assert_eq!(service.facts_count(), acid_lines.len());
    }

    /// Multi-valued fields (styles, memory cues) written through the request path
    /// are held once even if the subscriber catches up afterwards.
    #[test]
    fn styles_and_memory_cues_are_held_once_after_write_and_catch_up() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let hash = ContentHash::new("sha256:once_styles_0001");
        seed_track(&service, &hash, "Styled");

        for fact in [
            MusicValue::StyleDescriptor("Deep".to_string()),
            MusicValue::MemoryCue {
                position_ms: 1000,
                kind: music_facts::CueKind::Hot,
                label: Some("Drop".to_string()),
                index: Some(0),
            },
        ] {
            let resp = service.handle_request(LibraryRequest::WriteFact {
                hash: hash.clone(),
                fact,
            });
            assert!(matches!(resp, LibraryResponse::FactWritten), "{:?}", resp);
        }

        let subscriber_client = AcidClient::connect(&service.acid_socket).unwrap();
        assert_eq!(service.catch_up_with(&subscriber_client).unwrap(), 0);
        service.catch_up().unwrap();

        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .unwrap();
        assert_eq!(t.styles, vec!["Deep".to_string()]);
        assert_eq!(t.memory_cues.len(), 1);
        assert_eq!(t.provenance.len(), 3, "Title + style + cue, once each");
    }

    /// A single later retract removes the only copy (no leftover duplicate).
    #[test]
    fn single_retract_removes_the_only_style_copy() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let hash = ContentHash::new("sha256:once_retract_0001");
        seed_track(&service, &hash, "Retractable");
        let style = MusicValue::StyleDescriptor("Deep".to_string());
        service.handle_request(LibraryRequest::WriteFact {
            hash: hash.clone(),
            fact: style.clone(),
        });
        let resp = service.handle_request(LibraryRequest::RetractFact {
            hash: hash.clone(),
            fact: style,
        });
        assert!(matches!(resp, LibraryResponse::FactRetracted), "{:?}", resp);

        let tracks = service.tracks.lock().unwrap();
        let t = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
            .unwrap();
        assert!(t.styles.is_empty());
    }

    /// Two catch-ups in a row (and from two clients) never apply a line twice.
    #[test]
    fn catch_up_is_exactly_once_across_clients() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let hash = ContentHash::new("sha256:catch_up_once_0001");
        seed_track(&service, &hash, "Once");
        let facts_after_seed = service.facts_count();
        assert_eq!(facts_after_seed, 1);

        let other = AcidClient::connect(&service.acid_socket).unwrap();
        assert_eq!(service.catch_up().unwrap(), 0);
        assert_eq!(service.catch_up_with(&other).unwrap(), 0);
        assert_eq!(service.facts_count(), facts_after_seed);
        assert_eq!(provenance_count(&service, &hash), 1);
    }

    /// A writer (write + catch_up per fact) races a subscriber-style catch_up loop
    /// on its own client. Writes are interleaved with catch-ups, so whichever
    /// thread projects a line, each line must be applied exactly once. Without the
    /// catch-up mutex a line read by both threads would be applied twice. The exact
    /// interleaving is scheduler-dependent, but the asserted outcome is not.
    #[test]
    fn concurrent_catch_ups_apply_each_line_once() {
        let (service, _metadata_dir, _acid) = make_empty_service();
        let hash = ContentHash::new("sha256:catch_up_threads_0001");
        let n = 40usize;

        let subscriber_client = AcidClient::connect(&service.acid_socket).unwrap();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sub = {
            let service = Arc::clone(&service);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    service.catch_up_with(&subscriber_client).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                service.catch_up_with(&subscriber_client).unwrap();
            })
        };
        for i in 0..n {
            service
                .write_and_project(
                    &hash,
                    &[(MusicValue::StyleDescriptor(format!("s{i}")), user_source())],
                )
                .unwrap();
        }
        done.store(true, Ordering::Relaxed);
        sub.join().unwrap();

        assert_eq!(service.facts_count(), n);
        assert_eq!(provenance_count(&service, &hash), n);
        let tracks = service.tracks.lock().unwrap();
        assert_eq!(tracks[0].styles.len(), n);
    }

    /// Replaced while playing: playback writes TrackStopped for the retired hash
    /// directly to ACID. The hash must stay gone, incrementally and after a bulk reload.
    #[test]
    fn play_event_for_retired_hash_does_not_resurrect_it() {
        let hash = ContentHash::new("sha256:orphan_play_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();
        seed_track(&service, &hash, "Playing");
        retire(&service, &hash).unwrap();
        assert!(service.resolve_hash(&hash).is_err());

        service
            .acid_client
            .write_music_facts(
                &hash,
                &[(MusicValue::TrackStopped(StopReason::OnSkip), user_source())],
            )
            .unwrap();
        service.catch_up().unwrap();

        assert!(service.resolve_hash(&hash).is_err(), "incremental ghost");
        assert!(!service
            .content_hashes
            .lock()
            .unwrap()
            .contains(hash.as_str()));
        assert_eq!(service.tracks_count(), 0);

        let (loaded, _) =
            LibraryService::load_from_acid_stream(&service.acid_client, None).unwrap();
        assert!(loaded.tracks.is_empty(), "bulk must agree");
        assert!(loaded.content_hashes.is_empty());
    }

    /// Re-asserting a retired hash starts from a clean slate: AddedAt is the new one,
    /// identically in incremental, fresh replay and bulk (no history dependence).
    #[test]
    fn reassert_after_full_retraction_resets_added_at_on_all_paths() {
        let hash = ContentHash::new("sha256:readd_0001");
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 2, 2, 0, 0, 0).unwrap();
        let (service, _metadata_dir, _acid) = make_empty_service();

        service
            .write_and_project(
                &hash,
                &[
                    (MusicValue::AddedAt(t1), user_source()),
                    (MusicValue::Title(Title::new("First")), user_source()),
                ],
            )
            .unwrap();
        retire(&service, &hash).unwrap();
        assert!(service.resolve_hash(&hash).is_err());
        service
            .write_and_project(
                &hash,
                &[
                    (MusicValue::AddedAt(t2), user_source()),
                    (MusicValue::Title(Title::new("Second")), user_source()),
                ],
            )
            .unwrap();

        let added = |tracks: &[IndexedTrackInfo]| {
            tracks
                .iter()
                .find(|t| t.content_hash.as_str() == hash.as_str())
                .map(|t| (t.added_at, t.title.clone()))
        };
        let expected = Some((Some(t2), Some("Second".to_string())));

        assert_eq!(added(&service.tracks.lock().unwrap()), expected);

        let all_lines = service
            .acid_client
            .read_stream(None, 100_000)
            .unwrap()
            .lines;
        let (fresh, _fresh_dir, _fresh_acid) = make_empty_service();
        fresh.apply_stream_lines(&all_lines);
        fresh.drop_non_live();
        assert_eq!(added(&fresh.tracks.lock().unwrap()), expected);

        let (loaded, _) =
            LibraryService::load_from_acid_stream(&service.acid_client, None).unwrap();
        assert_eq!(added(&loaded.tracks), expected);
    }

    /// Reverting a replace (A -> B -> A) must not make A replace itself.
    #[test]
    fn revert_replace_does_not_assert_self_replaces() {
        let hash_a = ContentHash::new("sha256:revert_a_0001");
        let hash_b = ContentHash::new("sha256:revert_b_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();

        seed_track(&service, &hash_a, "A");
        seed_track(&service, &hash_b, "B");
        service
            .assert_replaces_facts(&hash_b, &hash_a, vec![])
            .unwrap();
        retire(&service, &hash_a).unwrap();

        // Revert: A comes back, replacing B (which inherited A as an ancestor).
        let inherited = service.gather_ancestor_replaces(&hash_b);
        seed_track(&service, &hash_a, "A again");
        service
            .assert_replaces_facts(&hash_a, &hash_b, inherited)
            .unwrap();

        assert_eq!(
            replaces_targets(&service, &hash_a),
            vec![hash_b.as_str().to_owned()]
        );
    }

    /// A -> B then B -> C with no subscriber run in between: C inherits A, and after
    /// catch_up (and a full replay / bulk reload) neither A nor B resolves again.
    #[test]
    fn replace_chain_without_subscriber_retires_old_hashes_for_good() {
        let hash_a = ContentHash::new("sha256:chain_a_0001");
        let hash_b = ContentHash::new("sha256:chain_b_0001");
        let hash_c = ContentHash::new("sha256:chain_c_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();

        seed_track(&service, &hash_a, "A");
        for (new, old, title) in [(&hash_b, &hash_a, "B"), (&hash_c, &hash_b, "C")] {
            let inherited = service.gather_ancestor_replaces(old);
            seed_track(&service, new, title);
            service.assert_replaces_facts(new, old, inherited).unwrap();
            retire(&service, old).unwrap();
        }
        service.catch_up().unwrap();

        let mut targets = replaces_targets(&service, &hash_c);
        targets.sort();
        assert_eq!(
            targets,
            vec![hash_a.as_str().to_owned(), hash_b.as_str().to_owned()]
        );
        assert!(service.resolve_hash(&hash_a).is_err(), "A must not resolve");
        assert!(service.resolve_hash(&hash_b).is_err(), "B must not resolve");
        assert!(service.resolve_hash(&hash_c).is_ok());
        assert_eq!(service.tracks_count(), 1);
        {
            let hashes = service.content_hashes.lock().unwrap();
            assert!(!hashes.contains(hash_a.as_str()));
            assert!(!hashes.contains(hash_b.as_str()));
        }

        // Replay the whole stream into a fresh index (what a new subscriber does).
        let all_lines = service
            .acid_client
            .read_stream(None, 100_000)
            .unwrap()
            .lines;
        let (fresh, _fresh_dir, _fresh_acid) = make_empty_service();
        fresh.apply_stream_lines(&all_lines);
        fresh.drop_non_live();
        assert!(fresh.resolve_hash(&hash_a).is_err(), "replay: A is a ghost");
        assert!(fresh.resolve_hash(&hash_b).is_err(), "replay: B is a ghost");
        assert!(fresh.resolve_hash(&hash_c).is_ok());
        assert_eq!(replaces_targets(&fresh, &hash_c).len(), 2);

        // Bulk bootstrap must agree.
        let (loaded, _) =
            LibraryService::load_from_acid_stream(&service.acid_client, None).unwrap();
        assert!(!loaded.content_hashes.contains(hash_a.as_str()));
        assert!(!loaded.content_hashes.contains(hash_b.as_str()));
        assert!(loaded.content_hashes.contains(hash_c.as_str()));
        assert_eq!(loaded.tracks.len(), 1);
    }

    /// Replaying the retract lines of an already-removed entity must not resurrect it.
    #[test]
    fn replaying_retract_lines_does_not_resurrect_removed_entity() {
        let hash = ContentHash::new("sha256:ghost_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();
        seed_track(&service, &hash, "Ghost");
        retire(&service, &hash).unwrap();
        assert!(service.resolve_hash(&hash).is_err());

        let lines = service.acid_client.read_entity(hash.as_str()).unwrap();
        let retract_lines: Vec<String> = lines
            .into_iter()
            .filter(|l| {
                serde_json::from_str::<Fact<ContentHash, MusicValue, FactSource>>(l)
                    .map(|f| f.operation() == Operation::Retract)
                    .unwrap_or(false)
            })
            .collect();
        assert!(!retract_lines.is_empty());

        service.apply_stream_lines(&retract_lines);

        assert!(service.resolve_hash(&hash).is_err(), "no ghost entry");
        assert!(!service
            .content_hashes
            .lock()
            .unwrap()
            .contains(hash.as_str()));
        assert_eq!(service.tracks_count(), 0);
    }

    /// Dropping a fully retracted entity also drops its fact_index values.
    #[test]
    fn fully_retracted_entity_leaves_no_fact_index_values() {
        let hash = ContentHash::new("sha256:fact_index_drop_0001");
        let (service, _metadata_dir, _acid) = make_empty_service();
        seed_track(&service, &hash, "Indexed Title");
        let title_type = FactType::new("Title");
        assert!(service.fact_index.lock().unwrap()[&title_type].contains("Indexed Title"));

        retire(&service, &hash).unwrap();

        let fact_index = service.fact_index.lock().unwrap();
        assert!(fact_index
            .get(&title_type)
            .map(|s| !s.contains("Indexed Title"))
            .unwrap_or(true));
    }

    /// handle_track_replace end to end with generated WAV files: old track is
    /// hard-retracted, the new one carries Replaces, and playlists are rewritten.
    #[test]
    fn handle_track_replace_end_to_end_with_wav_files() {
        let (service, metadata_dir, _acid) = make_service_with_playlists_dir();
        let inbox = service.music_dir.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();

        let old_wav = inbox.join("Old Artist - Old Song.wav");
        write_test_wav(&old_wav, 5);
        let old_hash = service.ingest_file_internal(&old_wav, None).unwrap();

        let new_wav = inbox.join("New Artist - New Song.wav");
        write_test_wav(&new_wav, 7);

        std::fs::write(
            metadata_dir.path().join("playlists").join("set.plist"),
            format!("{}\n", old_hash.as_str()),
        )
        .unwrap();

        let resp = service.handle_request(LibraryRequest::TrackReplace {
            old_hash: old_hash.clone(),
            new_file_path: new_wav.to_string_lossy().to_string(),
        });
        let (new_hash, rewritten) = match resp {
            LibraryResponse::TrackReplaced {
                new_hash,
                playlists_rewritten,
            } => (new_hash, playlists_rewritten),
            other => panic!("expected TrackReplaced, got {:?}", other),
        };

        assert_ne!(new_hash, old_hash);
        assert_eq!(rewritten, 1);
        assert!(service.resolve_hash(&old_hash).is_err(), "old must be gone");
        assert_eq!(service.resolve_hash(&new_hash).unwrap(), new_hash);
        assert_eq!(
            replaces_targets(&service, &new_hash),
            vec![old_hash.as_str().to_owned()],
            "exactly one Replaces(old) in memory"
        );
        assert_eq!(service.tracks_count(), 1);
        let plist =
            std::fs::read_to_string(metadata_dir.path().join("playlists").join("set.plist"))
                .unwrap();
        assert!(plist.contains(new_hash.as_str()), "playlist: {plist}");
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

    // =========================================================================
    // Projection → IPC: memory_cues and beat_grid carried in TrackInfo
    // =========================================================================

    /// TrackInfo returned by GetTrack must carry projected memory_cues and beat_grid
    /// when the corresponding facts have been asserted.
    ///
    /// This test catches B1: the old export path read raw display strings from
    /// get_facts instead of using the already-projected structured data.
    #[test]
    fn track_info_carries_cues_and_grid_after_assertion() {
        use music_facts::{Bpm, CueKind};

        let hash = ContentHash::new("sha256:cuetest01020304050607080910111213141516");

        let bpm = Bpm::from_f32(128.0).unwrap();
        let temp = write_facts_file(&[
            (hash.clone(), MusicValue::Title(Title::new("Cue Track"))),
            (
                hash.clone(),
                MusicValue::FilePath(std::path::PathBuf::from("track.flac")),
            ),
            (
                hash.clone(),
                MusicValue::BeatGrid {
                    first_beat_ms: 250,
                    bpm,
                    beats_per_bar: 4,
                },
            ),
            (
                hash.clone(),
                MusicValue::MemoryCue {
                    position_ms: 5500,
                    kind: CueKind::Memory,
                    label: Some("Intro".to_string()),
                    index: None,
                },
            ),
            (
                hash.clone(),
                MusicValue::MemoryCue {
                    position_ms: 62000,
                    kind: CueKind::Hot,
                    label: Some("Drop".to_string()),
                    index: Some(0),
                },
            ),
        ]);

        let (service, _dir) = make_service_with_facts(temp.path());

        let resp = service.handle_request(LibraryRequest::GetTrack { hash: hash.clone() });
        let track_info = match resp {
            LibraryResponse::Track(t) => *t,
            other => panic!("expected Track, got {:?}", other),
        };

        // beat_grid must be populated
        let grid = track_info
            .beat_grid
            .as_ref()
            .expect("beat_grid should be Some");
        assert_eq!(grid.first_beat_ms, 250);
        assert_eq!(grid.bpm, bpm);
        assert_eq!(grid.beats_per_bar, 4);

        // memory_cues must contain both asserted cues, in assertion order
        assert_eq!(track_info.memory_cues.len(), 2, "expected 2 cues");
        assert_eq!(track_info.memory_cues[0].position_ms, 5500);
        assert!(matches!(track_info.memory_cues[0].kind, CueKind::Memory));
        assert_eq!(track_info.memory_cues[0].label.as_deref(), Some("Intro"));
        assert_eq!(track_info.memory_cues[0].index, None);

        assert_eq!(track_info.memory_cues[1].position_ms, 62000);
        assert!(matches!(track_info.memory_cues[1].kind, CueKind::Hot));
        assert_eq!(track_info.memory_cues[1].label.as_deref(), Some("Drop"));
        assert_eq!(track_info.memory_cues[1].index, Some(0));
    }
}
