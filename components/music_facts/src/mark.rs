//! Marks: the user's labels on a track, stored as `MusicValue::Bookmarked` facts
//! (ADR-016). The fact schema is unchanged; a mark is a typed view of the
//! `scope` field. `scope: None` is the default mark, `Some(name)` a named mark.

use crate::MusicValue;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The one home of the default mark's name. Also the one name a `MarkName`
/// may not take, so the default mark has a single spelling.
const DEFAULT_MARK_NAME: &str = "liked";

const MAX_MARK_NAME_CHARS: usize = 64;

/// A named mark: trimmed, lowercased, 1..=64 characters, no control or
/// invisible characters, and not the default mark's name.
///
/// - Names are lowercased with `str::to_lowercase`. That is not Unicode case
///   folding and not NFC normalisation.
/// - Names are trimmed; internal whitespace is kept, so "big  banger" and
///   "big banger" are different names.
/// - The length limit is counted in `char`s after lowercasing, which can
///   lengthen a name (e.g. "İ" lowercases to two chars).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MarkName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MarkNameError {
    #[error("mark name is empty")]
    Empty,
    #[error("mark name is longer than {max} characters")]
    TooLong { max: usize },
    #[error("mark name contains a control or invisible character")]
    InvisibleCharacter,
    #[error("\"{DEFAULT_MARK_NAME}\" is the default mark and can't be used as a name")]
    Reserved,
}

/// Control characters (Cc) and the common Unicode format characters (Cf):
/// soft hyphen, zero-width and directional marks, bidi controls, word joiner
/// and invisible operators, and the byte order mark.
fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

impl MarkName {
    pub fn parse(s: &str) -> Result<Self, MarkNameError> {
        let folded = s.trim().to_lowercase();
        if folded.is_empty() {
            return Err(MarkNameError::Empty);
        }
        if folded.chars().any(is_invisible) {
            return Err(MarkNameError::InvisibleCharacter);
        }
        if folded.chars().count() > MAX_MARK_NAME_CHARS {
            return Err(MarkNameError::TooLong {
                max: MAX_MARK_NAME_CHARS,
            });
        }
        if folded == DEFAULT_MARK_NAME {
            return Err(MarkNameError::Reserved);
        }
        Ok(Self(folded))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for MarkName {
    type Err = MarkNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for MarkName {
    type Error = MarkNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<MarkName> for String {
    fn from(name: MarkName) -> Self {
        name.0
    }
}

impl fmt::Display for MarkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A mark: the default one ("liked") or a named one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mark {
    Default,
    Named(MarkName),
}

impl Mark {
    /// The `Bookmarked.scope` this mark is stored as.
    pub fn to_scope(&self) -> Option<String> {
        match self {
            Mark::Default => None,
            Mark::Named(name) => Some(name.as_str().to_owned()),
        }
    }

    /// Reads a stored scope: `None` is the default mark, and so is the
    /// reserved name "liked".
    pub fn from_scope(scope: Option<&str>) -> Result<Self, MarkNameError> {
        scope.map_or(Ok(Mark::Default), str::parse)
    }

    /// The fact value that applies this mark.
    pub fn fact_value(&self, at: DateTime<Utc>) -> MusicValue {
        MusicValue::Bookmarked {
            scope: self.to_scope(),
            timestamp: at,
        }
    }

    /// Whether `value` is a bookmark carrying this mark. Legacy scopes never match.
    pub fn matches(&self, value: &MusicValue) -> bool {
        matches!(value.stored_mark(), Some(StoredMark::Mark(m)) if m == *self)
    }
}

impl FromStr for Mark {
    type Err = MarkNameError;

    /// A valid name is `Named`; the reserved name is the `Default` mark.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match MarkName::parse(s) {
            Ok(name) => Ok(Mark::Named(name)),
            Err(MarkNameError::Reserved) => Ok(Mark::Default),
            Err(e) => Err(e),
        }
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mark::Default => f.write_str(DEFAULT_MARK_NAME),
            Mark::Named(name) => name.fmt(f),
        }
    }
}

/// How a stored `Bookmarked.scope` reads: a proper mark, or a legacy free-text
/// scope written before marks existed (kept as-is, never matched by a `Mark`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredMark<'a> {
    Mark(Mark),
    Legacy(&'a str),
}

impl<'a> StoredMark<'a> {
    pub fn from_scope(scope: Option<&'a str>) -> Self {
        match Mark::from_scope(scope) {
            Ok(mark) => StoredMark::Mark(mark),
            // An error implies `scope` was `Some`.
            Err(_) => StoredMark::Legacy(scope.unwrap_or_default()),
        }
    }
}

impl MusicValue {
    /// The mark a `Bookmarked` fact carries; `None` for every other value.
    pub fn stored_mark(&self) -> Option<StoredMark<'_>> {
        match self {
            MusicValue::Bookmarked { scope, .. } => Some(StoredMark::from_scope(scope.as_deref())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn named(s: &str) -> Mark {
        Mark::Named(MarkName::parse(s).unwrap())
    }

    fn bookmarked(scope: Option<&str>) -> MusicValue {
        MusicValue::Bookmarked {
            scope: scope.map(str::to_owned),
            timestamp: at(),
        }
    }

    #[rstest]
    #[case("Banger", "banger")]
    #[case(" opener ", "opener")]
    #[case("Å", "å")]
    fn names_are_trimmed_and_lowercased(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(MarkName::parse(input).unwrap().as_str(), expected);
    }

    #[test]
    fn sixty_four_chars_is_ok_and_counts_chars_not_bytes() {
        assert!(MarkName::parse(&"a".repeat(64)).is_ok());
        assert!(MarkName::parse(&"å".repeat(64)).is_ok());
        assert_eq!(
            MarkName::parse(&"a".repeat(65)),
            Err(MarkNameError::TooLong { max: 64 })
        );
    }

    #[rstest]
    #[case("", MarkNameError::Empty)]
    #[case("   ", MarkNameError::Empty)]
    #[case("a\nb", MarkNameError::InvisibleCharacter)]
    #[case("banger\u{200B}", MarkNameError::InvisibleCharacter)]
    #[case("a\u{202E}b", MarkNameError::InvisibleCharacter)]
    #[case("a\u{FEFF}b", MarkNameError::InvisibleCharacter)]
    #[case("a\u{2066}b", MarkNameError::InvisibleCharacter)]
    #[case("liked", MarkNameError::Reserved)]
    #[case("LIKED", MarkNameError::Reserved)]
    #[case(" Liked ", MarkNameError::Reserved)]
    fn invalid_names_are_rejected(#[case] input: &str, #[case] expected: MarkNameError) {
        assert_eq!(MarkName::parse(input), Err(expected));
    }

    #[test]
    fn error_messages_are_readable() {
        assert_eq!(MarkNameError::Empty.to_string(), "mark name is empty");
        assert_eq!(
            MarkNameError::TooLong { max: 64 }.to_string(),
            "mark name is longer than 64 characters"
        );
        assert_eq!(
            MarkNameError::InvisibleCharacter.to_string(),
            "mark name contains a control or invisible character"
        );
        assert_eq!(
            MarkNameError::Reserved.to_string(),
            "\"liked\" is the default mark and can't be used as a name"
        );
    }

    #[rstest]
    #[case("banger")]
    #[case("ΑΣ")]
    #[case("Å")]
    #[case("İ")]
    #[case("Straße")]
    fn parsing_is_idempotent(#[case] input: &str) {
        let name = MarkName::parse(input).unwrap();
        assert_eq!(MarkName::parse(name.as_str()), Ok(name));
    }

    #[test]
    fn length_is_counted_after_folding() {
        // "İ" lowercases to two chars, so 64 of them are 128.
        assert_eq!(
            MarkName::parse(&"İ".repeat(64)),
            Err(MarkNameError::TooLong { max: 64 })
        );
    }

    #[test]
    fn internal_whitespace_is_kept() {
        assert_ne!(
            MarkName::parse("big  banger").unwrap(),
            MarkName::parse("big banger").unwrap()
        );
    }

    #[test]
    fn marks_round_trip_through_display_and_parse() {
        assert_eq!(Mark::Default.to_string().parse::<Mark>(), Ok(Mark::Default));
        let banger = named("banger");
        assert_eq!(banger.to_string().parse::<Mark>(), Ok(banger));
    }

    #[test]
    fn from_scope_none_is_default() {
        assert_eq!(Mark::from_scope(None), Ok(Mark::Default));
        assert_eq!(Mark::from_scope(Some("Banger")), Ok(named("banger")));
        assert_eq!(Mark::from_scope(Some("")), Err(MarkNameError::Empty));
    }

    #[test]
    fn wire_format_is_pinned() {
        assert_eq!(
            serde_json::to_string(&Mark::Default).unwrap(),
            "\"Default\""
        );
        assert_eq!(
            serde_json::to_string(&named("banger")).unwrap(),
            r#"{"Named":"banger"}"#
        );
        assert_eq!(
            serde_json::from_str::<Mark>(r#"{"Named":"Banger"}"#).unwrap(),
            named("banger")
        );
    }

    #[rstest]
    #[case(r#"{"Named":""}"#)]
    #[case(r#"{"Named":"liked"}"#)]
    #[case(r#"{"Named":"a\nb"}"#)]
    fn deserialising_invalid_names_fails(#[case] json: &str) {
        assert!(serde_json::from_str::<Mark>(json).is_err());
    }

    #[test]
    fn deserialising_a_too_long_name_fails() {
        let json = format!(r#"{{"Named":"{}"}}"#, "a".repeat(65));
        assert!(serde_json::from_str::<Mark>(&json).is_err());
    }

    #[test]
    fn display_uses_the_default_name() {
        assert_eq!(Mark::Default.to_string(), "liked");
        assert_eq!(named("banger").to_string(), "banger");
    }

    #[test]
    fn default_matches_only_unscoped() {
        assert!(Mark::Default.matches(&bookmarked(None)));
        assert!(!Mark::Default.matches(&bookmarked(Some("banger"))));
        assert!(!Mark::Default.matches(&MusicValue::StyleDescriptor("x".into())));
    }

    #[test]
    fn stored_liked_scope_counts_as_default() {
        assert!(Mark::Default.matches(&bookmarked(Some("liked"))));
        assert_eq!(
            StoredMark::from_scope(Some("Liked")),
            StoredMark::Mark(Mark::Default)
        );
    }

    #[test]
    fn named_matches_case_insensitively_but_not_unscoped() {
        let banger = named("banger");
        assert!(banger.matches(&bookmarked(Some("Banger"))));
        assert!(!banger.matches(&bookmarked(None)));
        assert!(!banger.matches(&bookmarked(Some("opener"))));
    }

    #[test]
    fn legacy_scopes_match_nothing() {
        let legacy = bookmarked(Some(""));
        assert_eq!(legacy.stored_mark(), Some(StoredMark::Legacy("")));
        assert!(!Mark::Default.matches(&legacy));
        assert!(!named("banger").matches(&legacy));
    }

    #[test]
    fn legacy_scopes_that_are_not_valid_names() {
        let too_long = "a".repeat(65);
        assert_eq!(
            StoredMark::from_scope(Some(&too_long)),
            StoredMark::Legacy(&too_long)
        );
        assert_eq!(
            StoredMark::from_scope(Some("a\nb")),
            StoredMark::Legacy("a\nb")
        );
    }

    #[test]
    fn padded_scope_reads_as_the_trimmed_name() {
        assert_eq!(
            StoredMark::from_scope(Some(" banger ")),
            StoredMark::Mark(named("banger"))
        );
    }

    #[test]
    fn fact_value_builds_the_expected_value() {
        assert_eq!(Mark::Default.fact_value(at()), bookmarked(None));
        assert_eq!(named("banger").fact_value(at()), bookmarked(Some("banger")));
    }

    #[test]
    fn to_scope_is_the_stored_scope() {
        assert_eq!(named("banger").to_scope(), Some("banger".to_owned()));
        assert_eq!(Mark::Default.to_scope(), None);
    }
}
