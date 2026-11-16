//! Library projection: the pure fold from the fact log to the in-memory track index.
//!
//! This is the single implementation of the library's track projection (ADR-004,
//! ADR-011, ADR-021). The library service uses it online (bulk load, incremental
//! stream application); anything that needs the same view of a fact log offline can
//! build it here. No IPC, locks, filesystem or ACID client live in this component.
//!
//! ## Liveness
//! An entry exists only while it has live tracked provenance
//! ([`IndexedTrackInfo::is_live`]). A Retract that empties the provenance resets
//! the entry to pristine ([`apply_fact_to_track`]); the bulk fold ([`project`])
//! drops non-live entries, and so does [`retain_live`] after incremental application.
//!
//! ## Retirement
//! A merge asserts `Replaces(loser)` on the winner and then retracts the loser. A loser
//! that is live when the `Replaces` is asserted is marked retired
//! ([`IndexedTrackInfo::is_retired`]) until it empties, which is when the merge is
//! finished, or the `Replaces` is retracted. A loser that comes back later (a re-ingest
//! after the merge) was not live at the `Replaces`, so it is never marked. The rule
//! reads the log in stream order, never timestamps, so a clock that is wrong cannot
//! change it. Library writers skip retired tracks, and `mdma library dedup` finishes
//! their merges.
//!
//! ## Plays
//! Play events (start/stop) are history with identity, not tracked provenance. The
//! bulk fold leaves `last_started`/`last_stopped` unset; [`PlayLog`] folds them
//! separately, keeps every event seen (retired entities included) and credits a track
//! with the plays of every hash it transitively replaces (ADR-013, ADR-017). It is the
//! one fold of plays: the last times ([`PlayLog::apply_to`]) and the whole history
//! ([`PlayHistory`]) read the same resolved events. [`resolve_prefix`] is the one
//! resolution of a partial hash.

use library_ipc_protocol::FactType;
use music_facts::{ContentHash, Mark, MarkFilter, MusicValue, StartReason, StopReason};
use stainless_facts::{aggregate_facts, Fact, Operation};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

/// A fact of the music domain, as stored in the fact log.
pub type MusicFact = Fact<ContentHash, MusicValue, music_facts::FactSource>;

/// A single memory cue, hot cue, or loop point stored in the in-memory index.
///
/// Stores `kind` as the typed `CueKind` (not a display string) for query-consistency
/// and correct equality checks during retraction.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedCue {
    pub position_ms: u32,
    pub kind: music_facts::CueKind,
    pub label: Option<String>,
    pub index: Option<u8>,
}

/// Track info stored in memory for search
#[derive(Clone)]
pub struct IndexedTrackInfo {
    pub content_hash: ContentHash,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub label: Option<String>,
    pub genre: Option<String>,
    pub styles: Vec<String>,
    pub duration_seconds: Option<u32>,
    pub bpm: Option<f32>,
    pub key: Option<String>,
    pub year: Option<u32>,
    pub source: Option<String>,
    pub blob_path: PathBuf,
    pub cover_art_path: Option<PathBuf>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub last_started: Option<chrono::DateTime<chrono::Utc>>,
    pub last_stopped: Option<chrono::DateTime<chrono::Utc>>,
    pub added_at: Option<chrono::DateTime<chrono::Utc>>,
    pub item_id: Option<String>,
    /// Set when a Deleted fact has been asserted (track is hidden).
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// DJ curation role (Opener, BuildUp, Peak, etc.) — typed for query-consistency.
    pub role: Option<music_facts::TrackRole>,
    /// Energy level 1–10
    pub energy: Option<music_facts::EnergyLevel>,
    /// Beat-grid anchor as (first_beat_ms, bpm_f32, beats_per_bar)
    pub beat_grid: Option<(u32, f32, u8)>,
    /// Memory/hot cue and loop points stored as typed structs.
    pub memory_cues: Vec<IndexedCue>,
    /// Per-source provenance: ordered list of (value, source) assertions still live.
    ///
    /// Used to correctly handle retractions: Retract(value=V, source=S) removes every
    /// pair with an equal value and the same source as S (`FactSource::same_source`),
    /// then the scalar field is re-resolved from whatever remains.
    /// For multi-valued fields (StyleDescriptor, MemoryCue) each assertion contributes a separate entry.
    pub provenance: Vec<(MusicValue, music_facts::FactSource)>,
    /// Raw FilePath value stored during fact application.
    ///
    /// FilePath is excluded from provenance (so it is latest-wins, and a retract of the
    /// current value clears it) but the blob_path derivation needs the extension.  When content_hash is empty (aggregate_facts
    /// bulk path) the derivation is deferred; this field holds the path until the
    /// post-pass in [`project`] calls `derive_blob_path_if_needed`.
    pub raw_file_path: Option<PathBuf>,
    /// The entity key of the track whose `Replaces` fact retired this one while it was
    /// live: a merge began for it and has not retracted everything yet (an unfinished
    /// merge, or one a crash interrupted). Set when `Replaces(this)` is asserted on
    /// another entity while this entry is live; cleared when the entry empties (the
    /// liveness reset, so a re-ingested track starts clean) or when that `Replaces` is
    /// retracted (module docs, "Retirement").
    pub retired_by: Option<String>,
}

impl IndexedTrackInfo {
    /// Whether a `Replaces` fact has retired this track while it was live (see
    /// [`IndexedTrackInfo::retired_by`]). The one predicate library writers ask before
    /// adding a fact to a track.
    pub fn is_retired(&self) -> bool {
        self.retired_by.is_some()
    }

    /// An entry is live iff it holds at least one tracked assertion. Non-live
    /// entries (fully retracted, or orphan play events for unknown hashes) are
    /// dropped by every projection path so such a hash stops resolving (ADR-004).
    pub fn is_live(&self) -> bool {
        !self.provenance.is_empty()
    }

    /// Whether an assertion of `value` that a retract from `source` would cancel is
    /// live: an equal value from the same source (the retract rule, ADR-018).
    pub fn is_live_from(&self, value: &MusicValue, source: &music_facts::FactSource) -> bool {
        self.provenance
            .iter()
            .any(|(v, s)| retract_matches(v, s, value, source))
    }

    pub fn new_empty(entity: String) -> Self {
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
            retired_by: None,
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
pub fn blob_path_for(content_hash: &str, file_path: &std::path::Path) -> Option<PathBuf> {
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
    pub fn item_ids(&self) -> impl Iterator<Item = &str> {
        self.provenance.iter().filter_map(|(v, _)| match v {
            MusicValue::ItemId(id) => Some(id.as_str()),
            _ => None,
        })
    }

    /// Every AudioHash this track holds, as the hash string (version prefix included).
    pub fn audio_hashes(&self) -> impl Iterator<Item = &str> {
        self.provenance.iter().filter_map(|(v, _)| match v {
            MusicValue::AudioHash(h) => Some(h.as_str()),
            _ => None,
        })
    }

    /// Returns true if this track is hidden from default views.
    pub fn is_hidden(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Returns true if this track holds a live mark admitted by `filter`. Reads
    /// provenance, so a retracted mark no longer counts.
    pub fn has_mark(&self, filter: &MarkFilter) -> bool {
        match filter {
            MarkFilter::Only(mark) => self.live_marks(mark).next().is_some(),
            MarkFilter::Any => self
                .provenance
                .iter()
                .filter_map(|(v, _)| v.stored_mark())
                .any(|m| filter.admits(&m)),
        }
    }

    /// The live assertions of `mark` on this track, with their sources. The one
    /// predicate for "carries this mark live", shared by `write_mark` and `unmark`.
    pub fn live_marks<'a>(
        &'a self,
        mark: &'a Mark,
    ) -> impl Iterator<Item = &'a (MusicValue, music_facts::FactSource)> {
        self.provenance.iter().filter(move |(v, _)| mark.matches(v))
    }

    /// The keys ([`hash_key`]) of the tracks a live `Replaces` fact of this track
    /// names: the one place that reads the lineage out of the fold.
    pub fn replaced_keys(&self) -> impl Iterator<Item = String> + '_ {
        self.provenance.iter().filter_map(|(v, _)| match v {
            MusicValue::Replaces(h) => Some(hash_key(h.as_str())),
            _ => None,
        })
    }

    /// Derive `blob_path` from `raw_file_path` and `content_hash` if blob_path
    /// is still empty.
    ///
    /// Called in the post-pass of [`project`] after `content_hash` has been set
    /// from the entity key.  The aggregate_facts bulk path initialises entries
    /// with an empty content_hash (via Default) so the derivation inside
    /// `apply_assert_scalar` is skipped; this method completes it once the real
    /// hash is available.
    pub fn derive_blob_path_if_needed(&mut self) {
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
/// Removes every `(v, s)` pair from `entry.provenance` where `v == value` and `s` is
/// the same source as `source` (`FactSource::same_source`: tool and origin kind, not
/// version or payload), then re-resolves all scalar fields from the surviving
/// provenance. A retract of a value that is not live, or that only another source
/// asserted, has no effect (#96, ADR-018).
///
/// ## Liveness
/// An entry whose provenance is empty after a retraction is reset to a pristine
/// `new_empty` entry, clearing history-dependent state (AddedAt, FilePath,
/// blob_path, timestamps). Liveness is then purely `!provenance.is_empty()`
/// (`IndexedTrackInfo::is_live`), independent of how the facts were folded, so
/// bulk and incremental projections agree.
pub fn apply_fact_to_track(
    entry: &mut IndexedTrackInfo,
    value: &MusicValue,
    timestamp: chrono::DateTime<chrono::Utc>,
    operation: Operation,
    source: &music_facts::FactSource,
) {
    match operation {
        Operation::Assert => {
            // Store provenance for value-and-source-identity retraction (ADR-018)
            // (skip timestamp-only fields)
            if is_provenance_tracked(value) {
                entry.provenance.push((value.clone(), source.clone()));
            }
            apply_assert_scalar(entry, value, timestamp);
        }
        Operation::Retract => {
            // Remove every assertion with an equal value and the same source.
            if is_provenance_tracked(value)
                && !remove_cancelled(&mut entry.provenance, value, source, |(v, s)| (v, s))
            {
                // No matching assertion in provenance — retraction has no effect.
                // This handles the #96 case: Retract(Bpm=120) when only Bpm=128
                // is asserted finds no match and leaves the field untouched.
                return;
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

/// The one retract-matching rule (ADR-018): remove from `live` every entry whose value
/// equals `value` and whose source is the same source as `source`
/// (`FactSource::same_source`). Returns whether anything was removed. `of` reads the
/// value and source out of an entry, so the in-memory provenance and the ACID replay
/// (`dedup::live_facts`) share it.
pub fn remove_cancelled<T>(
    live: &mut Vec<T>,
    value: &MusicValue,
    source: &music_facts::FactSource,
    of: impl Fn(&T) -> (&MusicValue, &music_facts::FactSource),
) -> bool {
    let before = live.len();
    live.retain(|e| {
        let (v, s) = of(e);
        !retract_matches(v, s, value, source)
    });
    live.len() != before
}

/// Whether a retract of `value` from `source` cancels the assertion `(v, s)`: an equal
/// value from the same source (`FactSource::same_source`). The one predicate behind
/// `remove_cancelled`, `distinct_retractions` and `IndexedTrackInfo::is_live_from`.
pub fn retract_matches(
    v: &MusicValue,
    s: &music_facts::FactSource,
    value: &MusicValue,
    source: &music_facts::FactSource,
) -> bool {
    v == value && s.same_source(source)
}

/// One (value, source) per retract to write: assertions with an equal value and the
/// same source collapse into the first, since a single retract cancels them all.
/// The retract line carries the first assertion's source (version and payload);
/// that is fine because matching ignores both (`FactSource::same_source`).
pub fn distinct_retractions(
    live: impl IntoIterator<Item = (MusicValue, music_facts::FactSource)>,
) -> Vec<(MusicValue, music_facts::FactSource)> {
    let mut out: Vec<(MusicValue, music_facts::FactSource)> = Vec::new();
    for (value, source) in live {
        if !out
            .iter()
            .any(|(v, s)| retract_matches(v, s, &value, &source))
        {
            out.push((value, source));
        }
    }
    out
}

/// Whether `value` is a play event: a start or a stop of playback. Play events are
/// history with identity, so a merge leaves the loser's in the log (ADR-017 §3.9).
/// The one place to extend when the play tags change.
pub fn is_play_event(value: &MusicValue) -> bool {
    matches!(
        value,
        MusicValue::TrackStarted(_) | MusicValue::TrackStopped(_)
    )
}

/// Returns true for `MusicValue` variants whose (value, source) pairs are tracked
/// in `IndexedTrackInfo::provenance` for correct retraction semantics.
///
/// Timestamp-only values (TrackStarted, TrackStopped) use a separate most-recent-wins
/// strategy and are intentionally excluded — retractions for those are not emitted
/// in production and the timestamp logic is handled elsewhere.
///
/// `AddedAt` is excluded too: it has no provenance, so a retract of it (a merge
/// writes one) changes nothing in the projection. Projection is latest-wins
/// (`apply_assert_scalar`). `FilePath` is untracked as well, but a retract of the
/// current value clears it (`apply_retract_scalar`).
pub fn is_provenance_tracked(value: &MusicValue) -> bool {
    !(is_play_event(value) || matches!(value, MusicValue::FilePath(_) | MusicValue::AddedAt(_)))
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
        MusicValue::StyleDescriptor(v) => push_unique(&mut entry.styles, v.clone()),
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
            // bulk path) the helper returns None and the post-pass in `project`
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
            push_unique(
                &mut entry.memory_cues,
                IndexedCue {
                    position_ms: *position_ms,
                    kind: kind.clone(),
                    label: label.clone(),
                    index: *index,
                },
            );
        }
        _ => {}
    }
}

/// `styles` and `memory_cues` behave as sets: the same value asserted by two sources
/// (or twice) is held once, in first-assertion order.
fn push_unique<T: PartialEq>(held: &mut Vec<T>, item: T) {
    if !held.contains(&item) {
        held.push(item);
    }
}

/// The distinct items, first occurrence kept: what a retract rebuilds a set from.
fn unique<T: PartialEq>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out = Vec::new();
    for item in items {
        push_unique(&mut out, item);
    }
    out
}

/// Re-resolve a single scalar field after removing a provenance entry.
///
/// For fields that map to a scalar `Option<T>`, we scan the remaining provenance
/// for the last (most recent) assertion of the same variant and re-set the scalar.
/// For multi-valued fields (`StyleDescriptor`) we rebuild the Vec from scratch.
///
/// Fields not tracked in provenance are left unchanged, except `FilePath`, which a
/// retract of the current value clears: `TrackStarted`/`TrackStopped` are history and
/// `AddedAt` is latest-wins.
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
            // Rebuild the styles set from surviving provenance
            entry.styles = unique(entry.provenance.iter().filter_map(|(v, _)| {
                if let MusicValue::StyleDescriptor(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            }));
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
            entry.memory_cues = unique(entry.provenance.iter().filter_map(|(v, _)| {
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
            }));
        }
        // A retract of the current path clears it (a merge retracts the loser's), so a
        // track that outlives its retract holds no file. A retract of another path
        // changes nothing. The retract's source is ignored: FilePath has no provenance
        // to match it against, an exception to the source rule of ADR-018.
        MusicValue::FilePath(p) if entry.raw_file_path.as_ref() == Some(p) => {
            entry.raw_file_path = None;
            entry.blob_path = PathBuf::new();
        }
        // AddedAt stays latest-wins: a retract leaves it (it has no provenance to fall
        // back to), and the entry resets when nothing else is live. Play events are
        // history, not state: their retracts are ignored.
        _ => {}
    }
}

/// Implement `FactAggregator` so `stainless_facts::aggregate_facts` can drive
/// the bulk fold ([`project`]).
impl stainless_facts::FactAggregator<ContentHash, MusicValue, music_facts::FactSource>
    for IndexedTrackInfo
{
    fn assert(&mut self, value: &MusicValue, source: &music_facts::FactSource) {
        // TrackStarted/TrackStopped carry their real timestamp on the Fact struct,
        // but the FactAggregator trait only receives the value and source — the
        // timestamp is not available here. We must not populate last_started /
        // last_stopped with a bootstrap wall-clock placeholder, because
        // `PlayLog` is the sole authoritative path for those fields in
        // the bulk fold. Skip them here entirely.
        if is_play_event(value) {
            return;
        }
        apply_fact_to_track(
            self,
            value,
            chrono::Utc::now(), // timestamp unused for all non-timestamp fields
            Operation::Assert,
            source,
        );
    }

    fn retract(&mut self, value: &MusicValue, source: &music_facts::FactSource) {
        // TrackStarted/TrackStopped retractions are not emitted in production;
        // and `PlayLog` handles these fields authoritatively.
        // Skip to keep parity with assert.
        if is_play_event(value) {
            return;
        }
        apply_fact_to_track(self, value, chrono::Utc::now(), Operation::Retract, source);
    }
}

/// Set `slot` to `ts` unless it already holds a more recent timestamp.
pub fn update_if_more_recent(
    slot: &mut Option<chrono::DateTime<chrono::Utc>>,
    ts: chrono::DateTime<chrono::Utc>,
) {
    if slot.is_none_or(|existing| ts > existing) {
        *slot = Some(ts);
    }
}

/// Normalised form of a content hash for comparisons: no `sha256:` prefix, lowercase.
pub fn hash_key(h: &str) -> String {
    h.strip_prefix("sha256:").unwrap_or(h).to_lowercase()
}

/// One line of a fact stream, or the reason it is not a fact. A line this library
/// cannot read (written by a newer one, say) is a fact it does not see; callers decide
/// how loudly to say so.
pub fn try_parse_fact_line(line: &str) -> Result<MusicFact, serde_json::Error> {
    serde_json::from_str(line)
}

/// One line of a fact stream, or `None` for a line that is not a fact.
pub fn parse_fact_line(line: &str) -> Option<MusicFact> {
    try_parse_fact_line(line).ok()
}

/// The result of the bulk fold over a fact log.
pub struct Projection {
    /// Live tracks only (see [`IndexedTrackInfo::is_live`]).
    pub tracks: Vec<IndexedTrackInfo>,
    /// fact_type -> set of values currently asserted (provenance-tracked values only).
    pub fact_index: HashMap<FactType, HashSet<String>>,
    /// Content hashes of the live tracks.
    pub content_hashes: HashSet<String>,
    /// Content hashes of tracks that already have a Format fact
    pub has_format: HashSet<String>,
    /// Content hashes of tracks that already have a CoverArtPath fact
    pub has_cover_art: HashSet<String>,
    /// Lines that were not facts and were skipped ([`project_lines`]); 0 for [`project`].
    pub unparseable_lines: usize,
    /// Every play event of the log. Tracks carry no play times (the bulk fold leaves
    /// `last_started`/`last_stopped` unset); read them from here.
    pub plays: PlayLog,
}

/// Bulk-fold facts into a [`Projection`].
///
/// Single fold via `aggregate_facts`, then non-live entities are dropped (exactly
/// as the incremental projection's sweep drops them, so a hard-retracted hash stops
/// resolving, ADR-004), then `content_hash` is fixed up from the entity key, the
/// blob path derived and the side-effect indexes built.
///
/// `last_started` / `last_stopped` are left unset; the play events are in
/// [`Projection::plays`], and [`PlayLog::apply_to`] fills those in.
pub fn project(facts: Vec<MusicFact>) -> Projection {
    let plays = PlayLog::from_facts(&facts);
    let marks = retirement_marks(&facts);
    let mut aggregated: HashMap<ContentHash, IndexedTrackInfo> = aggregate_facts(facts);

    aggregated.retain(|_, entry| entry.is_live());
    if !marks.is_empty() {
        for (entity, entry) in aggregated.iter_mut() {
            entry.retired_by = marks.get(&hash_key(entity.as_str())).cloned();
        }
    }

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
    Projection {
        tracks: aggregated.into_values().collect(),
        fact_index,
        content_hashes,
        has_format,
        has_cover_art,
        unparseable_lines: 0,
        plays,
    }
}

/// Like [`project`], over raw JSONL fact lines. Lines that are not facts are skipped
/// and counted in [`Projection::unparseable_lines`].
pub fn project_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Projection {
    let mut unparseable_lines = 0;
    let facts = lines
        .into_iter()
        .filter_map(|line| {
            let fact = parse_fact_line(line);
            if fact.is_none() {
                unparseable_lines += 1;
            }
            fact
        })
        .collect();
    Projection {
        unparseable_lines,
        ..project(facts)
    }
}

/// Apply one fact incrementally to a projection held as its parts (the service holds
/// each under its own lock). `positions` maps an entity key to its index in `tracks`
/// and must be consistent with it.
///
/// A Retract never creates an entry: retracting an unknown (or already dropped)
/// entity is a no-op, otherwise replaying the retract lines of a removed entity
/// would resurrect it as an empty ghost (ADR-004). Entries are never removed here, so
/// positions stay valid; run [`retain_live`] once after all facts of a catch-up.
///
/// `fact_index` mirrors provenance (like the bulk fold): untracked values (FilePath,
/// AddedAt, play events) are never indexed.
pub fn apply_fact(
    tracks: &mut Vec<IndexedTrackInfo>,
    positions: &mut HashMap<String, usize>,
    content_hashes: &mut HashSet<String>,
    fact_index: &mut HashMap<FactType, HashSet<String>>,
    fact: &MusicFact,
) {
    let entity = fact.entity().as_str();
    let is_retract = fact.operation() == Operation::Retract;

    let pos = if let Some(&pos) = positions.get(entity) {
        pos
    } else if is_retract {
        return;
    } else {
        content_hashes.insert(entity.to_owned());
        let pos = tracks.len();
        tracks.push(IndexedTrackInfo::new_empty(entity.to_owned()));
        positions.insert(entity.to_owned(), pos);
        pos
    };

    let fact_type = FactType::new(fact.value().display_name());
    let value_str = fact.value().to_string();

    // Apply to track fields first (updates provenance)
    apply_fact_to_track(
        &mut tracks[pos],
        fact.value(),
        *fact.timestamp(),
        fact.operation(),
        fact.source(),
    );
    note_retirement(tracks, positions, pos, fact);

    if !is_provenance_tracked(fact.value()) {
        return;
    }
    match fact.operation() {
        Operation::Assert => {
            fact_index.entry(fact_type).or_default().insert(value_str);
        }
        Operation::Retract => {
            // Remove from fact_index only when no entity still asserts this
            // value (the current entry has already been updated above).
            let still_asserted = tracks
                .iter()
                .any(|t| t.provenance.iter().any(|(v, _)| v.to_string() == value_str));
            if !still_asserted {
                if let Some(set) = fact_index.get_mut(&fact_type) {
                    set.remove(&value_str);
                    if set.is_empty() {
                        fact_index.remove(&fact_type);
                    }
                }
            }
        }
    }
}

/// The retirement rule, incremental half (see [`retirement_marks`] for the bulk half):
/// a `Replaces(L)` asserted on the entry at `pos` retires `L` if `L` is live right now;
/// retracting it (so the entry no longer holds any `Replaces(L)`) lifts that. The entry
/// emptying clears the mark by itself, because the liveness reset rebuilds the entry.
///
/// A release hands the mark to another track that still holds a `Replaces` of the same
/// loser (the smallest entity key, so the bulk pass agrees) and clears it only when
/// there is none.
fn note_retirement(
    tracks: &mut [IndexedTrackInfo],
    positions: &HashMap<String, usize>,
    pos: usize,
    fact: &MusicFact,
) {
    let MusicValue::Replaces(replaced) = fact.value() else {
        return;
    };
    // Entity keys are written as `sha256:<hex>`; the value is read by its hash key.
    let key = hash_key(replaced.as_str());
    let Some(&lp) = positions
        .get(replaced.as_str())
        .or_else(|| positions.get(&format!("sha256:{key}")))
        .or_else(|| positions.get(&key))
    else {
        return;
    };
    if lp == pos {
        return;
    }
    let winner = fact.entity().as_str();
    match fact.operation() {
        Operation::Assert => {
            if tracks[lp].is_live() {
                tracks[lp].retired_by = Some(winner.to_owned());
            }
        }
        Operation::Retract => {
            if tracks[lp].retired_by.as_deref() != Some(winner) {
                return;
            }
            let other_holder = tracks
                .iter()
                .enumerate()
                .filter(|(i, t)| *i != lp && t.replaced_keys().any(|k| k == key))
                .map(|(_, t)| t.content_hash.as_str())
                .min()
                .map(str::to_owned);
            tracks[lp].retired_by = other_holder;
        }
    }
}

/// The retirement rule, bulk half: `loser hash key -> winner entity` for every entity
/// that a `Replaces` retired while live and that has not emptied or been released
/// since, read in stream order. `aggregate_facts` folds each entity alone, so the
/// cross-entity question "was the loser live when the `Replaces` came?" needs this
/// sequential pass (like [`PlayLog`]). It shadows the tracked provenance of the
/// entities a `Replaces` names or comes from, and nothing else. Everything is keyed by
/// [`hash_key`], like the incremental half.
fn retirement_marks(facts: &[MusicFact]) -> HashMap<String, String> {
    let mut involved: HashSet<String> = HashSet::new();
    for f in facts {
        if let MusicValue::Replaces(replaced) = f.value() {
            involved.insert(hash_key(f.entity().as_str()));
            involved.insert(hash_key(replaced.as_str()));
        }
    }
    let mut marks: HashMap<String, String> = HashMap::new();
    if involved.is_empty() {
        return marks;
    }

    type Live = Vec<(MusicValue, music_facts::FactSource)>;
    let holds = |live: &Live, key: &str| {
        live.iter()
            .any(|(v, _)| matches!(v, MusicValue::Replaces(h) if hash_key(h.as_str()) == key))
    };
    // hash key -> (entity key, tracked provenance)
    let mut shadow: HashMap<String, (&str, Live)> = HashMap::new();
    for f in facts {
        let entity = f.entity().as_str();
        let entity_key = hash_key(entity);
        if involved.contains(&entity_key) && is_provenance_tracked(f.value()) {
            let (_, live) = shadow
                .entry(entity_key.clone())
                .or_insert((entity, Vec::new()));
            match f.operation() {
                Operation::Assert => live.push((f.value().clone(), f.source().clone())),
                Operation::Retract => {
                    let removed = remove_cancelled(live, f.value(), f.source(), |(v, s)| (v, s));
                    if removed && live.is_empty() {
                        marks.remove(&entity_key);
                    }
                }
            }
        }
        let MusicValue::Replaces(replaced) = f.value() else {
            continue;
        };
        let loser = hash_key(replaced.as_str());
        if loser == entity_key {
            continue;
        }
        match f.operation() {
            Operation::Assert => {
                if shadow.get(&loser).is_some_and(|(_, live)| !live.is_empty()) {
                    marks.insert(loser, entity.to_owned());
                }
            }
            Operation::Retract => {
                if marks.get(&loser).is_none_or(|w| w != entity) {
                    continue;
                }
                let other_holder = shadow
                    .iter()
                    .filter(|(k, (_, live))| **k != loser && holds(live, &loser))
                    .map(|(_, (e, _))| *e)
                    .min();
                match other_holder {
                    Some(holder) => marks.insert(loser, holder.to_owned()),
                    None => marks.remove(&loser),
                };
            }
        }
    }
    marks
}

/// Drop every non-live entry (no tracked provenance): fully retracted entities and
/// orphan play events for unknown or retired hashes. Mirrors the bulk fold's
/// `retain(is_live)`.
///
/// Run once after *all* pages of a catch-up, never per line or per page, so an ingest
/// batch split across a page boundary is not dropped half-way.
pub fn retain_live(tracks: &mut Vec<IndexedTrackInfo>, content_hashes: &mut HashSet<String>) {
    tracks.retain(|t| {
        let live = t.is_live();
        if !live {
            content_hashes.remove(t.content_hash.as_str());
        }
        live
    });
}

/// One play event, as the fact log recorded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayEvent {
    Started {
        at: chrono::DateTime<chrono::Utc>,
        reason: StartReason,
    },
    Stopped {
        at: chrono::DateTime<chrono::Utc>,
        reason: StopReason,
    },
}

impl PlayEvent {
    /// The order events are read in: by time, and at one instant a start before a
    /// stop, so a stop and the start it closes never swap.
    fn order(&self) -> (chrono::DateTime<chrono::Utc>, u8) {
        match self {
            PlayEvent::Started { at, .. } => (*at, 0),
            PlayEvent::Stopped { at, .. } => (*at, 1),
        }
    }
}

/// A stop of playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    pub at: chrono::DateTime<chrono::Utc>,
    pub reason: StopReason,
}

/// One play of a track: a start and the stop that closed it, or a stop that no start
/// preceded (the legacy `Played`/`Skipped` facts are stop-only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Play {
    /// `stop` is `None` while the play is open, and for a play another start cut off.
    Started {
        at: chrono::DateTime<chrono::Utc>,
        reason: StartReason,
        stop: Option<Stop>,
    },
    StopOnly(Stop),
}

impl Play {
    /// When the play happened: its start, or for a stop-only play its stop.
    pub fn at(&self) -> chrono::DateTime<chrono::Utc> {
        match self {
            Play::Started { at, .. } => *at,
            Play::StopOnly(stop) => stop.at,
        }
    }
}

/// The plays of one track, oldest first. Pairing rule: a start opens a play and the
/// next stop closes it; a start while a play is open leaves the open one without a
/// stop; a stop with nothing open is a stop-only play.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayHistory {
    pub plays: Vec<Play>,
}

impl PlayHistory {
    /// Pair `events`, which must already be in [`PlayEvent::order`].
    fn from_events(events: impl IntoIterator<Item = PlayEvent>) -> Self {
        let mut plays = Vec::new();
        // The start of the play that is open, if any.
        let mut open: Option<(chrono::DateTime<chrono::Utc>, StartReason)> = None;
        for event in events {
            match event {
                PlayEvent::Started { at, reason } => {
                    if let Some((at, reason)) = open.replace((at, reason)) {
                        plays.push(Play::Started {
                            at,
                            reason,
                            stop: None,
                        });
                    }
                }
                PlayEvent::Stopped { at, reason } => {
                    let stop = Stop { at, reason };
                    plays.push(match open.take() {
                        Some((at, reason)) => Play::Started {
                            at,
                            reason,
                            stop: Some(stop),
                        },
                        None => Play::StopOnly(stop),
                    });
                }
            }
        }
        if let Some((at, reason)) = open {
            plays.push(Play::Started {
                at,
                reason,
                stop: None,
            });
        }
        Self { plays }
    }

    /// How many times the track was played.
    pub fn count(&self) -> usize {
        self.plays.len()
    }

    /// When the latest play happened ([`Play::at`]).
    pub fn last_played(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.plays.last().map(Play::at)
    }

    /// The latest start.
    pub fn last_started(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.plays
            .iter()
            .filter_map(|play| match play {
                Play::Started { at, .. } => Some(*at),
                Play::StopOnly(_) => None,
            })
            .max()
    }

    /// The latest stop.
    pub fn last_stopped(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.plays
            .iter()
            .filter_map(|play| match play {
                Play::Started { stop, .. } => stop.map(|s| s.at),
                Play::StopOnly(stop) => Some(stop.at),
            })
            .max()
    }
}

/// The play events seen in a fact log, per normalised entity key ([`hash_key`]).
///
/// Only assertions are plays. Retracts of plays (the legacy merge retracts, ADR-017
/// §6.7) are not plays and cancel nothing. Every event seen is remembered, whether or
/// not its entity is still live, so a merge or replace keeps its predecessors' history
/// (ADR-013, ADR-017): a track is credited with the plays of every hash it
/// transitively replaces, with their original timestamps. Events keyed by the short
/// hashes playback wrote (#86) resolve to the full hash. Playback wrote them until
/// ADR-026 (the CLI sent prefixes and playback recorded them verbatim), so the log
/// holds them from before that change, not only from some older playback. The lineage is
/// re-resolved on every read, so a `Replaces` asserted after the events were recorded
/// takes effect too.
///
/// This is the one fold of plays: [`apply_to`](Self::apply_to) (last started/stopped)
/// and [`history`](Self::history) (every play) read the same resolved events.
#[derive(Clone, Debug, Default)]
pub struct PlayLog {
    events: HashMap<String, Vec<PlayEvent>>,
}

impl PlayLog {
    /// Record `fact` if it is a play assertion. Only play events create an entry for
    /// their entity key; other facts are ignored.
    pub fn record(&mut self, fact: &MusicFact) {
        if fact.operation() != Operation::Assert {
            return;
        }
        let at = *fact.timestamp();
        let event = match fact.value() {
            MusicValue::TrackStarted(reason) => PlayEvent::Started {
                at,
                reason: *reason,
            },
            MusicValue::TrackStopped(reason) => PlayEvent::Stopped {
                at,
                reason: *reason,
            },
            _ => return,
        };
        self.events
            .entry(hash_key(fact.entity().as_str()))
            .or_default()
            .push(event);
    }

    /// The events of `facts`.
    pub fn from_facts<'a>(facts: impl IntoIterator<Item = &'a MusicFact>) -> Self {
        let mut log = Self::default();
        for fact in facts {
            log.record(fact);
        }
        log
    }

    /// Append the events of `later`, which were recorded after this log's.
    pub fn merge(&mut self, later: PlayLog) {
        for (key, mut events) in later.events {
            self.events.entry(key).or_default().append(&mut events);
        }
    }

    /// Set every track's `last_started` / `last_stopped` to the latest over its
    /// lineage: itself and every hash it transitively replaces. A time already set
    /// stays unless a later one is found.
    pub fn apply_to(&self, tracks: &mut [IndexedTrackInfo]) {
        let resolved = self.resolved(tracks);
        for track in tracks.iter_mut() {
            for event in resolved.events_of(track) {
                match event {
                    PlayEvent::Started { at, .. } => {
                        update_if_more_recent(&mut track.last_started, at)
                    }
                    PlayEvent::Stopped { at, .. } => {
                        update_if_more_recent(&mut track.last_stopped, at)
                    }
                }
            }
        }
    }

    /// The play history of every track, aligned with `tracks`.
    pub fn histories(&self, tracks: &[IndexedTrackInfo]) -> Vec<PlayHistory> {
        let resolved = self.resolved(tracks);
        tracks.iter().map(|t| resolved.history(t)).collect()
    }

    /// The `n` latest plays over all tracks, newest first, each with the index of the
    /// track in `tracks` that holds it. A play two live tracks both inherit (an
    /// unfinished merge) is listed for each.
    pub fn recent(&self, tracks: &[IndexedTrackInfo], n: usize) -> Vec<(usize, Play)> {
        let mut plays: Vec<(usize, Play)> = self
            .histories(tracks)
            .into_iter()
            .enumerate()
            .flat_map(|(i, history)| history.plays.into_iter().map(move |play| (i, play)))
            .collect();
        plays.sort_by(|(i, a), (j, b)| b.at().cmp(&a.at()).then(i.cmp(j)));
        plays.truncate(n);
        plays
    }

    /// The lineage and short-key resolution for `tracks`, the one place either is
    /// decided. It costs a pass over `tracks`, so resolve once and ask it for each
    /// track ([`ResolvedPlays::history`]) rather than once per track.
    pub fn resolved(&self, tracks: &[IndexedTrackInfo]) -> ResolvedPlays<'_> {
        let replaces: HashMap<String, Vec<String>> = tracks
            .iter()
            .map(|t| {
                let replaced = t.replaced_keys().collect();
                (hash_key(t.content_hash.as_str()), replaced)
            })
            .collect();
        // A short key is resolved only when exactly one full key (live track, replaced
        // hash or full-length event key) starts with it.
        let prefix_counts = short_prefix_counts(
            replaces
                .keys()
                .chain(replaces.values().flatten())
                .chain(self.events.keys())
                .map(String::as_str)
                .collect::<HashSet<_>>()
                .into_iter(),
        );
        ResolvedPlays {
            log: self,
            replaces,
            prefix_counts,
        }
    }
}

/// A [`PlayLog`] with the lineage and the short keys of a set of tracks resolved
/// ([`PlayLog::resolved`]).
pub struct ResolvedPlays<'a> {
    log: &'a PlayLog,
    /// A live track's key -> the keys it replaces.
    replaces: HashMap<String, Vec<String>>,
    /// For every 8- and 12-character prefix, how many full keys start with it.
    prefix_counts: HashMap<String, usize>,
}

impl ResolvedPlays<'_> {
    /// The plays of `track`, over its lineage. `track` should be one of the tracks
    /// this was resolved for.
    pub fn history(&self, track: &IndexedTrackInfo) -> PlayHistory {
        PlayHistory::from_events(self.events_of(track))
    }

    /// The events that belong to `track`, in [`PlayEvent::order`]: those recorded
    /// under every key of its lineage. Each event key is read once, however many
    /// lineage members reach it.
    fn events_of(&self, track: &IndexedTrackInfo) -> Vec<PlayEvent> {
        let lineage = replaces_lineage(&hash_key(track.content_hash.as_str()), &self.replaces);
        let keys: BTreeSet<&str> = lineage
            .iter()
            .flat_map(|key| self.event_keys(key))
            .collect();
        let mut events: Vec<PlayEvent> = keys
            .into_iter()
            .filter_map(|key| self.log.events.get(key))
            .flatten()
            .copied()
            .collect();
        events.sort_by_key(PlayEvent::order);
        events
    }

    /// The keys events for `key` are recorded under: the full hash, and the 12- and
    /// 8-character short forms playback wrote until ADR-026 (#86; the lines stay in the
    /// log, so this reader workaround stays too). A short form counts only
    /// when `key` is the single full key it is a prefix of, the same rule
    /// [`resolve_prefix`] applies: an ambiguous short key belongs to nobody.
    fn event_keys<'k>(&self, key: &'k str) -> Vec<&'k str> {
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
                    .filter(|short| self.prefix_counts.get(*short) == Some(&1)),
            )
            .collect()
    }
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
fn short_prefix_counts<'a>(full_keys: impl Iterator<Item = &'a str>) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for key in full_keys.filter(|k| k.len() > SHORT_KEY_MAX) {
        for prefix in [key.get(..8), key.get(..12)].into_iter().flatten() {
            *counts.entry(prefix.to_owned()).or_default() += 1;
        }
    }
    counts
}

/// Why a partial hash does not name one track ([`resolve_prefix`]).
pub enum Unresolved<'a> {
    /// No track's hash starts with it.
    NotFound,
    /// Different hashes start with it: every track that does.
    Ambiguous(Vec<&'a IndexedTrackInfo>),
}

/// What to tell the user when `partial` matches several `tracks`: how many, and the
/// first three.
pub fn ambiguity_message(partial: &str, tracks: &[&IndexedTrackInfo]) -> String {
    let examples: Vec<_> = tracks
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
    format!(
        "Ambiguous hash '{}' matches {} tracks:\n{}",
        partial,
        tracks.len(),
        examples.join("\n")
    )
}

/// Resolve a partial hash to the track it names (like git short refs). Hidden tracks
/// resolve too; the one lookup behind the library service and anything reading a
/// replica.
pub fn resolve_prefix<'a>(
    tracks: &'a [IndexedTrackInfo],
    partial: &str,
) -> Result<&'a IndexedTrackInfo, Unresolved<'a>> {
    let normalize = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();
    let partial_clean = normalize(partial);

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
        0 => Err(Unresolved::NotFound),
        1 => Ok(matches[0]),
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
                return Ok(*best);
            }

            // Genuinely different hashes share a prefix: real ambiguity.
            Err(Unresolved::Ambiguous(matches))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use music_facts::{FactSource, StartReason, StopReason, Title};
    use pretty_assertions::assert_eq;

    fn ts(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).unwrap()
    }

    fn src(tool: &str, version: &str) -> FactSource {
        FactSource::new(tool, version, "Unknown")
    }

    fn fact(
        entity: &str,
        value: MusicValue,
        at: u32,
        source: &FactSource,
        op: Operation,
    ) -> MusicFact {
        Fact::new(ContentHash::new(entity), value, ts(at), source.clone(), op)
    }

    fn assert_fact(entity: &str, value: MusicValue, at: u32, source: &FactSource) -> MusicFact {
        fact(entity, value, at, source, Operation::Assert)
    }

    fn retract_fact(entity: &str, value: MusicValue, at: u32, source: &FactSource) -> MusicFact {
        fact(entity, value, at, source, Operation::Retract)
    }

    fn title(t: &str) -> MusicValue {
        MusicValue::Title(Title::new(t))
    }

    // ---- per-fact application --------------------------------------------------

    #[test]
    fn replaces_is_stored_in_provenance_without_hiding_the_track() {
        let mut entry = IndexedTrackInfo::new_empty("sha256:new".to_owned());
        let source = src("test", "1.0.0");
        apply_fact_to_track(
            &mut entry,
            &MusicValue::Replaces(ContentHash::new("sha256:old")),
            ts(1),
            Operation::Assert,
            &source,
        );
        assert!(entry
            .provenance
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::Replaces(_))));
        assert!(!entry.is_hidden());
    }

    #[test]
    fn deleted_sets_and_retract_clears_deleted_at() {
        let mut entry = IndexedTrackInfo::new_empty("sha256:toDelete".to_owned());
        let source = src("test", "1.0.0");
        let deleted = MusicValue::Deleted { timestamp: ts(1) };
        apply_fact_to_track(&mut entry, &deleted, ts(1), Operation::Assert, &source);
        assert_eq!(entry.deleted_at, Some(ts(1)));
        assert!(entry.is_hidden());
        apply_fact_to_track(&mut entry, &deleted, ts(1), Operation::Retract, &source);
        assert!(!entry.is_hidden());
        assert_eq!(entry.deleted_at, None);
    }

    #[test]
    fn retract_of_another_value_leaves_the_live_one() {
        let mut entry = IndexedTrackInfo::new_empty("sha256:a".to_owned());
        let source = src("test", "1.0.0");
        apply_fact_to_track(
            &mut entry,
            &title("Live"),
            ts(1),
            Operation::Assert,
            &source,
        );
        apply_fact_to_track(
            &mut entry,
            &title("Other"),
            ts(2),
            Operation::Retract,
            &source,
        );
        assert_eq!(entry.title.as_deref(), Some("Live"));
        assert!(entry.is_live());
    }

    #[test]
    fn retract_matches_by_source_identity_not_version() {
        let mut entry = IndexedTrackInfo::new_empty("sha256:a".to_owned());
        apply_fact_to_track(
            &mut entry,
            &title("T"),
            ts(1),
            Operation::Assert,
            &src("ingest", "1.0.0"),
        );
        // Another source cannot retract it.
        apply_fact_to_track(
            &mut entry,
            &title("T"),
            ts(2),
            Operation::Retract,
            &src("user", "1.0.0"),
        );
        assert!(entry.is_live());
        // The same source at another version can.
        apply_fact_to_track(
            &mut entry,
            &title("T"),
            ts(3),
            Operation::Retract,
            &src("ingest", "2.0.0"),
        );
        assert!(!entry.is_live());
        assert_eq!(entry.title, None);
    }

    #[test]
    fn distinct_retractions_collapse_same_value_and_source() {
        let a = (title("T"), src("ingest", "1.0.0"));
        let b = (title("T"), src("ingest", "2.0.0"));
        let c = (title("T"), src("user", "1.0.0"));
        assert_eq!(distinct_retractions([a, b, c]).len(), 2);
    }

    #[test]
    fn blob_path_needs_a_hash_and_an_extension() {
        let path = std::path::Path::new("a/b.flac");
        assert_eq!(
            blob_path_for("sha256:abcdef", path),
            Some(PathBuf::from("blobs/ab/abcdef.flac"))
        );
        assert_eq!(blob_path_for("", path), None);
        assert_eq!(
            blob_path_for("sha256:abcdef", std::path::Path::new("x")),
            None
        );
    }

    #[test]
    fn hash_key_strips_prefix_and_lowercases() {
        assert_eq!(hash_key("sha256:ABCdef"), "abcdef");
        assert_eq!(hash_key("ABC"), "abc");
    }

    #[test]
    fn parse_fact_line_rejects_non_facts() {
        assert!(parse_fact_line("not a fact").is_none());
        assert!(parse_fact_line("{}").is_none());
        let line =
            serde_json::to_string(&assert_fact("sha256:a", title("T"), 1, &src("t", "1"))).unwrap();
        assert!(parse_fact_line(&line).is_some());
    }

    // ---- bulk projection -------------------------------------------------------

    #[test]
    fn project_drops_non_live_and_derives_blob_path() {
        let s = src("ingest", "1.0.0");
        let projection = project(vec![
            assert_fact("sha256:aabbcc", title("Kept"), 1, &s),
            assert_fact(
                "sha256:aabbcc",
                MusicValue::FilePath(PathBuf::from("x/y.flac")),
                1,
                &s,
            ),
            assert_fact("sha256:gone", title("Gone"), 1, &s),
            retract_fact("sha256:gone", title("Gone"), 2, &s),
            assert_fact(
                "sha256:orphan",
                MusicValue::TrackStarted(StartReason::OnRequest),
                1,
                &s,
            ),
        ]);
        assert_eq!(projection.tracks.len(), 1);
        let t = &projection.tracks[0];
        assert_eq!(t.content_hash.as_str(), "sha256:aabbcc");
        assert_eq!(t.blob_path, PathBuf::from("blobs/aa/aabbcc.flac"));
        assert_eq!(
            projection.content_hashes,
            HashSet::from(["sha256:aabbcc".to_owned()])
        );
        let title_type = FactType::new(MusicValue::Title(Title::new("x")).display_name());
        let titles = &projection.fact_index[&title_type];
        assert!(titles.contains(&title("Kept").to_string()));
        assert!(!titles.contains(&title("Gone").to_string()));
    }

    #[test]
    fn project_lines_skips_lines_that_are_not_facts() {
        let s = src("ingest", "1.0.0");
        let good = serde_json::to_string(&assert_fact("sha256:a1", title("T"), 1, &s)).unwrap();
        let projection = project_lines([good.as_str(), "garbage", "{}"]);
        assert_eq!(projection.tracks.len(), 1);
        assert_eq!(projection.unparseable_lines, 2);
        assert_eq!(project(Vec::new()).unparseable_lines, 0);
    }

    // ---- plays -----------------------------------------------------------------

    #[test]
    fn plays_survive_a_merge_through_the_replaces_lineage() {
        let ingest = src("ingest", "1.0.0");
        let play = src("playback", "1.0.0");
        let facts = vec![
            assert_fact("sha256:loser000000000", title("L"), 1, &ingest),
            assert_fact("sha256:winner00000000", title("W"), 2, &ingest),
            assert_fact(
                "sha256:loser000000000",
                MusicValue::TrackStarted(StartReason::OnRequest),
                3,
                &play,
            ),
            assert_fact(
                "sha256:loser000000000",
                MusicValue::TrackStopped(StopReason::OnSkip),
                4,
                &play,
            ),
            assert_fact(
                "sha256:winner00000000",
                MusicValue::Replaces(ContentHash::new("sha256:loser000000000")),
                5,
                &ingest,
            ),
            // The merge retires the loser's tracked facts; its plays stay in the log.
            retract_fact("sha256:loser000000000", title("L"), 6, &ingest),
        ];
        let mut projection = project(facts.clone());
        assert_eq!(projection.tracks.len(), 1);
        PlayLog::from_facts(&facts).apply_to(&mut projection.tracks);
        let winner = &projection.tracks[0];
        assert_eq!(winner.content_hash.as_str(), "sha256:winner00000000");
        assert_eq!(winner.last_started, Some(ts(3)));
        assert_eq!(winner.last_stopped, Some(ts(4)));
    }

    #[test]
    fn retracted_plays_are_not_plays_and_cancel_nothing() {
        let s = src("playback", "1.0.0");
        let started = MusicValue::TrackStarted(StartReason::OnRequest);
        let facts = vec![
            assert_fact("sha256:a1", title("T"), 1, &s),
            assert_fact("sha256:a1", started.clone(), 2, &s),
            retract_fact("sha256:a1", started, 3, &s),
        ];
        let mut tracks = project(facts.clone()).tracks;
        PlayLog::from_facts(&facts).apply_to(&mut tracks);
        assert_eq!(tracks[0].last_started, Some(ts(2)));
    }

    #[test]
    fn merge_keeps_the_latest_time_per_key() {
        let s = src("playback", "1.0.0");
        let started = MusicValue::TrackStarted(StartReason::OnRequest);
        let late_play = assert_fact("sha256:a1", started.clone(), 5, &s);
        let early_play = assert_fact("sha256:a1", started, 2, &s);
        let mut early = PlayLog::from_facts(&[late_play]);
        early.merge(PlayLog::from_facts(&[early_play]));
        let mut tracks = vec![IndexedTrackInfo::new_empty("sha256:a1".to_owned())];
        early.apply_to(&mut tracks);
        assert_eq!(tracks[0].last_started, Some(ts(5)));
    }

    // ---- play histories --------------------------------------------------------

    fn ingest() -> FactSource {
        src("ingest", "1.0.0")
    }

    /// A fact that makes `entity` a live track.
    fn live(entity: &str) -> MusicFact {
        assert_fact(entity, title("T"), 1, &ingest())
    }

    fn started(entity: &str, at: u32) -> MusicFact {
        let value = MusicValue::TrackStarted(StartReason::OnRequest);
        assert_fact(entity, value, at, &src("playback", "1.0.0"))
    }

    fn stopped(entity: &str, at: u32) -> MusicFact {
        let value = MusicValue::TrackStopped(StopReason::OnSkip);
        assert_fact(entity, value, at, &src("playback", "1.0.0"))
    }

    fn replaces(winner: &str, loser: &str, at: u32) -> MusicFact {
        let value = MusicValue::Replaces(ContentHash::new(loser));
        assert_fact(winner, value, at, &ingest())
    }

    /// The history of the live track `hash` in the projection of `facts`.
    fn history_of(facts: &[MusicFact], hash: &str) -> PlayHistory {
        let p = project(facts.to_vec());
        let track = p
            .tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash)
            .expect("the track is live");
        p.plays.resolved(&p.tracks).history(track)
    }

    /// Each play as `start-stop` by day of the month; either side empty when missing.
    fn shape(history: &PlayHistory) -> Vec<String> {
        use chrono::Datelike;
        history
            .plays
            .iter()
            .map(|play| match play {
                Play::Started { at, stop, .. } => format!(
                    "{}-{}",
                    at.day(),
                    stop.map_or(String::new(), |s| s.at.day().to_string())
                ),
                Play::StopOnly(stop) => format!("-{}", stop.at.day()),
            })
            .collect()
    }

    #[test]
    fn a_losers_short_key_plays_show_on_the_winner_and_apply_to_agrees() {
        let winner = "sha256:aabbccdd11223344556677";
        let loser = "sha256:99887766aabbccddeeff00";
        let mut facts = vec![live(winner), live(loser)];
        // Older playback wrote the loser's plays under its 8-character key.
        for day in [2, 4, 6, 8] {
            facts.push(started("sha256:99887766", day));
            facts.push(stopped("sha256:99887766", day + 1));
        }
        facts.push(replaces(winner, loser, 20));
        facts.push(retract_fact(loser, title("T"), 21, &ingest()));

        let p = project(facts.clone());
        assert_eq!(p.tracks.len(), 1, "the loser is gone");
        let history = p.plays.resolved(&p.tracks).history(&p.tracks[0]);
        assert_eq!(history.count(), 4);
        assert_eq!(shape(&history), ["2-3", "4-5", "6-7", "8-9"]);

        let mut tracks = p.tracks;
        PlayLog::from_facts(&facts).apply_to(&mut tracks);
        assert_eq!(tracks[0].last_started, Some(ts(8)));
        assert_eq!(tracks[0].last_stopped, Some(ts(9)));
        assert_eq!(tracks[0].last_started, history.last_started());
        assert_eq!(tracks[0].last_stopped, history.last_stopped());
    }

    #[test]
    fn twelve_character_keys_count_and_a_shared_short_key_credits_nobody() {
        let a = "sha256:aabbccdd0011223344";
        let b = "sha256:aabbccdd9988776655";
        let c = "sha256:eeff0011aabbccddee";
        let facts = vec![
            live(a),
            live(b),
            live(c),
            // a's 12-character key: only a starts with it.
            started("sha256:aabbccdd0011", 2),
            // The 8-character key a and b share: it belongs to nobody.
            started("sha256:aabbccdd", 3),
            // c's 8-character key, and its 12-character key.
            started("sha256:eeff0011", 4),
            started("sha256:eeff0011aabb", 5),
        ];
        assert_eq!(shape(&history_of(&facts, a)), ["2-"]);
        assert_eq!(shape(&history_of(&facts, b)), Vec::<String>::new());
        assert_eq!(shape(&history_of(&facts, c)), ["4-", "5-"]);
    }

    #[test]
    fn a_full_and_a_short_key_in_one_lineage_count_each_play_once() {
        let winner = "sha256:aabbccdd11223344556677";
        // The winner replaces the legacy short hash its own full hash starts with, so
        // the short key is reached twice: as the winner's 8-character key, and as a
        // member of its lineage.
        let facts = vec![
            live(winner),
            replaces(winner, "sha256:aabbccdd", 5),
            started("sha256:aabbccdd", 2),
            stopped("sha256:aabbccdd", 3),
            started(winner, 6),
            stopped(winner, 7),
        ];
        let history = history_of(&facts, winner);
        assert_eq!(history.count(), 2);
        assert_eq!(shape(&history), ["2-3", "6-7"]);
    }

    #[test]
    fn a_transitive_chain_credits_every_ancestor_and_no_descendant() {
        let (w, m, l) = (
            "sha256:aaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbb",
            "sha256:cccccccccccccccc",
        );
        // An unfinished merge keeps M live, with its own `Replaces`.
        let facts = vec![
            live(w),
            live(m),
            replaces(w, m, 8),
            replaces(m, l, 9),
            started(l, 2),
            stopped(l, 3),
            started(m, 4),
            stopped(m, 5),
            started(w, 6),
            stopped(w, 7),
        ];
        assert_eq!(
            shape(&history_of(&facts, w)),
            ["2-3", "4-5", "6-7"],
            "W -> M -> L"
        );
        assert_eq!(shape(&history_of(&facts, m)), ["2-3", "4-5"]);
    }

    #[test]
    fn a_start_opens_a_play_and_the_next_stop_closes_it() {
        let w = "sha256:aaaaaaaaaaaaaaaa";
        // (events in log order, S<day> a start and T<day> a stop; the plays they pair into)
        let cases: [(&str, &[&str]); 8] = [
            ("S1 T2", &["1-2"]),
            ("S1 S2 T3", &["1-", "2-3"]),
            ("T1", &["-1"]),
            ("S1 T2 T3", &["1-2", "-3"]),
            ("S1", &["1-"]),
            ("T1 S2 T3", &["-1", "2-3"]),
            // A stop logged before a start of the same instant still closes it.
            ("T2 S2", &["2-2"]),
            // Events are paired in time order, not log order.
            ("T3 S1", &["1-3"]),
        ];
        for (script, expected) in cases {
            let mut facts = vec![live(w)];
            for word in script.split(' ') {
                let fact = match word.split_at(1) {
                    ("S", day) => started(w, day.parse().unwrap()),
                    (_, day) => stopped(w, day.parse().unwrap()),
                };
                facts.push(fact);
            }
            assert_eq!(shape(&history_of(&facts, w)), expected, "{script}");
        }
    }

    #[test]
    fn a_history_says_how_many_times_and_when() {
        let w = "sha256:aaaaaaaaaaaaaaaa";
        let facts = vec![
            live(w),
            started(w, 1),
            stopped(w, 2),
            started(w, 5),
            stopped(w, 9),
            stopped(w, 11),
        ];
        let history = history_of(&facts, w);
        assert_eq!(history.count(), 3, "a stop with no start is a play too");
        assert_eq!(history.last_started(), Some(ts(5)));
        assert_eq!(history.last_stopped(), Some(ts(11)));
        assert_eq!(history.last_played(), Some(ts(11)));

        let never = history_of(&[live(w)], w);
        assert_eq!(never.count(), 0);
        assert_eq!(never.last_played(), None);
        assert_eq!(never.last_started(), None);
        assert_eq!(never.last_stopped(), None);
    }

    #[test]
    fn retracted_plays_are_not_counted_twice_or_at_all() {
        let w = "sha256:aaaaaaaaaaaaaaaa";
        let play = src("playback", "1.0.0");
        let facts = vec![
            live(w),
            started(w, 2),
            retract_fact(
                w,
                MusicValue::TrackStarted(StartReason::OnRequest),
                3,
                &play,
            ),
            retract_fact(w, MusicValue::TrackStopped(StopReason::OnSkip), 4, &play),
        ];
        let history = history_of(&facts, w);
        assert_eq!(shape(&history), ["2-"], "the retracts are not plays");
        assert_eq!(history.last_stopped(), None);
    }

    #[test]
    fn recent_lists_the_newest_plays_first_with_inherited_ones() {
        let (a, b, l) = (
            "sha256:aaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbb",
            "sha256:cccccccccccccccc",
        );
        // A replaces L, which is gone; L's play is A's.
        let facts = vec![
            live(a),
            live(b),
            replaces(a, l, 20),
            started(a, 2),
            stopped(a, 3),
            started(b, 4),
            started(l, 5),
            stopped(l, 6),
            started(a, 7),
        ];
        let p = project(facts);
        let recent = |n| {
            p.plays
                .recent(&p.tracks, n)
                .into_iter()
                .map(|(i, play)| {
                    let hash = p.tracks[i].content_hash.as_str().to_owned();
                    let history = PlayHistory { plays: vec![play] };
                    (hash, shape(&history).remove(0))
                })
                .collect::<Vec<_>>()
        };
        let all = vec![
            (a.to_owned(), "7-".to_owned()),
            (a.to_owned(), "5-6".to_owned()),
            (b.to_owned(), "4-".to_owned()),
            (a.to_owned(), "2-3".to_owned()),
        ];
        assert_eq!(recent(10), all);
        assert_eq!(recent(3), all[..3], "n cuts the oldest");
        assert_eq!(recent(0), vec![]);
    }

    #[test]
    fn project_lines_records_the_same_plays_as_the_facts() {
        let w = "sha256:aaaaaaaaaaaaaaaa";
        let facts = vec![live(w), started(w, 2), stopped(w, 3), started(w, 4)];
        let lines: Vec<String> = facts
            .iter()
            .map(|f| serde_json::to_string(f).unwrap())
            .collect();
        let p = project_lines(lines.iter().map(String::as_str));
        let expected = PlayLog::from_facts(&facts).histories(&p.tracks);
        assert_eq!(p.plays.histories(&p.tracks), expected);
        assert_eq!(expected[0].count(), 2, "the fixture has plays");
    }

    #[test]
    fn resolving_once_serves_every_track_like_histories_does() {
        let (a, b, l) = (
            "sha256:aaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbb",
            "sha256:cccccccccccccccc",
        );
        let facts = vec![
            live(a),
            live(b),
            replaces(a, l, 20),
            started(a, 2),
            started(b, 4),
            started(l, 5),
            stopped(l, 6),
        ];
        let p = project(facts);
        let resolved = p.plays.resolved(&p.tracks);

        let one_by_one: Vec<PlayHistory> = p.tracks.iter().map(|t| resolved.history(t)).collect();

        assert_eq!(one_by_one, p.plays.histories(&p.tracks));
        let counts: Vec<usize> = one_by_one.iter().map(PlayHistory::count).collect();
        assert_eq!(counts.iter().sum::<usize>(), 3, "a has two plays, b one");
    }

    // ---- resolving a partial hash ----------------------------------------------

    fn entry(hash: &str, name: &str) -> IndexedTrackInfo {
        let mut t = IndexedTrackInfo::new_empty(hash.to_owned());
        t.title = Some(name.to_owned());
        t
    }

    /// The hash `partial` resolves to; panics when it does not resolve.
    fn resolved<'a>(tracks: &'a [IndexedTrackInfo], partial: &str) -> &'a str {
        match resolve_prefix(tracks, partial) {
            Ok(track) => track.content_hash.as_str(),
            Err(_) => panic!("{partial} should resolve"),
        }
    }

    #[test]
    fn a_partial_hash_resolves_to_the_one_track_it_starts() {
        let tracks = [
            entry("sha256:abcdef0011223344", "A"),
            entry("sha256:99", "B"),
        ];
        let full = "sha256:abcdef0011223344";
        assert_eq!(resolved(&tracks, "abcdef00"), full);
        assert_eq!(resolved(&tracks, "sha256:abcdef00"), full);
        assert_eq!(resolved(&tracks, "ABCDEF00"), full, "case does not matter");
        let missing = resolve_prefix(&tracks, "ffff");
        assert!(matches!(missing, Err(Unresolved::NotFound)));
    }

    #[test]
    fn duplicate_entries_of_one_hash_resolve_to_it() {
        let tracks = [entry("10e95ec1", "Track A"), entry("10e95ec1", "Track B")];
        assert_eq!(resolved(&tracks, "10e95ec1"), "10e95ec1");
    }

    #[test]
    fn a_legacy_short_hash_and_its_full_hash_resolve_to_the_full_one() {
        let full = "sha256:9fb4105eaabbccdd11223344556677889900aabb";
        let tracks = [entry("9fb4105e", "Unknown"), entry(full, "20 Minutes")];
        assert_eq!(resolved(&tracks, "9fb4105e"), full);
    }

    #[test]
    fn different_hashes_sharing_a_prefix_are_ambiguous() {
        let tracks = [
            entry("sha256:abcdef0011223344", "Track A"),
            entry("sha256:abcdef0099887766", "Track B"),
        ];
        let Err(Unresolved::Ambiguous(found)) = resolve_prefix(&tracks, "sha256:abcdef00") else {
            panic!("a real ambiguity must not resolve");
        };
        assert_eq!(found.len(), 2);
        assert_eq!(
            ambiguity_message("sha256:abcdef00", &found),
            "Ambiguous hash 'sha256:abcdef00' matches 2 tracks:\n  abcdef00 (Track A)\n  abcdef00 (Track B)"
        );
    }

    // ---- split-point property (ADR-014 §14) -------------------------------------

    /// A fixture log covering merges with `Replaces`, retracts by source identity (a
    /// different version of the asserting source), plays, multi-valued fields and a
    /// retract-to-empty followed by a re-assert. Split points fall between write
    /// batches; within a batch ingest writes FilePath first, and ACID never exposes half
    /// a batch.
    fn fixture() -> Vec<MusicFact> {
        let ingest1 = src("ingest", "1.0.0");
        let ingest2 = src("ingest", "2.0.0");
        let user = src("user", "1.0.0");
        let play = src("playback", "1.0.0");
        let (a, b, c, d) = (
            "sha256:aaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbb",
            "sha256:cccccccccccccccc",
            "sha256:dddddddddddddddd",
        );
        let (e, f, g, h, i) = (
            "sha256:eeeeeeeeeeeeeeee",
            "sha256:ffffffffffffffff",
            "sha256:0000000000000000",
            "sha256:1111111111111111",
            "sha256:2222222222222222",
        );
        let (j, k, l) = (
            "sha256:3333333333333333",
            "sha256:4444444444444444",
            "sha256:5555555555555555",
        );
        let started = MusicValue::TrackStarted(StartReason::OnRequest);
        let stopped = MusicValue::TrackStopped(StopReason::OnSkip);
        let bpm = |v: f32| MusicValue::Bpm(music_facts::Bpm::from_f32(v).unwrap());
        vec![
            assert_fact(a, title("A"), 1, &ingest1),
            assert_fact(
                a,
                MusicValue::FilePath(PathBuf::from("a.flac")),
                1,
                &ingest1,
            ),
            assert_fact(a, MusicValue::AddedAt(ts(1)), 1, &ingest1),
            assert_fact(a, bpm(120.0), 1, &ingest1),
            assert_fact(
                a,
                MusicValue::StyleDescriptor("deep".to_owned()),
                1,
                &ingest1,
            ),
            assert_fact(a, MusicValue::StyleDescriptor("deep".to_owned()), 2, &user),
            assert_fact(a, started.clone(), 3, &play),
            assert_fact(b, title("B"), 2, &ingest1),
            assert_fact(b, MusicValue::FilePath(PathBuf::from("b.mp3")), 2, &ingest1),
            assert_fact(b, bpm(128.0), 2, &user),
            assert_fact(b, started.clone(), 4, &play),
            assert_fact(b, stopped.clone(), 5, &play),
            // Retract by source identity: ingest 2.0.0 cancels what ingest 1.0.0 wrote.
            retract_fact(a, bpm(120.0), 6, &ingest2),
            assert_fact(a, bpm(122.0), 6, &user),
            // Retract of a value another source asserted: no effect.
            retract_fact(b, bpm(128.0), 7, &ingest1),
            // Merge: A replaces B; B's tracked facts are retracted, its plays stay.
            assert_fact(a, MusicValue::Replaces(ContentHash::new(b)), 8, &ingest1),
            retract_fact(b, title("B"), 9, &ingest2),
            retract_fact(b, bpm(128.0), 9, &user),
            // Retracting an entity that is already gone.
            retract_fact(b, title("B"), 10, &ingest2),
            assert_fact(c, title("C"), 10, &ingest1),
            assert_fact(c, MusicValue::AddedAt(ts(10)), 10, &ingest1),
            assert_fact(c, started, 11, &play),
            // Retract to empty, then come back.
            retract_fact(c, title("C"), 12, &ingest1),
            assert_fact(c, title("C again"), 13, &ingest1),
            assert_fact(c, stopped, 14, &play),
            // Never live: a retract with no assertion, then a lone play.
            retract_fact(d, title("D"), 15, &ingest1),
            assert_fact(
                d,
                MusicValue::TrackStarted(StartReason::OnRequest),
                16,
                &play,
            ),
            // An unfinished merge, as an older release left it: F replaces E, and the
            // merge retracts E's title and path but misses its bpm, so E stays live.
            assert_fact(e, title("E"), 17, &ingest1),
            assert_fact(
                e,
                MusicValue::FilePath(PathBuf::from("e.flac")),
                17,
                &ingest1,
            ),
            assert_fact(e, bpm(100.0), 17, &user),
            assert_fact(f, title("F"), 18, &ingest1),
            assert_fact(f, MusicValue::Replaces(ContentHash::new(e)), 19, &ingest1),
            retract_fact(e, title("E"), 20, &ingest1),
            retract_fact(
                e,
                MusicValue::FilePath(PathBuf::from("e.flac")),
                20,
                &ingest1,
            ),
            // A finished merge whose loser is ingested again afterwards: G is not retired.
            assert_fact(g, title("G"), 17, &ingest1),
            assert_fact(f, MusicValue::Replaces(ContentHash::new(g)), 19, &ingest1),
            retract_fact(g, title("G"), 20, &ingest1),
            assert_fact(g, title("G again"), 21, &ingest1),
            // A `Replaces` that is retracted again releases its loser.
            assert_fact(h, title("H"), 22, &ingest1),
            assert_fact(i, title("I"), 22, &ingest1),
            assert_fact(i, MusicValue::Replaces(ContentHash::new(h)), 23, &ingest1),
            retract_fact(i, MusicValue::Replaces(ContentHash::new(h)), 24, &ingest1),
            // Two winners hold a `Replaces` of the live L; K's comes first, J's second.
            // J's is retracted again: L stays retired, by K, the one that still holds it.
            assert_fact(j, title("J"), 25, &ingest1),
            assert_fact(k, title("K"), 25, &ingest1),
            assert_fact(l, bpm(90.0), 25, &user),
            assert_fact(k, MusicValue::Replaces(ContentHash::new(l)), 26, &ingest1),
            assert_fact(j, MusicValue::Replaces(ContentHash::new(l)), 26, &ingest1),
            retract_fact(j, MusicValue::Replaces(ContentHash::new(l)), 27, &ingest1),
        ]
    }

    fn sorted(mut tracks: Vec<IndexedTrackInfo>) -> Vec<IndexedTrackInfo> {
        tracks.sort_by(|x, y| x.content_hash.as_str().cmp(y.content_hash.as_str()));
        tracks
    }

    /// Every field of every track, rendered for comparison.
    fn render(tracks: &[IndexedTrackInfo]) -> Vec<String> {
        tracks
            .iter()
            .map(|t| {
                format!(
                    "{} title={:?} artist={:?} album={:?} label={:?} genre={:?} styles={:?} \
                     duration={:?} bpm={:?} key={:?} year={:?} source={:?} blob={:?} cover={:?} \
                     track={:?} disc={:?} started={:?} stopped={:?} added={:?} item={:?} \
                     deleted={:?} role={:?} energy={:?} grid={:?} cues={:?} raw={:?} \
                     retired_by={:?} prov={:?}",
                    t.content_hash.as_str(),
                    t.title,
                    t.artist,
                    t.album,
                    t.label,
                    t.genre,
                    t.styles,
                    t.duration_seconds,
                    t.bpm,
                    t.key,
                    t.year,
                    t.source,
                    t.blob_path,
                    t.cover_art_path,
                    t.track_number,
                    t.disc_number,
                    t.last_started,
                    t.last_stopped,
                    t.added_at,
                    t.item_id,
                    t.deleted_at,
                    t.role,
                    t.energy,
                    t.beat_grid,
                    t.memory_cues,
                    t.raw_file_path,
                    t.retired_by,
                    t.provenance
                        .iter()
                        .map(|(v, s)| format!("{v}|{:?}", s.identity()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    /// Incrementally apply `facts` to `part`, then drop the entries that are not live.
    fn fold_incrementally(part: &mut Projection, facts: &[MusicFact]) {
        let mut positions: HashMap<String, usize> = part
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.content_hash.as_str().to_owned(), i))
            .collect();
        for f in facts {
            apply_fact(
                &mut part.tracks,
                &mut positions,
                &mut part.content_hashes,
                &mut part.fact_index,
                f,
            );
        }
        retain_live(&mut part.tracks, &mut part.content_hashes);
    }

    /// The whole log folded in one go: its rendered tracks, its fact index and the
    /// play history of each track (in the order of the rendered tracks).
    struct Whole {
        rendered: Vec<String>,
        fact_index: HashMap<FactType, HashSet<String>>,
        histories: Vec<PlayHistory>,
    }

    fn whole_log(facts: &[MusicFact]) -> Whole {
        let mut whole = project(facts.to_vec());
        PlayLog::from_facts(facts).apply_to(&mut whole.tracks);
        let tracks = sorted(whole.tracks);
        Whole {
            rendered: render(&tracks),
            fact_index: whole.fact_index,
            histories: whole.plays.histories(&tracks),
        }
    }

    fn assert_matches_whole(mut part: Projection, log: &PlayLog, expected: &Whole, label: &str) {
        log.apply_to(&mut part.tracks);
        let tracks = sorted(part.tracks);
        assert_eq!(
            log.histories(&tracks),
            expected.histories,
            "{label}: play histories"
        );
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.content_hash.as_str().to_owned())
                .collect::<HashSet<_>>(),
            part.content_hashes,
            "{label}: content_hashes must be the live set"
        );
        assert_eq!(render(&tracks), expected.rendered, "{label}");
        assert_eq!(part.fact_index, expected.fact_index, "{label}: fact_index");
    }

    #[test]
    fn every_split_of_the_log_projects_the_same_tracks() {
        let facts = fixture();
        let expected = whole_log(&facts);
        assert!(!expected.rendered.is_empty());

        for k in 0..=facts.len() {
            let (prefix, suffix) = facts.split_at(k);

            // Bulk projection of the prefix, then incremental application of the suffix.
            let mut part = project(prefix.to_vec());
            let mut events = PlayLog::from_facts(prefix);
            fold_incrementally(&mut part, suffix);
            events.merge(PlayLog::from_facts(suffix));
            assert_matches_whole(part, &events, &expected, &format!("split {k}"));
        }
    }

    #[test]
    fn every_two_splits_with_a_sweep_between_project_the_same_tracks() {
        let facts = fixture();
        let expected = whole_log(&facts);

        for i in 0..=facts.len() {
            for j in i..=facts.len() {
                let (prefix, rest) = facts.split_at(i);
                let (mid, tail) = rest.split_at(j - i);

                // prefix bulk, then mid incrementally and swept, then tail incrementally.
                let mut part = project(prefix.to_vec());
                let mut events = PlayLog::from_facts(prefix);
                fold_incrementally(&mut part, mid);
                events.merge(PlayLog::from_facts(mid));
                fold_incrementally(&mut part, tail);
                events.merge(PlayLog::from_facts(tail));
                assert_matches_whole(part, &events, &expected, &format!("splits {i}/{j}"));
            }
        }
    }

    #[test]
    fn a_short_key_play_is_not_made_ambiguous_by_a_retired_non_play_entity() {
        let s = src("ingest", "1.0.0");
        let play = src("playback", "1.0.0");
        let facts = vec![
            assert_fact("sha256:aabbccdd11223344", title("Live"), 1, &s),
            // Retired, and it never had a play: it must not claim the short key.
            assert_fact("sha256:aabbccdd99887766", title("Retired"), 2, &s),
            retract_fact("sha256:aabbccdd99887766", title("Retired"), 3, &s),
            assert_fact(
                "sha256:aabbccdd",
                MusicValue::TrackStarted(StartReason::OnRequest),
                4,
                &play,
            ),
        ];
        let mut tracks = project(facts.clone()).tracks;
        assert_eq!(tracks.len(), 1);
        PlayLog::from_facts(&facts).apply_to(&mut tracks);
        assert_eq!(tracks[0].last_started, Some(ts(4)));
    }

    #[test]
    fn the_fixture_exercises_what_it_claims() {
        let facts = fixture();
        let mut p = project(facts.clone());
        PlayLog::from_facts(&facts).apply_to(&mut p.tracks);
        let tracks = sorted(p.tracks);
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        // B was merged away, D never live.
        assert_eq!(
            hashes,
            [
                "sha256:0000000000000000",
                "sha256:1111111111111111",
                "sha256:2222222222222222",
                "sha256:3333333333333333",
                "sha256:4444444444444444",
                "sha256:5555555555555555",
                "sha256:aaaaaaaaaaaaaaaa",
                "sha256:cccccccccccccccc",
                "sha256:eeeeeeeeeeeeeeee",
                "sha256:ffffffffffffffff",
            ]
        );
        let by = |hash: &str| {
            tracks
                .iter()
                .find(|t| t.content_hash.as_str() == hash)
                .unwrap()
        };
        let a = by("sha256:aaaaaaaaaaaaaaaa");
        // Retract by source identity removed ingest's bpm; the user's survives.
        assert_eq!(a.bpm, Some(122.0));
        // A carries B's plays through the lineage, and its own.
        assert_eq!(a.last_started, Some(ts(4)));
        assert_eq!(a.last_stopped, Some(ts(5)));
        let history = p.plays.resolved(&tracks).history(a);
        assert_eq!(shape(&history), ["3-", "4-5"]);
        assert_eq!(a.blob_path, PathBuf::from("blobs/aa/aaaaaaaaaaaaaaaa.flac"));
        // C was reset on retract-to-empty, then re-asserted.
        let c = by("sha256:cccccccccccccccc");
        assert_eq!(c.title.as_deref(), Some("C again"));
        assert_eq!(c.added_at, None);
    }

    #[test]
    fn a_loser_a_merge_did_not_finish_is_retired_and_nothing_else_is() {
        let facts = fixture();
        for (label, p) in [
            ("bulk", project(facts.clone()).tracks),
            ("incremental", {
                let mut p = project(Vec::new());
                fold_incrementally(&mut p, &facts);
                p.tracks
            }),
        ] {
            // (first hex digit of the loser, first hex digit of its winner)
            let digit = |h: &str| h.chars().nth("sha256:".len()).unwrap();
            let retired: Vec<(char, char)> = sorted(p.clone())
                .iter()
                .filter_map(|t| {
                    let winner = t.retired_by.as_deref()?;
                    Some((digit(t.content_hash.as_str()), digit(winner)))
                })
                .collect();
            // L (5) is retired by K (4): J's `Replaces` was retracted, K's still holds it.
            assert_eq!(retired, [('5', '4'), ('e', 'f')], "{label}");
            let ghost = p
                .iter()
                .find(|t| t.content_hash.as_str().contains("eeee"))
                .unwrap();
            assert!(ghost.is_retired() && ghost.is_live(), "{label}");
            // The retract of its path cleared the file, though the track outlives it.
            assert!(ghost.blob_path.as_os_str().is_empty(), "{label}");
            assert_eq!(ghost.raw_file_path, None, "{label}");
        }
    }

    #[test]
    fn a_file_path_retract_of_another_path_changes_nothing() {
        let s = src("ingest", "1.0.0");
        let path = |p: &str| MusicValue::FilePath(PathBuf::from(p));
        let facts = vec![
            assert_fact("sha256:aabbcc", title("T"), 1, &s),
            assert_fact("sha256:aabbcc", path("x.flac"), 1, &s),
            retract_fact("sha256:aabbcc", path("other.flac"), 2, &s),
        ];
        let t = project(facts).tracks.remove(0);
        assert_eq!(t.blob_path, PathBuf::from("blobs/aa/aabbcc.flac"));
    }
}
