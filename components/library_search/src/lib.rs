//! Library search: composable, type-safe query system for track filtering.
//!
//! Usable from CLI, web UI, and any future interface.
//!
//! # Quick start
//!
//! ```rust
//! use library_search::{TrackQuery, TrackFields, StringQuery, NumericQuery, matches_query};
//!
//! let query = TrackQuery {
//!     artist: Some(StringQuery::Contains("carbon based".to_string())),
//!     bpm: Some(NumericQuery::Tolerance { value: 128.0, up: 4.0, down: 4.0 }),
//!     ..Default::default()
//! };
//!
//! let fields = TrackFields {
//!     artist: Some("Carbon Based Lifeforms"),
//!     bpm: Some(128.5),
//!     title: None, album: None, label: None, genre: None,
//!     styles: &[], key: None, duration: None, year: None, source: None,
//!     last_started: None, last_stopped: None, added: None,
//! };
//!
//! assert!(matches_query(&query, &fields));
//! ```

pub mod eval;
pub mod parse;
pub mod query;

pub use eval::{matches_query, TrackFields};
pub use parse::{
    parse_date_query, parse_duration_query, parse_key_query, parse_numeric_query,
    parse_string_query, ParseError,
};
pub use query::{
    CamelotLetter, DatePrecision, DateQuery, DurationQuery, DurationUnit, KeyQuery, NumericQuery,
    StringQuery, TrackQuery,
};
