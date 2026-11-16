use crate::query::{
    DatePrecision, DateQuery, DurationQuery, DurationUnit, KeyQuery, NumericQuery, Reach,
    StringQuery,
};
use chrono::NaiveDate;
use music_primitives::TrackRole;
use std::convert::Infallible;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid numeric query '{0}': expected a number, 'lo..hi', 'val+-tol', 'val+tol', or 'val-tol'")]
    InvalidNumeric(String),
    #[error("invalid duration query '{0}': expected e.g. '7m15s', '7m', '>5m', '<8m', '6m..8m'")]
    InvalidDuration(String),
    #[error("invalid key query '{0}': expected a key (8B, 1m, Am, C# major), steps (8A+-2, 8A+2, 8A-1) or a span (6A..10B)")]
    InvalidKey(String),
    #[error("key query '{0}': the `~` suffix is gone; use steps like `8A+-2` (an A/B switch costs one step) or a span like `6A..10B`")]
    KeyTilde(String),
    #[error("invalid date query '{0}'")]
    InvalidDateQuery(String),
    #[error("invalid role '{0}': expected one of opener, build-up, peak, banger, cool-down, closer, filler (case-insensitive)")]
    InvalidRole(String),
}

impl ParseError {
    /// What was wrong, without the word that was typed: for callers that print the
    /// word themselves (an argument parser says ``couldn't parse `x`:`` first).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::InvalidNumeric(_) => {
                "expected a number, `lo..hi`, `val+-tol`, `val+tol` or `val-tol`"
            }
            Self::InvalidDuration(_) => {
                "expected a duration such as `7m15s`, `7m`, `>5m`, `<8m` or `6m..8m`"
            }
            Self::InvalidKey(_) => {
                "expected a key (`8B`, `1m`, `Am`, `C# major`), steps (`8A+-2`, `8A+2`, `8A-1`) or a span (`6A..10B`)"
            }
            Self::KeyTilde(_) => {
                "the `~` suffix is gone; use steps like `8A+-2` (an A/B switch costs one step) or a span like `6A..10B`"
            }
            Self::InvalidDateQuery(_) => {
                "expected a date such as `2026`, `2026-02`, `>2026-02` or `2026-01..2026-06`, a date expression, or `N/A`"
            }
            Self::InvalidRole(_) => {
                "expected one of opener, build-up, peak, banger, cool-down, closer, filler"
            }
        }
    }
}

/// Parsing a query from a word, with the bare reason as the error (see
/// [`ParseError::reason`]).
macro_rules! from_str_query {
    ($query:ty, $parse:ident) => {
        impl FromStr for $query {
            type Err = &'static str;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $parse(s).map_err(|e| e.reason())
            }
        }
    };
}

from_str_query!(NumericQuery, parse_numeric_query);
from_str_query!(DurationQuery, parse_duration_query);
from_str_query!(KeyQuery, parse_key_query);
from_str_query!(DateQuery, parse_date_query);

/// Lets `KeyQuery` serialise as its string form (`#[serde(try_from = "String")]`).
impl TryFrom<String> for KeyQuery {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        parse_key_query(&s)
    }
}

impl FromStr for StringQuery {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(parse_string_query(s))
    }
}

/// Splits `s` into `(base, up_str, down_str)` by detecting tolerance suffix patterns.
/// - `"val+-tol"` → symmetric tolerance
/// - `"val+tol"` → up-only tolerance
/// - `"val-tol"` → down-only tolerance
///
/// `min_offset`: minimum byte offset at which operators are searched
/// (use 1 for numeric to skip leading minus, 2 for "8B" Camelot notation).
fn split_tolerance(s: &str, min_offset: usize) -> Option<(&str, &str, &str)> {
    // Check symmetric "+-" first (before single "+")
    if let Some(idx) = s.find("+-") {
        return Some((&s[..idx], &s[idx + 2..], &s[idx + 2..]));
    }
    if s.len() > min_offset {
        if let Some(rel) = s[min_offset..].find('+') {
            let idx = rel + min_offset;
            return Some((&s[..idx], &s[idx + 1..], "0"));
        }
        if let Some(rel) = s[min_offset..].find('-') {
            let idx = rel + min_offset;
            return Some((&s[..idx], "0", &s[idx + 1..]));
        }
    }
    None
}

/// Parse a CLI string into a StringQuery.
///
/// Detection rules:
/// 1. Starts and ends with `/` → `Regex` (slashes stripped)
/// 2. No spaces AND 2+ uppercase letters → `Initialism`
///    (covers both CamelCase like "CarbBased" and all-caps like "CBL")
/// 3. Otherwise → `Contains`
pub fn parse_string_query(s: &str) -> StringQuery {
    if s.starts_with('/') && s.ends_with('/') && s.len() > 1 {
        return StringQuery::Regex(s[1..s.len() - 1].to_string());
    }

    if !s.contains(' ') {
        let upper_count = s.chars().filter(|c| c.is_uppercase()).count();
        if upper_count >= 2 {
            return StringQuery::Initialism(s.to_string());
        }
    }

    StringQuery::Contains(s.to_string())
}

/// Parse a CLI numeric string into a NumericQuery.
///
/// Formats:
/// - `"128"` → `Exact(128.0)`
/// - `"125..130"` → `Range(125.0, 130.0)`
/// - `"128+-2"` → `Tolerance { value: 128.0, up: 2.0, down: 2.0 }`
/// - `"128+2"` → `Tolerance { value: 128.0, up: 2.0, down: 0.0 }`
/// - `"128-2"` → `Tolerance { value: 128.0, up: 0.0, down: 2.0 }`
pub fn parse_numeric_query(s: &str) -> Result<NumericQuery, ParseError> {
    let s = s.trim();

    if let Some(idx) = s.find("..") {
        let lo: f32 = s[..idx]
            .parse()
            .map_err(|_| ParseError::InvalidNumeric(s.to_string()))?;
        let hi: f32 = s[idx + 2..]
            .parse()
            .map_err(|_| ParseError::InvalidNumeric(s.to_string()))?;
        return Ok(NumericQuery::Range(lo, hi));
    }

    if let Some((base, up_s, down_s)) = split_tolerance(s, 1) {
        let value: f32 = base
            .parse()
            .map_err(|_| ParseError::InvalidNumeric(s.to_string()))?;
        let up: f32 = up_s
            .parse()
            .map_err(|_| ParseError::InvalidNumeric(s.to_string()))?;
        let down: f32 = down_s
            .parse()
            .map_err(|_| ParseError::InvalidNumeric(s.to_string()))?;
        return Ok(NumericQuery::Tolerance { value, up, down });
    }

    let value: f32 = s
        .parse()
        .map_err(|_| ParseError::InvalidNumeric(s.to_string()))?;
    Ok(NumericQuery::Exact(value))
}

/// Parse a CLI duration string into a DurationQuery.
///
/// Formats:
/// - `"7m15s"` → `Exact(435)`
/// - `"7m"` → `WithPrecision(420, Minutes)`
/// - `">7m"` → `AtLeast(420)`
/// - `"<7m"` → `AtMost(420)`
/// - `"6m..7m30s"` → `Range(360, 450)`
pub fn parse_duration_query(s: &str) -> Result<DurationQuery, ParseError> {
    let s = s.trim();

    if let Some(rest) = s.strip_prefix('>') {
        let secs = parse_duration_secs(rest)?;
        return Ok(DurationQuery::AtLeast(secs));
    }

    if let Some(rest) = s.strip_prefix('<') {
        let secs = parse_duration_secs(rest)?;
        return Ok(DurationQuery::AtMost(secs));
    }

    if let Some(idx) = s.find("..") {
        let lo = parse_duration_secs(&s[..idx])?;
        let hi = parse_duration_secs(&s[idx + 2..])?;
        return Ok(DurationQuery::Range(lo, hi));
    }

    let (secs, last_unit) = parse_duration_parts(s)?;
    match last_unit {
        Some(DurationUnit::Seconds) => Ok(DurationQuery::Exact(secs)),
        Some(unit) => Ok(DurationQuery::WithPrecision(secs, unit)),
        None => Err(ParseError::InvalidDuration(s.to_string())),
    }
}

fn parse_duration_secs(s: &str) -> Result<u32, ParseError> {
    Ok(parse_duration_parts(s)?.0)
}

/// Returns (total_seconds, last_unit_seen).
fn parse_duration_parts(s: &str) -> Result<(u32, Option<DurationUnit>), ParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ParseError::InvalidDuration(s.to_string()));
    }

    let mut total: u32 = 0;
    let mut last_unit: Option<DurationUnit> = None;
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let num_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == num_start {
            return Err(ParseError::InvalidDuration(s.to_string()));
        }
        let num: u32 = s[num_start..i]
            .parse()
            .map_err(|_| ParseError::InvalidDuration(s.to_string()))?;

        if i >= bytes.len() {
            // Trailing digits with no unit
            return Err(ParseError::InvalidDuration(s.to_string()));
        }

        let (unit, factor) = match bytes[i] {
            b'h' => (DurationUnit::Hours, 3600u32),
            b'm' => (DurationUnit::Minutes, 60),
            b's' => (DurationUnit::Seconds, 1),
            _ => return Err(ParseError::InvalidDuration(s.to_string())),
        };
        total += num * factor;
        last_unit = Some(unit);
        i += 1;
    }

    Ok((total, last_unit))
}

/// Parse a key query string into a KeyQuery.
///
/// A key is any notation `Key` parses (`8A`, `1m`, `Am`, `C#`, `A minor`).
///
/// Formats:
/// - `"8A"` → exact key (`down = up = 0`)
/// - `"8A+-2"` (alias `"8A±2"`) → 2 steps either way
/// - `"8A+2"` → 2 steps clockwise; `"8A-1"` → 1 step counter-clockwise
///   (an A/B switch costs one step)
/// - `"6A..10B"` → numbers 6 to 10 clockwise, letters A or B
///
/// The old `~` suffix is rejected with a pointer to the new syntax.
pub fn parse_key_query(s: &str) -> Result<KeyQuery, ParseError> {
    let s = s.trim();
    let invalid = || ParseError::InvalidKey(s.to_string());

    if s.contains('~') {
        return Err(ParseError::KeyTilde(s.to_string()));
    }

    if let Some((from, to)) = s.split_once("..") {
        let from = from.parse().map_err(|_| invalid())?;
        let to = to.parse().map_err(|_| invalid())?;
        return Ok(KeyQuery::Span { from, to });
    }

    // `+-` must be tried before `+` and `-`.
    type Operator = (&'static str, fn(u8) -> Reach);
    const STEP_OPERATORS: [Operator; 4] = [
        ("+-", Reach::Both),
        ("±", Reach::Both),
        ("+", Reach::Up),
        ("-", Reach::Down),
    ];
    let (base, reach) = match s.find(['+', '-', '±']) {
        Some(at) => {
            let (base, op_and_steps) = s.split_at(at);
            let (steps, reach) = STEP_OPERATORS
                .iter()
                .find_map(|(op, reach)| op_and_steps.strip_prefix(op).map(|steps| (steps, reach)))
                .ok_or_else(invalid)?;
            // Digits only: `u8::from_str` would accept a second sign (`8A-+1`).
            if !steps.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            let steps: u8 = steps.parse().map_err(|_| invalid())?;
            (base, reach(steps))
        }
        None => (s, Reach::Both(0)),
    };
    let centre = base.parse().map_err(|_| invalid())?;
    Ok(KeyQuery::Steps { centre, reach })
}

/// Parse a CLI started/stopped date string into a `DateQuery`.
///
/// Formats:
/// - `"N/A"` → `NA`
/// - `"2026"` → range covering whole year
/// - `"2026-02"` → range covering whole month
/// - `"2026-02-20"` → range covering exact day
/// - `">2026-01"` → `After(2026-01-01, YearMonth)`
/// - `"<2026-01"` → `Before(2026-01-01, YearMonth)`
/// - `"2026-01..2026-06"` → `Range(2026-01-01, YearMonth, 2026-06-01, YearMonth)`
pub fn parse_date_query(s: &str) -> Result<DateQuery, ParseError> {
    let s = s.trim();

    if s.eq_ignore_ascii_case("n/a") {
        return Ok(DateQuery::NA);
    }

    if let Some(rest) = s.strip_prefix('>') {
        let (date, precision) = parse_date_with_precision(rest.trim(), s)?;
        return Ok(DateQuery::After(date, precision));
    }

    if let Some(rest) = s.strip_prefix('<') {
        let (date, precision) = parse_date_with_precision(rest.trim(), s)?;
        return Ok(DateQuery::Before(date, precision));
    }

    if let Some(idx) = s.find("..") {
        let lo_str = s[..idx].trim();
        let hi_str = s[idx + 2..].trim();
        let (lo_date, lo_prec) = parse_date_with_precision(lo_str, s)?;
        let (hi_date, hi_prec) = parse_date_with_precision(hi_str, s)?;
        return Ok(DateQuery::Range(lo_date, lo_prec, hi_date, hi_prec));
    }

    // Bare date: treat as range covering the full period
    let (date, precision) = parse_date_with_precision(s, s)?;
    Ok(DateQuery::Range(date, precision, date, precision))
}

fn parse_date_with_precision(
    s: &str,
    original: &str,
) -> Result<(NaiveDate, DatePrecision), ParseError> {
    let err = || ParseError::InvalidDateQuery(original.to_string());

    // Try date expression syntax (e.g. `~`, `-3/2`, `~/~/^`) only when the string
    // contains date expression tokens, to avoid intercepting plain date strings
    // like "2026" or "2026-02".
    let looks_like_date_expr = s.contains('~')
        || s.contains('^')
        || s.contains('$')
        || s.starts_with('+')
        || s.starts_with('-');
    if looks_like_date_expr {
        if let Ok(date) = date_expression::resolve(s, chrono::Local::now().date_naive()) {
            return Ok((date, DatePrecision::YearMonthDay));
        }
    }

    if s.len() == 10 && s.as_bytes().get(4) == Some(&b'-') && s.as_bytes().get(7) == Some(&b'-') {
        let date = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| err())?;
        return Ok((date, DatePrecision::YearMonthDay));
    }

    if s.len() == 7 && s.as_bytes().get(4) == Some(&b'-') {
        let year: i32 = s[..4].parse().map_err(|_| err())?;
        let month: u32 = s[5..7].parse().map_err(|_| err())?;
        let date = NaiveDate::from_ymd_opt(year, month, 1).ok_or_else(err)?;
        return Ok((date, DatePrecision::YearMonth));
    }

    if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s.parse().map_err(|_| err())?;
        let date = NaiveDate::from_ymd_opt(year, 1, 1).ok_or_else(err)?;
        return Ok((date, DatePrecision::Year));
    }

    Err(err())
}

/// Parse a CLI role string into a `TrackRole`.
///
/// Accepts the same forms as `TrackRole::from_str`:
/// lowercase, kebab-case ("build-up", "cool-down"), and PascalCase ("BuildUp").
pub fn parse_role_query(s: &str) -> Result<TrackRole, ParseError> {
    s.parse::<TrackRole>()
        .map_err(|_| ParseError::InvalidRole(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // --- FromStr: each query type parses itself, with a bare reason on failure ---

    #[test]
    fn query_types_parse_through_from_str() {
        assert!(matches!(
            "CBL".parse::<StringQuery>(),
            Ok(StringQuery::Initialism(_))
        ));
        assert!(matches!(
            "128".parse::<NumericQuery>(),
            Ok(NumericQuery::Exact(v)) if v == 128.0
        ));
        assert!(matches!(
            ">5m".parse::<DurationQuery>(),
            Ok(DurationQuery::AtLeast(300))
        ));
        assert!(matches!(
            "8B".parse::<KeyQuery>(),
            Ok(KeyQuery::Steps {
                reach: Reach::Both(0),
                ..
            })
        ));
        assert!(matches!("N/A".parse::<DateQuery>(), Ok(DateQuery::NA)));
    }

    #[test]
    fn from_str_errors_are_the_reason_without_the_word() {
        let word = "garbage";
        let reasons = [
            "garbage".parse::<NumericQuery>().unwrap_err(),
            "garbage".parse::<DurationQuery>().unwrap_err(),
            "garbage".parse::<KeyQuery>().unwrap_err(),
            "garbage".parse::<DateQuery>().unwrap_err(),
        ];
        for reason in reasons {
            assert!(!reason.is_empty());
            assert!(!reason.contains(word), "{reason}");
        }
        assert!(parse_numeric_query("garbage")
            .unwrap_err()
            .to_string()
            .contains("invalid numeric query 'garbage'"));
    }

    #[rstest]
    #[case("rymden")]
    #[case("carbon based lifeforms")]
    fn string_query_contains(#[case] input: &str) {
        assert!(matches!(
            parse_string_query(input),
            StringQuery::Contains(_)
        ));
    }

    #[rstest]
    #[case("CarbBased")]
    #[case("CarbBasedLife")]
    // All-caps is also treated as initialism
    #[case("CBL")]
    fn string_query_initialism(#[case] input: &str) {
        assert!(matches!(
            parse_string_query(input),
            StringQuery::Initialism(_)
        ));
    }

    #[test]
    fn string_query_regex() {
        let sq = parse_string_query("/^Carbon.*/");
        assert!(matches!(sq, StringQuery::Regex(ref p) if p == "^Carbon.*"));
    }

    #[test]
    fn numeric_exact() {
        let q = parse_numeric_query("128").unwrap();
        assert!(matches!(q, NumericQuery::Exact(v) if (v - 128.0).abs() < 0.01));
    }

    #[test]
    fn numeric_range() {
        let q = parse_numeric_query("124..132").unwrap();
        assert!(matches!(q, NumericQuery::Range(lo, hi) if lo == 124.0 && hi == 132.0));
    }

    #[test]
    fn numeric_tolerance_symmetric() {
        let q = parse_numeric_query("128+-4").unwrap();
        assert!(matches!(q, NumericQuery::Tolerance { value, up, down } if
            (value - 128.0).abs() < 0.01 && (up - 4.0).abs() < 0.01 && (down - 4.0).abs() < 0.01
        ));
    }

    #[test]
    fn numeric_tolerance_up_only() {
        let q = parse_numeric_query("128+2").unwrap();
        assert!(matches!(q, NumericQuery::Tolerance { value, up, down } if
            (value - 128.0).abs() < 0.01 && (up - 2.0).abs() < 0.01 && down == 0.0
        ));
    }

    #[test]
    fn numeric_tolerance_down_only() {
        let q = parse_numeric_query("128-2").unwrap();
        assert!(matches!(q, NumericQuery::Tolerance { value, up, down } if
            (value - 128.0).abs() < 0.01 && up == 0.0 && (down - 2.0).abs() < 0.01
        ));
    }

    #[test]
    fn duration_exact() {
        let q = parse_duration_query("7m15s").unwrap();
        assert!(matches!(q, DurationQuery::Exact(435)));
    }

    #[test]
    fn duration_with_precision_minutes() {
        let q = parse_duration_query("7m").unwrap();
        assert!(matches!(
            q,
            DurationQuery::WithPrecision(420, DurationUnit::Minutes)
        ));
    }

    #[test]
    fn duration_at_least() {
        let q = parse_duration_query(">7m").unwrap();
        assert!(matches!(q, DurationQuery::AtLeast(420)));
    }

    #[test]
    fn duration_at_most() {
        let q = parse_duration_query("<8m").unwrap();
        assert!(matches!(q, DurationQuery::AtMost(480)));
    }

    #[test]
    fn duration_range() {
        let q = parse_duration_query("6m..7m30s").unwrap();
        assert!(matches!(q, DurationQuery::Range(360, 450)));
    }

    /// Camelot names of every key the query matches, in wheel order (1A..12A, 1B..12B).
    fn matching(query: &str) -> Vec<String> {
        let q: KeyQuery = query.parse().unwrap();
        ['A', 'B']
            .into_iter()
            .flat_map(|l| (1..=12).map(move |n| format!("{n}{l}")))
            .filter(|k| q.matches(k.parse().unwrap()))
            .collect()
    }

    fn names(keys: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
        // Same wheel order as `matching`: A before B, numeric within a letter.
        v.sort_by_key(|k| (k.ends_with('B'), k[..k.len() - 1].parse::<u8>().unwrap()));
        v
    }

    #[rstest]
    #[case("8A", &["8A"])]
    #[case("1m", &["8A"])]
    #[case("Am", &["8A"])]
    #[case("A minor", &["8A"])]
    #[case("C#", &["3B"])]
    #[case("Eb", &["5B"])]
    fn key_exact_matches_only_that_key(#[case] query: &str, #[case] expected: &[&str]) {
        assert_eq!(matching(query), names(expected));
    }

    #[test]
    fn key_steps_both_directions_with_mode_switch() {
        assert_eq!(
            matching("8A+-2"),
            names(&["6A", "7A", "8A", "9A", "10A", "7B", "8B", "9B"])
        );
    }

    #[test]
    fn key_steps_wrap_around_the_wheel() {
        assert_eq!(matching("1A+-1"), names(&["12A", "1A", "2A", "1B"]));
    }

    #[test]
    fn key_steps_mode_switch_costs_a_step() {
        let m = matching("3A+-2");
        for k in ["4B", "2B", "3B"] {
            assert!(m.contains(&k.to_string()), "{k}");
        }
        assert!(!m.contains(&"5B".to_string()));
    }

    #[test]
    fn key_steps_clockwise_only() {
        assert_eq!(matching("8A+2"), names(&["8A", "9A", "10A", "8B", "9B"]));
    }

    #[test]
    fn key_steps_counter_clockwise_only() {
        assert_eq!(matching("8A-1"), names(&["7A", "8A", "8B"]));
    }

    #[test]
    fn key_plus_minus_alias_is_accepted() {
        assert_eq!(matching("8A±2"), matching("8A+-2"));
    }

    #[test]
    fn key_span_within_one_letter() {
        assert_eq!(matching("6A..10A"), names(&["6A", "7A", "8A", "9A", "10A"]));
    }

    #[test]
    fn key_span_wraps_clockwise() {
        assert_eq!(matching("11A..1A"), names(&["11A", "12A", "1A"]));
    }

    #[test]
    fn key_span_letters_are_a_set_without_order() {
        assert_eq!(matching("1A..1B"), names(&["1A", "1B"]));
        assert_eq!(matching("1B..1A"), matching("1A..1B"));
    }

    #[test]
    fn key_span_across_letters_has_ten_keys() {
        assert_eq!(
            matching("6A..10B"),
            names(&["6A", "7A", "8A", "9A", "10A", "6B", "7B", "8B", "9B", "10B"])
        );
    }

    #[test]
    fn key_span_runs_the_long_way_round() {
        assert_eq!(
            matching("10A..6A"),
            names(&["10A", "11A", "12A", "1A", "2A", "3A", "4A", "5A", "6A"])
        );
    }

    #[test]
    fn key_query_matches_every_spelling_of_the_same_key() {
        let stored: music_primitives::Key = "A Minor".parse().unwrap();
        for q in ["8A", "Am", "1m", "A minor"] {
            assert!(q.parse::<KeyQuery>().unwrap().matches(stored), "{q}");
        }
        assert!(!"8B".parse::<KeyQuery>().unwrap().matches(stored));
    }

    #[test]
    fn tilde_is_a_parse_error_pointing_to_new_syntax() {
        let err = parse_key_query("8A+-1~").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("`~` suffix is gone"), "{text}");
        assert!(text.contains("8A+-2") && text.contains("6A..10B"), "{text}");
        let reason = "8A+-1~".parse::<KeyQuery>().unwrap_err();
        assert!(reason.contains("`~` suffix is gone"), "{reason}");
    }

    #[rstest]
    #[case("~8A")]
    #[case("8A~+-1")]
    #[case("8A+-1~")]
    fn tilde_anywhere_gets_the_specific_error(#[case] input: &str) {
        assert!(matches!(
            parse_key_query(input),
            Err(ParseError::KeyTilde(_))
        ));
    }

    #[rstest]
    #[case("8A+")]
    #[case("8A+x")]
    #[case("8A..")]
    #[case("..8A")]
    #[case("8A..9A..10A")]
    #[case("13A+1")]
    #[case("")]
    #[case("8A-+1")]
    #[case("8A++1")]
    #[case("8A+-+1")]
    #[case("8A+-1-1")]
    fn key_query_rejects_malformed(#[case] input: &str) {
        assert!(input.parse::<KeyQuery>().is_err(), "{input}");
    }

    #[rstest]
    #[case("8A")]
    #[case("8A+-2")]
    #[case("8A+3")]
    #[case("8A-1")]
    #[case("8A+0")]
    #[case("8A-0")]
    #[case("6A..10B")]
    #[case("1B..1A")]
    fn key_query_display_round_trips(#[case] input: &str) {
        let q: KeyQuery = input.parse().unwrap();
        assert_eq!(q.to_string(), input);
        assert_eq!(q.to_string().parse::<KeyQuery>().unwrap(), q);
    }

    #[rstest]
    #[case("8A", Reach::Both(0))]
    #[case("8A+-2", Reach::Both(2))]
    #[case("8A+3", Reach::Up(3))]
    #[case("8A-1", Reach::Down(1))]
    fn key_query_display_round_trips_every_reach(#[case] text: &str, #[case] reach: Reach) {
        let q = KeyQuery::Steps {
            centre: "8A".parse().unwrap(),
            reach,
        };
        assert_eq!(q.to_string(), text);
        assert_eq!(text.parse::<KeyQuery>().unwrap(), q);
    }

    #[rstest]
    #[case("\"8A+-2\"")]
    #[case("\"8A\"")]
    #[case("\"8A+3\"")]
    #[case("\"8A-1\"")]
    #[case("\"6A..10B\"")]
    fn key_query_json_round_trips_as_its_string_form(#[case] json: &str) {
        let q: KeyQuery = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&q).unwrap(), json);
    }

    #[test]
    fn key_query_json_rejects_a_bad_string() {
        assert!(serde_json::from_str::<KeyQuery>("\"nonsense\"").is_err());
    }

    #[test]
    fn key_query_display_normalises_notation() {
        let q: KeyQuery = "A minor+-1".parse().unwrap();
        assert_eq!(q.to_string(), "8A+-1");
    }

    #[rstest]
    #[case("N/A")]
    #[case("n/a")]
    fn date_query_na(#[case] input: &str) {
        assert!(matches!(parse_date_query(input).unwrap(), DateQuery::NA));
    }

    #[test]
    fn date_query_bare_year() {
        assert!(matches!(
            parse_date_query("2026").unwrap(),
            DateQuery::Range(_, DatePrecision::Year, _, DatePrecision::Year)
        ));
    }

    #[test]
    fn date_query_bare_year_month() {
        assert!(matches!(
            parse_date_query("2026-02").unwrap(),
            DateQuery::Range(_, DatePrecision::YearMonth, _, DatePrecision::YearMonth)
        ));
    }

    #[test]
    fn date_query_after_year_month() {
        assert!(matches!(
            parse_date_query(">2026-01").unwrap(),
            DateQuery::After(_, DatePrecision::YearMonth)
        ));
    }

    #[test]
    fn date_query_before_year() {
        assert!(matches!(
            parse_date_query("<2026").unwrap(),
            DateQuery::Before(_, DatePrecision::Year)
        ));
    }

    #[test]
    fn date_query_range() {
        assert!(matches!(
            parse_date_query("2026-01..2026-06").unwrap(),
            DateQuery::Range(_, DatePrecision::YearMonth, _, DatePrecision::YearMonth)
        ));
    }

    #[rstest]
    #[case("not-a-date")]
    #[case("")]
    fn date_query_invalid(#[case] input: &str) {
        assert!(parse_date_query(input).is_err());
    }

    // --- date expression integration tests ---

    #[test]
    fn date_query_expr_today() {
        // `~` resolves to today with YearMonthDay precision
        assert!(matches!(
            parse_date_query("~").unwrap(),
            DateQuery::Range(
                _,
                DatePrecision::YearMonthDay,
                _,
                DatePrecision::YearMonthDay
            )
        ));
    }

    #[test]
    fn date_query_expr_first_of_month() {
        let result = parse_date_query("~/~/^");
        assert!(result.is_ok());
        assert!(matches!(
            result.unwrap(),
            DateQuery::Range(
                _,
                DatePrecision::YearMonthDay,
                _,
                DatePrecision::YearMonthDay
            )
        ));
    }

    #[test]
    fn date_query_expr_range_tilde_based() {
        // `~/~/^..~/~/$` -- first to last day of current month
        let result = parse_date_query("~/~/^..~/~/$");
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DateQuery::Range(..)));
    }

    // =========================================================================
    // Role query parse tests
    // =========================================================================

    #[rstest]
    #[case("banger", TrackRole::Banger)]
    #[case("Banger", TrackRole::Banger)]
    #[case("build-up", TrackRole::BuildUp)]
    #[case("BuildUp", TrackRole::BuildUp)]
    #[case("peak", TrackRole::Peak)]
    #[case("Peak", TrackRole::Peak)]
    #[case("opener", TrackRole::Opener)]
    #[case("cool-down", TrackRole::CoolDown)]
    #[case("CoolDown", TrackRole::CoolDown)]
    #[case("closer", TrackRole::Closer)]
    #[case("filler", TrackRole::Filler)]
    fn parse_role_query_valid(#[case] input: &str, #[case] expected: TrackRole) {
        assert_eq!(parse_role_query(input).unwrap(), expected);
    }

    #[rstest]
    #[case("invalid-role")]
    #[case("")]
    #[case("DJ")]
    #[case("123")]
    fn parse_role_query_invalid(#[case] input: &str) {
        assert!(parse_role_query(input).is_err());
        let err = parse_role_query(input).unwrap_err();
        assert!(err.to_string().contains(input) || input.is_empty());
    }
}
