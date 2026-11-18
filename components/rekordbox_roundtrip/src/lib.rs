//! Semantic mapping between Rekordbox XML types and MDMA music facts.
//!
//! This component provides:
//!
//! - [`HashLocation`] — typed (hash, location) pair used by the hash↔location maps
//! - [`build_hash_to_location`] — map content hashes to export location URIs (three hash forms)
//! - [`location_to_hash`] — reverse map: location URI → full content hash
//! - [`grid_from_tempo`] — convert a Rekordbox [`TempoAnchor`] + BPM to a [`MusicValue::BeatGrid`]
//! - [`cues_from_position_marks`] — convert Rekordbox [`PositionMark`]s to [`MusicValue::MemoryCue`] facts
//! - [`position_marks_from_cues`] — exact inverse of `cues_from_position_marks`
//! - [`Stars`] and [`decide`] — Rekordbox `Rating` ↔ energy, and the three-way merge of
//!   an import (ADR-034)

use std::collections::{hash_map::Entry, HashMap};

use music_facts::{Bpm, ContentHash, CueKind, EnergyLevel, MusicValue};
use rekordbox_xml::xml::{PositionMark, TempoAnchor};
use sequence_merge::scalar::merge3_scalar;
use serde::{Deserialize, Serialize};

pub use sequence_merge::scalar::Decision;

// =============================================================================
// Hash ↔ Location maps (hoisted from mdma-cli)
// =============================================================================

/// A typed (content_hash, location_uri) pair used by the hash↔location maps.
///
/// Using a named struct instead of `(String, String)` prevents accidentally
/// passing the arguments in the wrong order.
pub struct HashLocation {
    pub hash: ContentHash,
    pub location: String,
}

/// Build a `hash → location-URI` map that resolves all three hash forms a
/// playlist may use to look up a track:
///
/// - Full `sha256:<64hex>` form
/// - 12-char short hex (canonical 0.18.1+ playlist format)
/// - 8-char short hex (legacy 0.18.x playlist format)
///
/// Short-hash collisions are detected: the first writer wins and a warning is
/// printed to stderr.
pub fn build_hash_to_location(entries: &[HashLocation]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for entry in entries {
        let full = entry.hash.as_str().to_string();
        let location = &entry.location;
        let hex = full.strip_prefix("sha256:").unwrap_or(&full);

        // Full hash: collision is impossible by construction — always insert.
        map.insert(full.clone(), location.clone());

        // Short prefixes: detect conflicts; first writer wins, warn on divergence.
        let short_keys: &[usize] = &[12, 8];
        for &len in short_keys {
            if hex.len() >= len {
                let short = hex[..len].to_string();
                match map.entry(short) {
                    Entry::Vacant(e) => {
                        e.insert(location.clone());
                    }
                    Entry::Occupied(e) if e.get() == location.as_str() => {
                        // Same location — no-op.
                    }
                    Entry::Occupied(e) => {
                        eprintln!(
                            "warning: short hash prefix {} maps to multiple tracks; \
                             playlists referring to this prefix will resolve to {}",
                            e.key(),
                            e.get()
                        );
                        // Do not overwrite — first writer wins.
                    }
                }
            }
        }
    }
    map
}

/// Build a `location-URI → content-hash` map from the same [`HashLocation`]
/// entries used by [`build_hash_to_location`].
///
/// Location is authoritative for matching when MDMA exported the files (the
/// manifest records the exact destination path).  Falls back to
/// `track_matcher` when a location is not in the manifest.
///
/// Only the full content hash is stored (no short-hash aliasing needed here —
/// locations are unique per file).
pub fn location_to_hash(entries: &[HashLocation]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|e| (e.location.clone(), e.hash.as_str().to_string()))
        .collect()
}

// =============================================================================
// BeatGrid mapping
// =============================================================================

/// Convert a Rekordbox `<TEMPO>` anchor and BPM value to a `BeatGrid` fact.
///
/// The anchor's `inizio_seconds` is converted to milliseconds by **rounding**
/// (not truncating) to preserve sub-millisecond precision in the source data.
/// `beats_per_bar` is always 4 (4/4 time — Rekordbox does not export the
/// time signature separately in the single-anchor grid format).
///
/// Rekordbox is the beat-grid master; MDMA never writes `<TEMPO>` grid data
/// back on export.
pub fn grid_from_tempo(anchor: &TempoAnchor, bpm: Bpm) -> MusicValue {
    let first_beat_ms = (anchor.inizio_seconds * 1000.0).round() as u32;
    MusicValue::BeatGrid {
        first_beat_ms,
        bpm,
        beats_per_bar: 4,
    }
}

// =============================================================================
// Cue mapping
// =============================================================================

/// Convert Rekordbox `<POSITION_MARK>` elements to MDMA `MemoryCue` facts.
///
/// **Mapping rules:**
///
/// | Rekordbox          | MDMA                                            |
/// |--------------------|------------------------------------------------|
/// | `Type=0`, `Num=-1` | `CueKind::Memory`, `index=None`                |
/// | `Type=0`, `Num≥0`  | `CueKind::Hot`, `index=Some(num as u8)`        |
/// | `Type=4` + `End`   | `CueKind::Loop { length_ms=(end-start)×1000 }` |
/// | Empty `Name`       | `label=None`                                   |
///
/// For loops, `Num=-1` → `index=None` (memory loop); `Num≥0` → `index=Some(num)`.
///
/// **Unknown types** (`Type` ≠ 0 and ≠ 4): the mark is silently skipped.
/// Callers can detect this by comparing input and output counts.  Rekordbox 7
/// introduced `Type=1` (phrase markers) which are intentionally dropped here
/// because MDMA has no equivalent concept.
///
/// Seconds are converted to milliseconds by rounding (not truncating).
pub fn cues_from_position_marks(marks: &[PositionMark]) -> Vec<MusicValue> {
    marks
        .iter()
        .filter_map(|mark| match mark.mark_type {
            0 => {
                let position_ms = secs_to_ms(mark.start_seconds);
                let (kind, index) = if mark.num == -1 {
                    (CueKind::Memory, None)
                } else {
                    // RB hot-cue slots are 0–7; values above 255 are truncated (lossy by design).
                    (CueKind::Hot, Some(mark.num as u8))
                };
                let label = non_empty(mark.name.as_str());
                Some(MusicValue::MemoryCue {
                    position_ms,
                    kind,
                    label,
                    index,
                })
            }
            4 => {
                let end = mark.end_seconds?;
                let position_ms = secs_to_ms(mark.start_seconds);
                let length_ms = secs_to_ms(end - mark.start_seconds);
                let kind = CueKind::Loop { length_ms };
                let index = if mark.num == -1 {
                    None
                } else {
                    // RB hot-loop slots are 0–7; values above 255 are truncated (lossy by design).
                    Some(mark.num as u8)
                };
                let label = non_empty(mark.name.as_str());
                Some(MusicValue::MemoryCue {
                    position_ms,
                    kind,
                    label,
                    index,
                })
            }
            // Unknown mark types are silently skipped.  This is the correct
            // choice because: (a) MDMA has no concept for Rekordbox phrase
            // markers or future types; (b) the caller can detect skips by
            // comparing input.len() vs output.len() if needed.
            _ => None,
        })
        .collect()
}

/// Convert MDMA `MemoryCue` facts back to Rekordbox `<POSITION_MARK>` elements.
///
/// This is the **exact inverse** of [`cues_from_position_marks`]:
///
/// | MDMA                                  | Rekordbox                             |
/// |---------------------------------------|---------------------------------------|
/// | `CueKind::Memory`                     | `Type=0`, `Num=-1`                    |
/// | `CueKind::Hot`, `index=Some(n)`       | `Type=0`, `Num=n`                     |
/// | `CueKind::Hot`, `index=None`          | `Type=0`, `Num=0` (fallback)          |
/// | `CueKind::Loop`, `index=None`         | `Type=4`, `Num=-1` (memory loop)      |
/// | `CueKind::Loop`, `index=Some(n)`      | `Type=4`, `Num=n` (hot loop)          |
/// | `label=None`                          | `Name=""`                             |
///
/// Non-`MemoryCue` `MusicValue` variants are silently skipped.
///
/// Milliseconds are converted to seconds exactly (`ms as f64 / 1000.0`).
pub fn position_marks_from_cues(cues: &[MusicValue]) -> Vec<PositionMark> {
    cues.iter()
        .filter_map(|v| {
            if let MusicValue::MemoryCue {
                position_ms,
                kind,
                label,
                index,
            } = v
            {
                let start_seconds = *position_ms as f64 / 1000.0;
                let name = label.clone().unwrap_or_default();
                match kind {
                    CueKind::Memory => Some(PositionMark {
                        name,
                        mark_type: 0,
                        start_seconds,
                        end_seconds: None,
                        num: -1,
                    }),
                    CueKind::Hot => {
                        let num = index.map(|i| i as i32).unwrap_or(0);
                        Some(PositionMark {
                            name,
                            mark_type: 0,
                            start_seconds,
                            end_seconds: None,
                            num,
                        })
                    }
                    CueKind::Loop { length_ms } => {
                        let end_seconds = start_seconds + *length_ms as f64 / 1000.0;
                        let num = index.map(|i| i as i32).unwrap_or(-1);
                        Some(PositionMark {
                            name,
                            mark_type: 4,
                            start_seconds,
                            end_seconds: Some(end_seconds),
                            num,
                        })
                    }
                }
            } else {
                None
            }
        })
        .collect()
}

// =============================================================================
// Rating ↔ energy (ADR-034)
// =============================================================================

/// A Rekordbox star rating, 0 to 5 (0 is unrated). Pioneer vocabulary: it stays out
/// of the library, where the same thing is energy 1..10.
///
/// Every comparison of a rating with an energy happens in star space: star to energy
/// to star is the identity, which is what makes a round-trip stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Stars(u8);

/// A number that is not 0 to 5 stars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotStars(pub u8);

impl std::fmt::Display for NotStars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not 0 to 5 stars", self.0)
    }
}

impl std::error::Error for NotStars {}

impl TryFrom<u8> for Stars {
    type Error = NotStars;

    fn try_from(stars: u8) -> Result<Self, NotStars> {
        Stars::new(stars).ok_or(NotStars(stars))
    }
}

impl From<Stars> for u8 {
    fn from(stars: Stars) -> u8 {
        stars.0
    }
}

/// The distance between two ratings Rekordbox writes: 0, 51, 102, 153, 204, 255.
const RATING_PER_STAR: u32 = 51;

impl Stars {
    /// Unrated.
    pub const NONE: Stars = Stars(0);

    /// `Some` for 0..=5.
    pub fn new(stars: u8) -> Option<Stars> {
        (stars <= 5).then_some(Stars(stars))
    }

    pub fn value(self) -> u8 {
        self.0
    }

    /// The stars of a `Rating` attribute: `round(r / 51)`. A rating above 255 is not one
    /// Rekordbox writes: it warns on stderr and counts as unrated.
    pub fn from_rating(rating: u32) -> Stars {
        if rating > 5 * RATING_PER_STAR {
            eprintln!("warning: rekordbox rating {rating} is above 255; counted as unrated");
            return Stars::NONE;
        }
        // Integer round(r / 51); the halfway points are not whole numbers, so no tie.
        Stars(((rating + RATING_PER_STAR / 2) / RATING_PER_STAR) as u8)
    }

    /// The `Rating` attribute for these stars: `s × 51`.
    pub fn to_rating(self) -> u32 {
        u32::from(self.0) * RATING_PER_STAR
    }

    /// The stars of an energy: `ceil(e / 2)`; no energy is unrated.
    pub fn from_energy(energy: Option<EnergyLevel>) -> Stars {
        Stars(energy.map_or(0, |e| e.value().div_ceil(2)))
    }

    /// The energy of these stars: `2s`; unrated is no energy.
    pub fn to_energy(self) -> Option<EnergyLevel> {
        EnergyLevel::new(self.0 * 2).ok()
    }
}

impl std::fmt::Display for Stars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}★", self.0)
    }
}

/// What an import does with the energy of a track (ADR-034 §2): `base` is the stars last
/// exchanged (none counts as 0), `theirs` the rating in Rekordbox now, `ours` the stars
/// of the energy now.
///
/// The three-way merge, local wins, except that a local clear gives way: with `ours`
/// unrated a changed rating is taken.
pub fn decide(base: Stars, theirs: Stars, ours: Stars) -> Decision<Stars> {
    match merge3_scalar(base, theirs, ours) {
        Decision::Conflict if ours == Stars::NONE => Decision::Take(theirs),
        decision => decision,
    }
}

// =============================================================================
// Private helpers
// =============================================================================

fn secs_to_ms(seconds: f64) -> u32 {
    (seconds * 1000.0).round() as u32
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── build_hash_to_location ────────────────────────────────────────────────

    fn hl(hash: &str, location: &str) -> HashLocation {
        HashLocation {
            hash: ContentHash::new(hash),
            location: location.to_string(),
        }
    }

    /// Full `sha256:...` hash key must resolve in the map.
    #[test]
    fn build_hash_to_location_full_hash_resolves() {
        let entries = vec![hl(
            "sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011",
            "file://localhost/tmp/export/polyrytmi.aiff",
        )];
        let map = build_hash_to_location(&entries);
        assert_eq!(
            map.get("sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011"),
            Some(&"file://localhost/tmp/export/polyrytmi.aiff".to_string())
        );
    }

    /// 12-char short hex key (0.18.1+ canonical playlist format) must resolve.
    #[test]
    fn build_hash_to_location_short12_resolves() {
        let entries = vec![hl(
            "sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011",
            "file://localhost/tmp/export/silverhaze.aiff",
        )];
        let map = build_hash_to_location(&entries);
        assert_eq!(
            map.get("abcdef123456"),
            Some(&"file://localhost/tmp/export/silverhaze.aiff".to_string())
        );
    }

    /// 8-char short hex key (legacy playlist format) must resolve.
    #[test]
    fn build_hash_to_location_short8_resolves() {
        let entries = vec![hl(
            "sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011",
            "file://localhost/tmp/export/track.aiff",
        )];
        let map = build_hash_to_location(&entries);
        assert_eq!(
            map.get("abcdef12"),
            Some(&"file://localhost/tmp/export/track.aiff".to_string())
        );
    }

    /// Two tracks, playlists mixing full and 12-char short hashes — all must resolve.
    #[test]
    fn build_hash_to_location_two_tracks_mixed_formats() {
        let entries = vec![
            hl(
                "sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011",
                "file://localhost/tmp/export/polyrytmi.aiff",
            ),
            hl(
                "sha256:fedcba9876543210aabbccddeeff00112233445566778899aabbccddeeff0022",
                "file://localhost/tmp/export/silverhaze.aiff",
            ),
        ];
        let map = build_hash_to_location(&entries);

        assert_eq!(
            map.get("sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011"),
            Some(&"file://localhost/tmp/export/polyrytmi.aiff".to_string()),
            "full hash for polyrytmi must resolve"
        );
        assert_eq!(
            map.get("fedcba987654"),
            Some(&"file://localhost/tmp/export/silverhaze.aiff".to_string()),
            "12-char short hash for silverhaze must resolve"
        );
        assert_eq!(
            map.get("abcdef12"),
            Some(&"file://localhost/tmp/export/polyrytmi.aiff".to_string()),
            "8-char legacy hash for polyrytmi must resolve"
        );
        assert_eq!(
            map.get("fedcba98"),
            Some(&"file://localhost/tmp/export/silverhaze.aiff".to_string()),
            "8-char legacy hash for silverhaze must resolve"
        );
    }

    /// Two tracks sharing the same 8-char prefix: first writer wins, both full hashes resolve.
    #[test]
    fn build_hash_to_location_short_hash_collision_first_wins() {
        let entries = vec![
            hl(
                "sha256:aabbccdd11111111aabbccddeeff00112233445566778899aabbccddeeff0011",
                "file://localhost/tmp/export/track_a.aiff",
            ),
            hl(
                "sha256:aabbccdd22222222aabbccddeeff00112233445566778899aabbccddeeff0022",
                "file://localhost/tmp/export/track_b.aiff",
            ),
        ];
        let map = build_hash_to_location(&entries);

        assert_eq!(
            map.get("sha256:aabbccdd11111111aabbccddeeff00112233445566778899aabbccddeeff0011"),
            Some(&"file://localhost/tmp/export/track_a.aiff".to_string()),
            "full hash for track_a must resolve"
        );
        assert_eq!(
            map.get("sha256:aabbccdd22222222aabbccddeeff00112233445566778899aabbccddeeff0022"),
            Some(&"file://localhost/tmp/export/track_b.aiff".to_string()),
            "full hash for track_b must resolve"
        );
        // Collision on 8-char prefix: first writer (track_a) wins
        assert_eq!(
            map.get("aabbccdd"),
            Some(&"file://localhost/tmp/export/track_a.aiff".to_string()),
            "8-char collision: first-inserted track must win"
        );
    }

    // ── location_to_hash ──────────────────────────────────────────────────────

    #[test]
    fn location_to_hash_basic() {
        let entries = vec![hl("sha256:abc123", "file://localhost/music/track.aiff")];
        let map = location_to_hash(&entries);
        assert_eq!(
            map.get("file://localhost/music/track.aiff"),
            Some(&"sha256:abc123".to_string())
        );
    }

    #[test]
    fn location_to_hash_multiple_entries() {
        let entries = vec![
            hl("sha256:aaa", "file://localhost/a.aiff"),
            hl("sha256:bbb", "file://localhost/b.aiff"),
        ];
        let map = location_to_hash(&entries);
        assert_eq!(
            map.get("file://localhost/a.aiff"),
            Some(&"sha256:aaa".to_string())
        );
        assert_eq!(
            map.get("file://localhost/b.aiff"),
            Some(&"sha256:bbb".to_string())
        );
        assert_eq!(map.len(), 2);
    }

    // ── grid_from_tempo ───────────────────────────────────────────────────────

    #[test]
    fn grid_from_tempo_seconds_to_ms_basic() {
        let bpm = Bpm::from_f32(128.0).unwrap();
        let anchor = TempoAnchor {
            inizio_seconds: 0.025,
        };
        let grid = grid_from_tempo(&anchor, bpm);
        assert_eq!(
            grid,
            MusicValue::BeatGrid {
                first_beat_ms: 25,
                bpm,
                beats_per_bar: 4,
            }
        );
    }

    /// Confirm rounding (not truncation): 1.0009s * 1000 ≈ 1000.9 → 1001ms
    ///
    /// With plain `as u32` truncation this would yield 1000; `.round()` gives 1001.
    /// The value 1.0009 is chosen so that even with IEEE 754 floating-point error
    /// the result is comfortably above 1000.5 (unambiguous: not a borderline case).
    #[test]
    fn grid_from_tempo_rounds_not_truncates() {
        let bpm = Bpm::from_f32(140.0).unwrap();
        let anchor = TempoAnchor {
            inizio_seconds: 1.0009_f64,
        };
        let grid = grid_from_tempo(&anchor, bpm);
        if let MusicValue::BeatGrid { first_beat_ms, .. } = grid {
            assert_eq!(
                first_beat_ms, 1001,
                "1.0009s should round to 1001ms, not truncate to 1000ms"
            );
        } else {
            panic!("expected BeatGrid");
        }
    }

    #[test]
    fn grid_from_tempo_zero_anchor() {
        let bpm = Bpm::from_f32(120.0).unwrap();
        let anchor = TempoAnchor {
            inizio_seconds: 0.0,
        };
        let grid = grid_from_tempo(&anchor, bpm);
        if let MusicValue::BeatGrid {
            first_beat_ms,
            beats_per_bar,
            ..
        } = grid
        {
            assert_eq!(first_beat_ms, 0);
            assert_eq!(beats_per_bar, 4, "always 4/4");
        } else {
            panic!("expected BeatGrid");
        }
    }

    #[test]
    fn grid_from_tempo_large_offset() {
        let bpm = Bpm::from_f32(132.0).unwrap();
        let anchor = TempoAnchor {
            inizio_seconds: 1.5,
        };
        let grid = grid_from_tempo(&anchor, bpm);
        if let MusicValue::BeatGrid { first_beat_ms, .. } = grid {
            assert_eq!(first_beat_ms, 1500);
        } else {
            panic!("expected BeatGrid");
        }
    }

    // ── cues_from_position_marks ──────────────────────────────────────────────

    #[test]
    fn cues_memory_cue_num_minus_one() {
        let marks = vec![PositionMark {
            name: "Intro".to_string(),
            mark_type: 0,
            start_seconds: 5.5,
            end_seconds: None,
            num: -1,
        }];
        let cues = cues_from_position_marks(&marks);
        assert_eq!(cues.len(), 1);
        assert_eq!(
            cues[0],
            MusicValue::MemoryCue {
                position_ms: 5500,
                kind: CueKind::Memory,
                label: Some("Intro".to_string()),
                index: None,
            }
        );
    }

    #[test]
    fn cues_hot_cue_num_zero_to_seven() {
        let marks = vec![PositionMark {
            name: "Drop".to_string(),
            mark_type: 0,
            start_seconds: 62.0,
            end_seconds: None,
            num: 0,
        }];
        let cues = cues_from_position_marks(&marks);
        assert_eq!(
            cues[0],
            MusicValue::MemoryCue {
                position_ms: 62000,
                kind: CueKind::Hot,
                label: Some("Drop".to_string()),
                index: Some(0),
            }
        );
    }

    #[test]
    fn cues_hot_cue_highest_slot() {
        let marks = vec![PositionMark {
            name: String::new(),
            mark_type: 0,
            start_seconds: 10.0,
            end_seconds: None,
            num: 7,
        }];
        let cues = cues_from_position_marks(&marks);
        assert_eq!(
            cues[0],
            MusicValue::MemoryCue {
                position_ms: 10000,
                kind: CueKind::Hot,
                label: None,
                index: Some(7),
            }
        );
    }

    #[test]
    fn cues_empty_name_becomes_none_label() {
        let marks = vec![PositionMark {
            name: String::new(),
            mark_type: 0,
            start_seconds: 0.0,
            end_seconds: None,
            num: -1,
        }];
        let cues = cues_from_position_marks(&marks);
        if let MusicValue::MemoryCue { label, .. } = &cues[0] {
            assert_eq!(*label, None, "empty Name must produce label=None");
        } else {
            panic!("expected MemoryCue");
        }
    }

    #[test]
    fn cues_loop_type4_with_end() {
        let marks = vec![PositionMark {
            name: "Chorus".to_string(),
            mark_type: 4,
            start_seconds: 122.0,
            end_seconds: Some(124.0),
            num: 1,
        }];
        let cues = cues_from_position_marks(&marks);
        assert_eq!(
            cues[0],
            MusicValue::MemoryCue {
                position_ms: 122000,
                kind: CueKind::Loop { length_ms: 2000 },
                label: Some("Chorus".to_string()),
                index: Some(1),
            }
        );
    }

    #[test]
    fn cues_loop_memory_loop_num_minus_one() {
        let marks = vec![PositionMark {
            name: String::new(),
            mark_type: 4,
            start_seconds: 32.0,
            end_seconds: Some(34.0),
            num: -1,
        }];
        let cues = cues_from_position_marks(&marks);
        if let MusicValue::MemoryCue { kind, index, .. } = &cues[0] {
            assert_eq!(*kind, CueKind::Loop { length_ms: 2000 });
            assert_eq!(*index, None, "num=-1 means memory loop → index=None");
        } else {
            panic!("expected MemoryCue");
        }
    }

    #[test]
    fn cues_loop_type4_without_end_skipped() {
        // Loop without end_seconds is invalid — must be skipped
        let marks = vec![PositionMark {
            name: "Bad Loop".to_string(),
            mark_type: 4,
            start_seconds: 10.0,
            end_seconds: None, // missing End — skip
            num: 0,
        }];
        let cues = cues_from_position_marks(&marks);
        assert!(cues.is_empty(), "loop without end should be skipped");
    }

    #[test]
    fn cues_unknown_type_skipped() {
        // Type=1 (phrase marker in newer Rekordbox) must be silently dropped
        let marks = vec![
            PositionMark {
                name: "Phrase".to_string(),
                mark_type: 1,
                start_seconds: 0.0,
                end_seconds: None,
                num: 0,
            },
            PositionMark {
                name: "Real".to_string(),
                mark_type: 0,
                start_seconds: 5.0,
                end_seconds: None,
                num: -1,
            },
        ];
        let cues = cues_from_position_marks(&marks);
        assert_eq!(cues.len(), 1, "only the Type=0 mark should survive");
        if let MusicValue::MemoryCue { label, .. } = &cues[0] {
            assert_eq!(label.as_deref(), Some("Real"));
        }
    }

    #[test]
    fn cues_seconds_to_ms_rounding() {
        // 0.025s → 25ms (not 24ms via truncation)
        let marks = vec![PositionMark {
            name: String::new(),
            mark_type: 0,
            start_seconds: 0.025,
            end_seconds: None,
            num: -1,
        }];
        let cues = cues_from_position_marks(&marks);
        if let MusicValue::MemoryCue { position_ms, .. } = &cues[0] {
            assert_eq!(*position_ms, 25);
        }
    }

    // ── position_marks_from_cues ──────────────────────────────────────────────

    #[test]
    fn marks_memory_cue_produces_num_minus_one() {
        let cues = vec![MusicValue::MemoryCue {
            position_ms: 5500,
            kind: CueKind::Memory,
            label: Some("Intro".to_string()),
            index: None,
        }];
        let marks = position_marks_from_cues(&cues);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].mark_type, 0);
        assert_eq!(marks[0].num, -1);
        assert!((marks[0].start_seconds - 5.5).abs() < 1e-9);
        assert_eq!(marks[0].name, "Intro");
    }

    #[test]
    fn marks_hot_cue_produces_num_from_index() {
        let cues = vec![MusicValue::MemoryCue {
            position_ms: 62000,
            kind: CueKind::Hot,
            label: Some("Drop".to_string()),
            index: Some(3),
        }];
        let marks = position_marks_from_cues(&cues);
        assert_eq!(marks[0].num, 3);
        assert_eq!(marks[0].mark_type, 0);
        assert!(marks[0].end_seconds.is_none());
    }

    #[test]
    fn marks_loop_produces_type4_with_end() {
        let cues = vec![MusicValue::MemoryCue {
            position_ms: 32000,
            kind: CueKind::Loop { length_ms: 4000 },
            label: None,
            index: Some(1),
        }];
        let marks = position_marks_from_cues(&cues);
        assert_eq!(marks[0].mark_type, 4);
        assert_eq!(marks[0].num, 1);
        assert!((marks[0].start_seconds - 32.0).abs() < 1e-9);
        let end = marks[0].end_seconds.expect("loop must have end");
        assert!((end - 36.0).abs() < 1e-9, "end should be 32+4=36s");
    }

    #[test]
    fn marks_memory_loop_num_minus_one() {
        let cues = vec![MusicValue::MemoryCue {
            position_ms: 8000,
            kind: CueKind::Loop { length_ms: 2000 },
            label: None,
            index: None,
        }];
        let marks = position_marks_from_cues(&cues);
        assert_eq!(marks[0].num, -1, "memory loop must have Num=-1");
    }

    #[test]
    fn marks_none_label_produces_empty_name() {
        let cues = vec![MusicValue::MemoryCue {
            position_ms: 0,
            kind: CueKind::Memory,
            label: None,
            index: None,
        }];
        let marks = position_marks_from_cues(&cues);
        assert_eq!(marks[0].name, "");
    }

    #[test]
    fn marks_non_memory_cue_variant_skipped() {
        let cues = vec![
            MusicValue::Bpm(Bpm::from_f32(128.0).unwrap()),
            MusicValue::MemoryCue {
                position_ms: 1000,
                kind: CueKind::Memory,
                label: None,
                index: None,
            },
        ];
        let marks = position_marks_from_cues(&cues);
        assert_eq!(marks.len(), 1, "non-MemoryCue variant must be skipped");
    }

    // ── golden round-trip: cues → marks → cues ────────────────────────────────

    #[test]
    fn round_trip_cues_marks_cues() {
        let original_cues = vec![
            MusicValue::MemoryCue {
                position_ms: 5500,
                kind: CueKind::Memory,
                label: Some("Intro".to_string()),
                index: None,
            },
            MusicValue::MemoryCue {
                position_ms: 62000,
                kind: CueKind::Hot,
                label: Some("Drop".to_string()),
                index: Some(0),
            },
            MusicValue::MemoryCue {
                position_ms: 122000,
                kind: CueKind::Loop { length_ms: 2000 },
                label: Some("Chorus".to_string()),
                index: Some(1),
            },
            MusicValue::MemoryCue {
                position_ms: 32000,
                kind: CueKind::Loop { length_ms: 8000 },
                label: None,
                index: None, // memory loop
            },
        ];

        let marks = position_marks_from_cues(&original_cues);
        let roundtripped = cues_from_position_marks(&marks);

        assert_eq!(
            roundtripped, original_cues,
            "cues → marks → cues must be lossless"
        );
    }

    // ── Stars ─────────────────────────────────────────────────────────────────

    use proptest::prelude::*;
    use rstest::rstest;

    fn stars(n: u8) -> Stars {
        Stars::new(n).unwrap()
    }

    fn energy(n: u8) -> Option<EnergyLevel> {
        Some(EnergyLevel::new(n).unwrap())
    }

    #[rstest]
    #[case::unrated(0, 0)]
    #[case::just_below_half_a_star(25, 0)]
    #[case::just_above_half_a_star(26, 1)]
    #[case::one_star(51, 1)]
    #[case::just_below_one_and_a_half(76, 1)]
    #[case::just_above_one_and_a_half(77, 2)]
    #[case::two_stars(102, 2)]
    #[case::three_stars(153, 3)]
    #[case::four_stars(204, 4)]
    #[case::five_stars(255, 5)]
    #[case::too_high_counts_as_unrated(256, 0)]
    #[case::far_too_high(u32::MAX, 0)]
    fn a_rating_is_the_nearest_star(#[case] rating: u32, #[case] expected: u8) {
        assert_eq!(Stars::from_rating(rating), stars(expected));
    }

    #[rstest]
    #[case(0, 0)]
    #[case(1, 51)]
    #[case(2, 102)]
    #[case(3, 153)]
    #[case(4, 204)]
    #[case(5, 255)]
    fn stars_are_written_as_a_rating(#[case] s: u8, #[case] rating: u32) {
        assert_eq!(stars(s).to_rating(), rating);
    }

    #[rstest]
    #[case(0, None)]
    #[case(1, Some(2))]
    #[case(2, Some(4))]
    #[case(3, Some(6))]
    #[case(4, Some(8))]
    #[case(5, Some(10))]
    fn stars_are_twice_the_energy(#[case] s: u8, #[case] expected: Option<u8>) {
        assert_eq!(stars(s).to_energy(), expected.and_then(energy));
    }

    #[rstest]
    #[case(None, 0)]
    #[case(Some(1), 1)]
    #[case(Some(2), 1)]
    #[case(Some(3), 2)]
    #[case(Some(4), 2)]
    #[case(Some(7), 4)]
    #[case(Some(8), 4)]
    #[case(Some(9), 5)]
    #[case(Some(10), 5)]
    fn an_energy_is_rounded_up_to_stars(#[case] e: Option<u8>, #[case] expected: u8) {
        assert_eq!(Stars::from_energy(e.and_then(energy)), stars(expected));
    }

    #[test]
    fn stars_beyond_five_do_not_exist() {
        assert_eq!(Stars::new(6), None);
        assert_eq!(stars(4).to_string(), "4★");
    }

    proptest! {
        /// Star to energy to star is the identity, so comparing in star space is exact.
        #[test]
        fn star_to_energy_to_star_is_the_identity(s in 0u8..=5) {
            prop_assert_eq!(Stars::from_energy(stars(s).to_energy()), stars(s));
        }

        /// Rating to stars to rating is the identity on what Rekordbox writes.
        #[test]
        fn stars_to_rating_to_stars_is_the_identity(s in 0u8..=5) {
            prop_assert_eq!(Stars::from_rating(stars(s).to_rating()), stars(s));
        }

        /// Export, then re-import with nothing changed: the energy is kept, whichever
        /// of 1..10 it is.
        #[test]
        fn export_then_reimport_keeps_every_energy(e in 1u8..=10) {
            let ours = Stars::from_energy(energy(e));
            let exported = ours.to_rating();
            let base = Stars::from_rating(exported); // export sets base := what it wrote
            let theirs = Stars::from_rating(exported);
            prop_assert_eq!(decide(base, theirs, ours), Decision::Keep);
        }

        /// An unrated local never conflicts: it gives way. The import relies on this to
        /// know that a conflict always has a local energy to report.
        #[test]
        fn an_unrated_local_never_conflicts(b in 0u8..=5, t in 0u8..=5) {
            prop_assert_ne!(decide(stars(b), stars(t), Stars::NONE), Decision::Conflict);
        }

        /// Merging again with base advanced to theirs never changes anything.
        #[test]
        fn deciding_twice_with_base_advanced_keeps(b in 0u8..=5, t in 0u8..=5, o in 0u8..=5) {
            let (b, t, o) = (stars(b), stars(t), stars(o));
            let after = match decide(b, t, o) {
                Decision::Take(v) => v,
                _ => o,
            };
            prop_assert_eq!(decide(t, t, after), Decision::Keep);
        }
    }

    // ── decide: one test per row of the table of ADR-034 ──────────────────────

    #[rstest]
    #[case::theirs_unchanged_ours_unchanged(2, 2, 2, Decision::Keep)]
    #[case::theirs_unchanged_ours_edited(2, 2, 4, Decision::Keep)]
    #[case::theirs_unchanged_ours_cleared(2, 2, 0, Decision::Keep)]
    #[case::theirs_changed_ours_unchanged(2, 4, 2, Decision::Take(stars(4)))]
    #[case::theirs_cleared_ours_unchanged(2, 0, 2, Decision::Take(stars(0)))]
    #[case::theirs_changed_ours_cleared_gives_way(2, 4, 0, Decision::Take(stars(4)))]
    #[case::theirs_changed_to_what_ours_is(2, 4, 4, Decision::Converged)]
    #[case::theirs_cleared_and_ours_cleared(2, 0, 0, Decision::Converged)]
    #[case::both_changed_differently(2, 4, 3, Decision::Conflict)]
    #[case::theirs_cleared_ours_edited(2, 0, 3, Decision::Conflict)]
    // No base counts as 0 stars, with no special case.
    #[case::no_base_unrated_both(0, 0, 0, Decision::Keep)]
    #[case::no_base_unrated_theirs_keeps_ours(0, 0, 5, Decision::Keep)]
    #[case::no_base_rated_theirs_ours_without_energy(0, 3, 0, Decision::Take(stars(3)))]
    #[case::no_base_rated_theirs_ours_same(0, 3, 3, Decision::Converged)]
    #[case::no_base_rated_theirs_ours_different(0, 3, 5, Decision::Conflict)]
    fn the_decision_table(
        #[case] base: u8,
        #[case] theirs: u8,
        #[case] ours: u8,
        #[case] expected: Decision<Stars>,
    ) {
        assert_eq!(decide(stars(base), stars(theirs), stars(ours)), expected);
    }
}
