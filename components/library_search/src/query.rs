use chrono::NaiveDate;
use music_primitives::{Key, TrackRole};
use serde::{Deserialize, Serialize};
use std::fmt;

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
    pub started: Option<DateQuery>,
    pub stopped: Option<DateQuery>,
    pub added: Option<DateQuery>,
    /// DJ curation role (Opener, BuildUp, Peak, Banger, CoolDown, Closer, Filler).
    /// Exact match; accepts the same string forms as `TrackRole::from_str`.
    #[serde(default)]
    pub role: Option<TrackRole>,
    /// Energy level query using the same numeric syntax as bpm/year (1..=10).
    /// e.g. `7`, `5..8`, `7+-2`
    #[serde(default)]
    pub energy: Option<NumericQuery>,
    /// When true, invert the match — return tracks that do NOT match the other filters.
    #[serde(default)]
    pub not: bool,
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
            && self.started.is_none()
            && self.stopped.is_none()
            && self.added.is_none()
            && self.role.is_none()
            && self.energy.is_none()
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

/// Musical key matching on the Camelot wheel. Any notation `Key` parses is accepted.
///
/// Every key's distance from the centre costs 1 per step around the wheel plus 1 for
/// an A/B (mode) switch; `+` goes clockwise (a fifth up), `-` counter-clockwise.
///
/// On the wire a query is its string form (`"8A+-2"`), the same text the CLI accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum KeyQuery {
    /// `centre` and how far from it to reach.
    Steps { centre: Key, reach: Reach },
    /// `"6A..10B"`: numbers run clockwise from `from` to `to` (inclusive, wrapping);
    /// letters are the set {from's, to's}.
    Span { from: Key, to: Key },
}

/// How many wheel steps a `KeyQuery::Steps` reaches from its centre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// `"8A+-2"`: 2 steps either way. `Both(0)` is the exact key, written `"8A"`.
    Both(u8),
    /// `"8A+2"`: 2 steps clockwise (a fifth up per step).
    Up(u8),
    /// `"8A-1"`: 1 step counter-clockwise.
    Down(u8),
}

impl Reach {
    /// Steps reachable (clockwise, counter-clockwise).
    fn up_down(self) -> (u8, u8) {
        match self {
            Reach::Both(n) => (n, n),
            Reach::Up(n) => (n, 0),
            Reach::Down(n) => (0, n),
        }
    }
}

impl KeyQuery {
    /// Whether `key` satisfies the query.
    pub fn matches(&self, key: Key) -> bool {
        match *self {
            KeyQuery::Steps { centre, reach } => {
                let (up, down) = reach.up_down();
                let cw = key.camelot_steps_from(centre);
                let ccw = centre.camelot_steps_from(key);
                let switch = u8::from(key.mode_differs(centre));
                cw + switch <= up || ccw + switch <= down
            }
            KeyQuery::Span { from, to } => {
                key.camelot_steps_from(from) <= to.camelot_steps_from(from)
                    && (key.mode() == from.mode() || key.mode() == to.mode())
            }
        }
    }
}

impl fmt::Display for KeyQuery {
    /// Canonical form that parses back to an equal query.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            KeyQuery::Steps { centre, reach } => {
                let c = centre.to_camelot();
                match reach {
                    Reach::Both(0) => write!(f, "{c}"),
                    Reach::Both(n) => write!(f, "{c}+-{n}"),
                    Reach::Up(n) => write!(f, "{c}+{n}"),
                    Reach::Down(n) => write!(f, "{c}-{n}"),
                }
            }
            KeyQuery::Span { from, to } => {
                write!(f, "{}..{}", from.to_camelot(), to.to_camelot())
            }
        }
    }
}

impl From<KeyQuery> for String {
    fn from(query: KeyQuery) -> String {
        query.to_string()
    }
}

/// Date precision used in started/stopped date queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatePrecision {
    /// Only the year is significant.
    Year,
    /// Year + month.
    YearMonth,
    /// Year + month + day (exact).
    YearMonthDay,
}

/// Date matching for track events (started/stopped).
///
/// `NA` matches tracks that have never been started/stopped.
/// Bare date tokens are stored as `Range(date, prec, date, prec)` and the evaluator
/// expands them to [start-of-period, end-of-period] using the precision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DateQuery {
    /// Track has never been started/stopped (field is `None`)
    NA,
    /// `>2026-02` — strictly after the end of the given period
    After(NaiveDate, DatePrecision),
    /// `<2026-02` — strictly before the start of the given period
    Before(NaiveDate, DatePrecision),
    /// `2026-01..2026-06` — within [start-of-lo, end-of-hi]
    Range(NaiveDate, DatePrecision, NaiveDate, DatePrecision),
}
