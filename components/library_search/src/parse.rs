use crate::query::{
    CamelotLetter, DatePrecision, DateQuery, DurationQuery, DurationUnit, KeyQuery, NumericQuery,
    StringQuery,
};
use chrono::NaiveDate;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid numeric query '{0}': expected a number, 'lo..hi', 'val+-tol', 'val+tol', or 'val-tol'")]
    InvalidNumeric(String),
    #[error("invalid duration query '{0}': expected e.g. '7m15s', '7m', '>5m', '<8m', '6m..8m'")]
    InvalidDuration(String),
    #[error("invalid key query '{0}': expected Camelot (8B, 10A) or traditional (Am, C# major, A minor)")]
    InvalidKey(String),
    #[error("invalid date query '{0}'")]
    InvalidDateQuery(String),
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
/// Formats:
/// - `"8B"`, `"10A"` → Camelot exact
/// - `"Am"`, `"A minor"`, `"C# major"` → traditional notation (converted to Camelot)
/// - `"8B+-1"` → symmetric tolerance
/// - `"8B+1"` → up-only tolerance
/// - `"8B-1"` → down-only tolerance
/// - `"8B+-1~"` → tolerance with relative key included
pub fn parse_key_query(s: &str) -> Result<KeyQuery, ParseError> {
    let s = s.trim();

    let (s_notilde, include_relative) = if let Some(stripped) = s.strip_suffix('~') {
        (stripped, true)
    } else {
        (s, false)
    };

    if let Some((base, up_s, down_s)) = split_tolerance(s_notilde, 2) {
        let tol_up: u8 = up_s
            .parse()
            .map_err(|_| ParseError::InvalidKey(s.to_string()))?;
        let tol_down: u8 = down_s
            .parse()
            .map_err(|_| ParseError::InvalidKey(s.to_string()))?;
        let (number, letter) = parse_camelot_base(base, s)?;
        return Ok(KeyQuery::Tolerance {
            number,
            letter,
            tolerance_up: tol_up,
            tolerance_down: tol_down,
            include_relative,
        });
    }

    let (number, letter) = parse_camelot_base(s_notilde, s)?;
    Ok(KeyQuery::Exact { number, letter })
}

/// Parse a bare key string (no tolerance suffix) to (camelot_number, CamelotLetter).
fn parse_camelot_base(base: &str, original: &str) -> Result<(u8, CamelotLetter), ParseError> {
    let base = base.trim();

    // Try Camelot format: "8B", "10A"
    if let Some(last) = base.chars().last() {
        if last == 'A' || last == 'B' {
            let num_str = &base[..base.len() - 1];
            if let Ok(num) = num_str.parse::<u8>() {
                if (1..=12).contains(&num) {
                    let letter = if last == 'A' {
                        CamelotLetter::A
                    } else {
                        CamelotLetter::B
                    };
                    return Ok((num, letter));
                }
            }
        }
    }

    traditional_to_camelot(base).ok_or_else(|| ParseError::InvalidKey(original.to_string()))
}

/// Convert traditional key notation to (camelot_number, CamelotLetter).
fn traditional_to_camelot(s: &str) -> Option<(u8, CamelotLetter)> {
    let normalized = normalize_key_notation(&s.trim().to_lowercase());

    // (normalized_key, camelot_number, is_minor)
    // is_minor = true → CamelotLetter::A, false → CamelotLetter::B
    let map: &[(&str, u8, bool)] = &[
        // Minor keys → A
        ("abm", 1, true),
        ("g#m", 1, true),
        ("ebm", 2, true),
        ("d#m", 2, true),
        ("bbm", 3, true),
        ("a#m", 3, true),
        ("fm", 4, true),
        ("cm", 5, true),
        ("gm", 6, true),
        ("dm", 7, true),
        ("am", 8, true),
        ("em", 9, true),
        ("bm", 10, true),
        ("f#m", 11, true),
        ("gbm", 11, true),
        ("c#m", 12, true),
        ("dbm", 12, true),
        // Major keys → B
        ("b", 1, false),
        ("f#", 2, false),
        ("gb", 2, false),
        ("db", 3, false),
        ("c#", 3, false),
        ("ab", 4, false),
        ("g#", 4, false),
        ("eb", 5, false),
        ("d#", 5, false),
        ("bb", 6, false),
        ("a#", 6, false),
        ("f", 7, false),
        ("c", 8, false),
        ("g", 9, false),
        ("d", 10, false),
        ("a", 11, false),
        ("e", 12, false),
    ];

    for (key, num, is_minor) in map {
        if normalized == *key {
            return Some((
                *num,
                if *is_minor {
                    CamelotLetter::A
                } else {
                    CamelotLetter::B
                },
            ));
        }
    }
    None
}

/// Normalize key notation: "a minor" → "am", "c# major" → "c#", "am" → "am".
fn normalize_key_notation(s: &str) -> String {
    let s = s.trim();
    if let Some(key) = s.strip_suffix(" minor") {
        return format!("{}m", key.trim().replace(' ', ""));
    }
    if let Some(key) = s.strip_suffix(" major") {
        return key.trim().replace(' ', "").to_string();
    }
    s.replace(' ', "")
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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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

    #[test]
    fn key_camelot_exact() {
        let q = parse_key_query("8B").unwrap();
        assert!(matches!(
            q,
            KeyQuery::Exact {
                number: 8,
                letter: CamelotLetter::B
            }
        ));
    }

    #[test]
    fn key_traditional_minor() {
        let q = parse_key_query("Am").unwrap();
        assert!(matches!(
            q,
            KeyQuery::Exact {
                number: 8,
                letter: CamelotLetter::A
            }
        ));
    }

    #[test]
    fn key_traditional_minor_long() {
        let q = parse_key_query("A minor").unwrap();
        assert!(matches!(
            q,
            KeyQuery::Exact {
                number: 8,
                letter: CamelotLetter::A
            }
        ));
    }

    #[test]
    fn key_traditional_major() {
        // C major = 8B
        let q = parse_key_query("C").unwrap();
        assert!(matches!(
            q,
            KeyQuery::Exact {
                number: 8,
                letter: CamelotLetter::B
            }
        ));
    }

    #[test]
    fn key_camelot_tolerance_symmetric() {
        let q = parse_key_query("8B+-1").unwrap();
        assert!(matches!(
            q,
            KeyQuery::Tolerance {
                number: 8,
                letter: CamelotLetter::B,
                tolerance_up: 1,
                tolerance_down: 1,
                include_relative: false,
            }
        ));
    }

    #[test]
    fn key_camelot_tolerance_with_relative() {
        let q = parse_key_query("8B+-1~").unwrap();
        assert!(matches!(
            q,
            KeyQuery::Tolerance {
                include_relative: true,
                ..
            }
        ));
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
}
