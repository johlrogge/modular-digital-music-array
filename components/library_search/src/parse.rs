use crate::query::{
    DatePrecision, DateQuery, DurationQuery, DurationUnit, EnergyQuery, KeyQuery, NumericQuery,
    Reach, Shape, StringQuery,
};
use chrono::NaiveDate;
use music_primitives::{EnergyLevel, TrackRole};
use std::convert::Infallible;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid numeric query '{0}': expected a number, 'lo..hi', 'val+-tol', 'val+tol', or 'val-tol'")]
    InvalidNumeric(String),
    #[error("invalid energy query '{0}': {1}")]
    InvalidEnergy(String, &'static str),
    #[error("invalid duration query '{0}': expected e.g. '7m15s', '7m', '>5m', '<8m', '6m..8m'")]
    InvalidDuration(String),
    #[error("invalid key query '{0}': expected a key (8B, 1m, Am, C# major), steps (8A+-2, 8A+2, 8A-1; add d for any path: 8A+-2d; extra moves: 8A+-1,+7) or a span (6A..10B)")]
    InvalidKey(String),
    #[error("key query '{0}': the `~` suffix is gone; use steps like `8A+-2` (round the wheel, or the relative key; `8A+-2d` for any path) or a span like `6A..10B`")]
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
            Self::InvalidEnergy(_, phrase) => phrase,
            Self::InvalidDuration(_) => {
                "expected a duration such as `7m15s`, `7m`, `>5m`, `<8m` or `6m..8m`"
            }
            Self::InvalidKey(_) => {
                "expected a key (`8B`, `1m`, `Am`, `C# major`), steps (`8A+-2`, `8A+2`, `8A-1`; add `d` for any path: `8A+-2d`; extra moves: `8A+-1,+7`) or a span (`6A..10B`)"
            }
            Self::KeyTilde(_) => {
                "the `~` suffix is gone; use steps like `8A+-2` (round the wheel, or the relative key; `8A+-2d` for any path) or a span like `6A..10B`"
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
from_str_query!(EnergyQuery, parse_energy_query);
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

/// Parse an energy query (1-10): the numeric formats, with every bound a valid
/// [`EnergyLevel`]; a tolerance's centre must be one, and the tolerance itself is unbounded
/// (`10+-1` covers 9-10, `7+-20` covers 1-10). `0`, `11`, `0..5`, `7.5` and `7.5+-1` are
/// rejected.
pub fn parse_energy_query(s: &str) -> Result<EnergyQuery, ParseError> {
    let query = parse_numeric_query(s)?;
    let reject = |phrase| ParseError::InvalidEnergy(s.trim().to_string(), phrase);
    let check = |bound: f32, phrase| {
        let whole = bound.fract() == 0.0 && (0.0..=255.0).contains(&bound);
        whole
            .then_some(bound as u8)
            .and_then(|level| EnergyLevel::new(level).ok())
            .map(drop)
            .ok_or_else(|| reject(phrase))
    };
    match &query {
        NumericQuery::Exact(v) => check(*v, "must be a whole level from 1 to 10")?,
        NumericQuery::Range(lo, hi) => {
            check(*lo, "lower bound must be a whole level from 1 to 10")?;
            check(*hi, "upper bound must be a whole level from 1 to 10")?;
        }
        NumericQuery::Tolerance { value, .. } => {
            // The tolerance is unbounded: it covers whatever part of 1-10 it reaches.
            check(*value, "value must be a whole level from 1 to 10")?;
        }
    }
    Ok(EnergyQuery(query))
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
/// - `"8A+-2"` (alias `"8A±2"`) → 2 steps either way, Straight: round the wheel in
///   one letter, or exactly the relative (6A 7A 8A 9A 10A 8B)
/// - `"8A+2"` → 2 steps clockwise; `"8A-1"` → 1 step counter-clockwise
/// - `"8A+-2d"` → distance only, any path (a step or an A/B switch costs 1); the
///   `d` suffix works on every step form
/// - `"8A+-1,+7"` → plus exact moves from the centre, keeping the letter (`,+7`
///   adds 3A); only after a steps term
/// - `"6A..10B"` → numbers 6 to 10 clockwise, letters A or B
///
/// The old `~` suffix is rejected with a pointer to the new syntax.
pub fn parse_key_query(s: &str) -> Result<KeyQuery, ParseError> {
    let s = s.trim();
    let invalid = || ParseError::InvalidKey(s.to_string());

    if s.contains('~') {
        return Err(ParseError::KeyTilde(s.to_string()));
    }

    let (term, extra_moves) = match s.split_once(',') {
        Some((term, rest)) => (term, Some(rest)),
        None => (s, None),
    };

    if let Some((from, to)) = term.split_once("..") {
        if extra_moves.is_some() {
            return Err(invalid());
        }
        let from = from.parse().map_err(|_| invalid())?;
        let to = to.parse().map_err(|_| invalid())?;
        return Ok(KeyQuery::Span { from, to });
    }

    // Each extra move is a signed wheel offset: digits only, like the steps.
    let jumps = extra_moves
        .into_iter()
        .flat_map(|rest| rest.split(','))
        .map(|m| {
            let digits = m.strip_prefix(['+', '-']).ok_or_else(invalid)?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            m.parse::<i8>().map_err(|_| invalid())
        })
        .collect::<Result<Vec<_>, _>>()?;
    // `,+7,+7` says one thing once.
    let jumps = jumps.into_iter().fold(Vec::new(), |mut seen, j| {
        if !seen.contains(&j) {
            seen.push(j);
        }
        seen
    });

    // `+-` must be tried before `+` and `-`.
    type Operator = (&'static str, fn(u8) -> Reach);
    const STEP_OPERATORS: [Operator; 4] = [
        ("+-", Reach::Both),
        ("±", Reach::Both),
        ("+", Reach::Up),
        ("-", Reach::Down),
    ];
    let (base, reach, shape) = match term.find(['+', '-', '±']) {
        Some(at) => {
            let (base, op_and_steps) = term.split_at(at);
            let (steps, reach) = STEP_OPERATORS
                .iter()
                .find_map(|(op, reach)| op_and_steps.strip_prefix(op).map(|steps| (steps, reach)))
                .ok_or_else(invalid)?;
            let (steps, any_path) = match steps.strip_suffix('d') {
                Some(steps) => (steps, true),
                None => (steps, false),
            };
            // Digits only: `u8::from_str` would accept a second sign (`8A-+1`).
            if !steps.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            let steps: u8 = steps.parse().map_err(|_| invalid())?;
            // At 0 steps there is no path to take: the exact key, written without a `d`.
            let shape = if any_path && steps > 0 {
                Shape::AnyPath
            } else {
                Shape::Straight
            };
            (base, reach(steps), shape)
        }
        None => (term, Reach::Both(0), Shape::Straight),
    };
    let centre = base.parse().map_err(|_| invalid())?;
    Ok(KeyQuery::Steps {
        centre,
        reach,
        shape,
        jumps,
    })
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

    #[rstest]
    #[case("1")]
    #[case("7")]
    #[case("10")]
    #[case("5..8")]
    #[case("1..10")]
    #[case("7+-2")]
    #[case("7+2")]
    #[case("7-2")]
    #[case("10+-1")]
    #[case("1+-1")]
    #[case("10+2")]
    #[case("7+-20")]
    fn energy_query_accepts_valid(#[case] input: &str) {
        assert!(parse_energy_query(input).is_ok(), "{input}");
    }

    #[rstest]
    #[case("0", "must be a whole level")]
    #[case("11", "must be a whole level")]
    #[case("7.5", "must be a whole level")]
    #[case("0..5", "lower bound")]
    #[case("5..11", "upper bound")]
    #[case("0+-2", "value must be")]
    #[case("11+-1", "value must be")]
    #[case("7.5+-1", "value must be")]
    fn energy_query_rejects_out_of_range(#[case] input: &str, #[case] reason: &str) {
        let err = parse_energy_query(input).unwrap_err();
        assert!(err.reason().contains(reason), "{input}: {err}");
    }

    #[test]
    fn energy_query_rejects_garbage_as_numeric() {
        assert!(matches!(
            parse_energy_query("loud"),
            Err(ParseError::InvalidNumeric(_))
        ));
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
        music_primitives::Key::camelot_wheel()
            .filter(|k| q.matches(*k))
            .map(|k| k.to_camelot())
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
    fn key_steps_straight_is_a_t_shape() {
        assert_eq!(
            matching("8A+-2"),
            names(&["6A", "7A", "8A", "9A", "10A", "8B"])
        );
    }

    #[test]
    fn key_steps_distance_adds_the_diagonals() {
        assert_eq!(
            matching("8A+-2d"),
            names(&["6A", "7A", "8A", "9A", "10A", "7B", "8B", "9B"])
        );
    }

    #[test]
    fn key_steps_at_one_are_the_neighbours_and_the_relative() {
        assert_eq!(matching("8A+-1"), names(&["7A", "8A", "9A", "8B"]));
        assert_eq!(matching("8A+-1d"), names(&["7A", "8A", "9A", "8B"]));
    }

    #[test]
    fn key_steps_shapes_are_equal_at_one_for_every_centre() {
        for centre in all_keys() {
            assert_eq!(
                matching(&format!("{centre}+-1")),
                matching(&format!("{centre}+-1d")),
                "{centre}"
            );
        }
    }

    #[test]
    fn key_steps_straight_is_within_distance_for_every_centre_and_reach() {
        for centre in all_keys() {
            for n in 0..=6 {
                for op in ["+-", "+", "-"] {
                    let straight = matching(&format!("{centre}{op}{n}"));
                    let distance = matching(&format!("{centre}{op}{n}d"));
                    assert!(
                        straight.iter().all(|k| distance.contains(k)),
                        "{centre}{op}{n}"
                    );
                }
            }
        }
    }

    #[test]
    fn key_steps_straight_at_zero_is_the_exact_key() {
        assert_eq!(matching("8A+-0"), names(&["8A"]));
    }

    #[test]
    fn key_steps_plus_zero_is_exactly_the_centre() {
        assert_eq!(matching("8A+0"), names(&["8A"]));
        assert_eq!(matching("8A+0d"), names(&["8A"]));
    }

    #[test]
    fn key_steps_distance_at_zero_is_the_plain_exact_key() {
        let q: KeyQuery = "8A+-0d".parse().unwrap();
        assert_eq!(q, "8A".parse().unwrap());
        assert_eq!(q.to_string(), "8A");
    }

    /// `d` at 0 steps normalises away, so what Display writes parses back to the same query.
    #[rstest]
    #[case("8A+-0d", "8A")]
    #[case("8A+0d", "8A+0")]
    #[case("8A+-0d,+7", "8A,+7")]
    fn key_distance_at_zero_round_trips_normalised(#[case] input: &str, #[case] shown: &str) {
        let q: KeyQuery = input.parse().unwrap();
        assert_eq!(q.to_string(), shown);
        assert_eq!(shown.parse::<KeyQuery>().unwrap(), q);
    }

    #[test]
    fn key_steps_clockwise_excludes_the_counter_clockwise_side() {
        let m = matching("8A+2");
        assert!(!m.contains(&"7A".to_string()));
        assert!(!matching("8A-2").contains(&"9A".to_string()));
    }

    #[test]
    fn key_steps_counter_clockwise_wraps_to_12a() {
        assert_eq!(matching("1A-1"), names(&["12A", "1A", "1B"]));
    }

    #[test]
    fn key_steps_b_centre_mirrors_the_a_centre() {
        assert_eq!(
            matching("8B+-2"),
            names(&["6B", "7B", "8B", "9B", "10B", "8A"])
        );
        assert_eq!(
            matching("8B+-2d"),
            names(&["6B", "7B", "8B", "9B", "10B", "7A", "8A", "9A"])
        );
    }

    #[test]
    fn key_steps_b_centre_wraps_the_wheel() {
        assert_eq!(matching("1B+-1"), names(&["12B", "1B", "2B", "1A"]));
    }

    #[test]
    fn key_steps_straight_wraps_the_wheel_at_two() {
        assert_eq!(
            matching("12A+-2"),
            names(&["10A", "11A", "12A", "1A", "2A", "12B"])
        );
    }

    #[test]
    fn key_jump_adds_only_the_power_jump() {
        assert_eq!(matching("8A+-1,+7"), names(&["7A", "8A", "9A", "8B", "3A"]));
    }

    #[test]
    fn key_jump_composes_with_the_distance_shape() {
        assert_eq!(
            matching("8A+-2d,+7"),
            names(&["6A", "7A", "8A", "9A", "10A", "7B", "8B", "9B", "3A"])
        );
    }

    #[test]
    fn key_jump_keeps_the_letter() {
        assert_eq!(matching("8B,+7"), names(&["8B", "3B"]));
    }

    #[test]
    fn key_jump_may_go_down() {
        // -7 is 5 steps clockwise round the wheel: 1A.
        assert_eq!(matching("8A,-7"), names(&["8A", "1A"]));
    }

    #[test]
    fn key_jump_wraps_the_wheel() {
        assert_eq!(matching("12A,+7"), names(&["12A", "7A"]));
    }

    /// A whole turn of the wheel and no turn at all land on the centre, which every reach
    /// already includes: both are accepted and add nothing.
    #[rstest]
    #[case("8A,+12")]
    #[case("8A,+0")]
    #[case("8A,-12")]
    fn key_jump_of_a_whole_turn_or_none_is_the_centre(#[case] query: &str) {
        assert_eq!(matching(query), names(&["8A"]));
    }

    #[test]
    fn key_jumps_are_deduplicated() {
        let q: KeyQuery = "8A,+7,+7".parse().unwrap();
        assert_eq!(q.to_string(), "8A,+7");
    }

    /// Camelot names of the 24 keys, in wheel order.
    fn all_keys() -> Vec<String> {
        music_primitives::Key::camelot_wheel()
            .map(|k| k.to_camelot())
            .collect()
    }

    #[test]
    fn key_steps_wrap_around_the_wheel() {
        assert_eq!(matching("1A+-1"), names(&["12A", "1A", "2A", "1B"]));
    }

    #[test]
    fn key_steps_distance_mode_switch_costs_a_step() {
        assert_eq!(
            matching("3A+-2d"),
            names(&["1A", "2A", "3A", "4A", "5A", "2B", "3B", "4B"])
        );
    }

    #[test]
    fn key_steps_clockwise_only() {
        assert_eq!(matching("8A+2"), names(&["8A", "9A", "10A", "8B"]));
        assert_eq!(matching("8A+2d"), names(&["8A", "9A", "10A", "8B", "9B"]));
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
    #[case("8A+-2x")]
    #[case("8A+-2D")]
    #[case("8A+-d")]
    #[case("8Ad")]
    #[case("8A+-2dd")]
    #[case("8A+-1,")]
    #[case("8A+-1,7")]
    #[case("8A+-1,+")]
    #[case("8A+-1,+x")]
    #[case("8A+-1,+-7")]
    #[case("8A+-1,+7d")]
    #[case("6A..10B,+7")]
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
    #[case("8A+-2d")]
    #[case("8A+3d")]
    #[case("8A-1d")]
    #[case("8A+-1,+7")]
    #[case("8A+-2d,+7")]
    #[case("8A,+7,-7")]
    #[case("6A..10B")]
    #[case("1B..1A")]
    fn key_query_display_round_trips(#[case] input: &str) {
        let q: KeyQuery = input.parse().unwrap();
        assert_eq!(q.to_string(), input);
        assert_eq!(q.to_string().parse::<KeyQuery>().unwrap(), q);
    }

    mod round_trip {
        use super::*;
        use proptest::prelude::*;

        /// Any steps query in the shape the parser produces: no `d` at 0 steps, jumps once each.
        fn steps_query() -> impl Strategy<Value = KeyQuery> {
            (
                0usize..24,
                0u8..=12,
                0u8..3,
                any::<bool>(),
                proptest::collection::vec(-12i8..=12, 0..4),
            )
                .prop_map(|(centre, n, direction, any_path, moves)| {
                    let reach = match direction {
                        0 => Reach::Both(n),
                        1 => Reach::Up(n),
                        _ => Reach::Down(n),
                    };
                    let mut jumps = Vec::new();
                    for j in moves {
                        if !jumps.contains(&j) {
                            jumps.push(j);
                        }
                    }
                    KeyQuery::Steps {
                        centre: music_primitives::Key::camelot_wheel().nth(centre).unwrap(),
                        reach,
                        shape: if any_path && n > 0 {
                            Shape::AnyPath
                        } else {
                            Shape::Straight
                        },
                        jumps,
                    }
                })
        }

        proptest! {
            #[test]
            fn display_parses_back_to_the_same_query(q in steps_query()) {
                prop_assert_eq!(q.to_string().parse::<KeyQuery>().unwrap(), q);
            }
        }
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
            shape: Shape::Straight,
            jumps: vec![],
        };
        assert_eq!(q.to_string(), text);
        assert_eq!(text.parse::<KeyQuery>().unwrap(), q);
    }

    #[rstest]
    #[case("\"8A+-2\"")]
    #[case("\"8A\"")]
    #[case("\"8A+3\"")]
    #[case("\"8A-1\"")]
    #[case("\"8A+-2d,+7\"")]
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
