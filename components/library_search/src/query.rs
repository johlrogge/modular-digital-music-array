use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Search query for tracks. Implicit AND across all non-None fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrackQuery {
    /// Searches all text fields (title, artist, album, label, genre) — OR across fields.
    pub any_text: Option<StringQuery>,
    pub artist: Option<StringQuery>,
    pub title: Option<StringQuery>,
    pub album: Option<StringQuery>,
    pub label: Option<StringQuery>,
    /// MainGenre field
    pub genre: Option<StringQuery>,
    /// StyleDescriptor field — matches if any descriptor matches
    pub style: Option<StringQuery>,
    pub bpm: Option<NumericQuery>,
    pub key: Option<KeyQuery>,
    pub duration: Option<DurationQuery>,
    pub year: Option<NumericQuery>,
    /// e.g. "bandcamp", "beatport", "upload"
    pub source: Option<String>,
    pub played: Option<PlayedQuery>,
    pub skipped: Option<PlayedQuery>,
}

impl TrackQuery {
    /// Returns true if no filter fields are set (the query would match everything).
    pub fn is_empty(&self) -> bool {
        self.any_text.is_none()
            && self.artist.is_none()
            && self.title.is_none()
            && self.album.is_none()
            && self.label.is_none()
            && self.genre.is_none()
            && self.style.is_none()
            && self.bpm.is_none()
            && self.key.is_none()
            && self.duration.is_none()
            && self.year.is_none()
            && self.source.is_none()
            && self.played.is_none()
            && self.skipped.is_none()
    }
}

/// String field matching — parsed from a CLI argument.
///
/// Detection rules in `parse_string_query`:
/// 1. Starts and ends with `/` → `Regex` (slashes stripped)
/// 2. No spaces AND 2+ uppercase letters AND not all-caps → `Initialism`
/// 3. Otherwise → `Contains`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StringQuery {
    /// All words present in the field, any order (case-insensitive).
    Contains(String),
    /// CamelCase initialism: each segment is a prefix of successive words.
    /// e.g. "CarbBased" matches "Carbon Based Lifeforms".
    Initialism(String),
    /// Regex pattern (without surrounding slashes).
    Regex(String),
}

/// Numeric field matching (BPM, Year; f32 covers both).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NumericQuery {
    Exact(f32),
    /// `"125..130"` → \[125, 130\] inclusive
    Range(f32, f32),
    /// `"128+-2"` → up: 2, down: 2 → \[126, 130\]
    /// `"128+2"`  → up: 2, down: 0 → \[128, 130\]
    /// `"128-2"`  → up: 0, down: 2 → \[126, 128\]
    Tolerance {
        value: f32,
        up: f32,
        down: f32,
    },
}

/// Duration matching with precision semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DurationQuery {
    /// `"7m15s"` → exact 435 seconds
    Exact(u32),
    /// `">7m"` → >= 420 seconds
    AtLeast(u32),
    /// `"<7m"` → < 420 seconds
    AtMost(u32),
    /// `"6m30s..7m30s"` → \[390, 450\]
    Range(u32, u32),
    /// `"7m"` → \[420, 480\) — named-unit precision bucket
    WithPrecision(u32, DurationUnit),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DurationUnit {
    Hours,
    Minutes,
    Seconds,
}

/// Musical key matching — always resolved to Camelot internally for tolerance math.
/// Traditional notation ("Am", "A minor") is converted to Camelot at parse time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyQuery {
    /// Exact Camelot position. Parsed from "8B", "Am", "A minor".
    Exact { number: u8, letter: CamelotLetter },
    /// Asymmetric tolerance on the Camelot number circle.
    /// `"8B+-1"` → up: 1, down: 1 → {7B, 8B, 9B}
    /// `"8B+1"`  → up: 1, down: 0 → {8B, 9B}
    /// `"8B-1"`  → up: 0, down: 1 → {7B, 8B}
    /// `"8B+-1~"` → up: 1, down: 1, include_relative → {7A, 7B, 8A, 8B, 9A, 9B}
    Tolerance {
        number: u8,
        letter: CamelotLetter,
        tolerance_up: u8,
        tolerance_down: u8,
        /// `~` suffix: also include the relative key (cross the A/B boundary)
        include_relative: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CamelotLetter {
    A,
    B,
}

/// Date precision used in played/skipped queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatePrecision {
    /// Only the year is significant.
    Year,
    /// Year + month.
    YearMonth,
    /// Year + month + day (exact).
    YearMonthDay,
}

/// Played/skipped date matching.
///
/// `NA` matches tracks that have never been played/skipped.
/// Bare date tokens are stored as `Range(date, prec, date, prec)` and the evaluator
/// expands them to [start-of-period, end-of-period] using the precision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayedQuery {
    /// Track has never been played/skipped (field is `None`)
    NA,
    /// `>2026-02` — strictly after the end of the given period
    After(NaiveDate, DatePrecision),
    /// `<2026-02` — strictly before the start of the given period
    Before(NaiveDate, DatePrecision),
    /// `2026-01..2026-06` — within [start-of-lo, end-of-hi]
    Range(NaiveDate, DatePrecision, NaiveDate, DatePrecision),
}
