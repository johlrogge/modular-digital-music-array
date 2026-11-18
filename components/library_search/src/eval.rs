use crate::query::{
    DatePrecision, DateQuery, DurationQuery, DurationUnit, NumericQuery, StringQuery, TrackQuery,
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use music_facts::Mark;
use music_primitives::{Key, TrackRole};
use regex::Regex;

/// Flat track fields used to evaluate a `TrackQuery`.
/// Built from `IndexedTrackInfo` in the library service.
pub struct TrackFields<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub label: Option<&'a str>,
    pub genre: Option<&'a str>,
    /// StyleDescriptor — multiple per track; style query matches if any matches.
    pub styles: &'a [String],
    pub bpm: Option<f32>,
    pub key: Option<Key>,
    /// Duration in seconds.
    pub duration: Option<u32>,
    pub year: Option<u32>,
    pub source: Option<&'a str>,
    pub last_started: Option<DateTime<Utc>>,
    pub last_stopped: Option<DateTime<Utc>>,
    pub added: Option<DateTime<Utc>>,
    /// DJ curation role.
    pub role: Option<TrackRole>,
    /// Energy level 1–10.
    pub energy: Option<u8>,
    /// The marks the track carries live. Owned, because the fold derives them.
    pub marks: Vec<Mark>,
}

/// Evaluate a `TrackQuery` against a set of track fields, respecting `query.not`.
///
/// Compiles the query on every call; to test many tracks build a [`TrackMatcher`] once.
pub fn matches_query(query: &TrackQuery, track: &TrackFields) -> bool {
    TrackMatcher::new(query).matches(track)
}

/// A `StringQuery` compiled once: the one implementation of the text rules, for a single
/// string (a playlist name) or, through [`TrackMatcher`], for track fields.
///
/// A regex that does not compile matches nothing.
#[derive(Debug, Clone)]
pub struct StringMatcher(Compiled);

#[derive(Debug, Clone)]
enum Compiled {
    /// Lower-cased words; all must occur in the string.
    Contains(Vec<String>),
    /// Initialism or regex; `None` when the pattern is invalid.
    Pattern(Option<Regex>),
}

impl StringMatcher {
    pub fn new(query: &StringQuery) -> Self {
        Self(match query {
            StringQuery::Contains(q) => {
                Compiled::Contains(q.split_whitespace().map(str::to_lowercase).collect())
            }
            StringQuery::Initialism(q) => {
                Compiled::Pattern(Regex::new(&build_initialism_regex(q)).ok())
            }
            StringQuery::Regex(pattern) => Compiled::Pattern(Regex::new(pattern).ok()),
        })
    }

    pub fn matches(&self, field: &str) -> bool {
        match &self.0 {
            Compiled::Contains(words) => {
                let lower = field.to_lowercase();
                words.iter().all(|w| lower.contains(w.as_str()))
            }
            Compiled::Pattern(re) => re.as_ref().is_some_and(|re| re.is_match(field)),
        }
    }
}

/// A `TrackQuery` compiled once: build it per query, not per track.
#[derive(Debug, Clone)]
pub struct TrackMatcher {
    query: TrackQuery,
    any_text: Option<StringMatcher>,
    artist: Option<StringMatcher>,
    title: Option<StringMatcher>,
    album: Option<StringMatcher>,
    label: Option<StringMatcher>,
    genre: Option<StringMatcher>,
    style: Option<StringMatcher>,
}

impl TrackMatcher {
    pub fn new(query: &TrackQuery) -> Self {
        let compile = |q: &Option<StringQuery>| q.as_ref().map(StringMatcher::new);
        Self {
            any_text: compile(&query.any_text),
            artist: compile(&query.artist),
            title: compile(&query.title),
            album: compile(&query.album),
            label: compile(&query.label),
            genre: compile(&query.genre),
            style: compile(&query.style),
            query: query.clone(),
        }
    }

    /// Evaluate against a track's fields, respecting `query.not`.
    pub fn matches(&self, track: &TrackFields) -> bool {
        let matched = self.matches_inner(track);
        if self.query.not {
            !matched
        } else {
            matched
        }
    }

    /// Returns `true` only if ALL non-None query fields match.
    /// `any_text` uses OR semantics across title/artist/album/label/genre.
    fn matches_inner(&self, track: &TrackFields) -> bool {
        let query = &self.query;
        if let Some(m) = &self.any_text {
            let text_fields = [
                track.title,
                track.artist,
                track.album,
                track.label,
                track.genre,
            ];
            if !text_fields.iter().any(|f| f.is_some_and(|s| m.matches(s))) {
                return false;
            }
        }

        if !check_text(self.artist.as_ref(), track.artist)
            || !check_text(self.title.as_ref(), track.title)
            || !check_text(self.album.as_ref(), track.album)
            || !check_text(self.label.as_ref(), track.label)
            || !check_text(self.genre.as_ref(), track.genre)
        {
            return false;
        }

        if let Some(m) = &self.style {
            if !track.styles.iter().any(|s| m.matches(s.as_str())) {
                return false;
            }
        }

        if let Some(nq) = &query.bpm {
            if !track.bpm.is_some_and(|b| matches_numeric(nq, b)) {
                return false;
            }
        }

        if let Some(nq) = &query.year {
            if !track.year.is_some_and(|y| matches_numeric(nq, y as f32)) {
                return false;
            }
        }

        if let Some(dq) = &query.duration {
            if !track.duration.is_some_and(|d| matches_duration(dq, d)) {
                return false;
            }
        }

        if let Some(kq) = &query.key {
            if !track.key.is_some_and(|k| kq.matches(k)) {
                return false;
            }
        }

        if let Some(src) = &query.source {
            if !track.source.is_some_and(|s| s.eq_ignore_ascii_case(src)) {
                return false;
            }
        }

        if let Some(q) = &query.started {
            if !matches_date(q, track.last_started) {
                return false;
            }
        }

        if let Some(q) = &query.stopped {
            if !matches_date(q, track.last_stopped) {
                return false;
            }
        }

        if let Some(q) = &query.added {
            if !matches_date(q, track.added) {
                return false;
            }
        }

        if let Some(role) = query.role {
            if !track.role.is_some_and(|r| r == role) {
                return false;
            }
        }

        if let Some(nq) = &query.energy {
            if !track.energy.is_some_and(|e| matches_numeric(nq, e as f32)) {
                return false;
            }
        }

        if let Some(mark) = &query.mark {
            if !track.marks.contains(mark) {
                return false;
            }
        }

        true
    }
}

/// Returns true if there is no string query constraint, or if the track field matches.
fn check_text(matcher: Option<&StringMatcher>, track_field: Option<&str>) -> bool {
    match matcher {
        None => true,
        Some(m) => track_field.is_some_and(|s| m.matches(s)),
    }
}

/// Split a CamelCase string into its word segments.
///
/// `"CarbBasedLife"` → `["Carb", "Based", "Life"]`
fn split_camel_case(s: &str) -> Vec<String> {
    let mut segments: Vec<String> = vec![];
    let mut current = String::new();

    for c in s.chars() {
        if c.is_uppercase() && !current.is_empty() {
            segments.push(current.clone());
            current = String::new();
        }
        current.push(c);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Build a regex pattern from a CamelCase initialism.
///
/// `"CarbBased"` → `(?i)\bCarb\w*\b.*\bBased\w*\b`
fn build_initialism_regex(s: &str) -> String {
    let segments = split_camel_case(s);
    let parts: Vec<String> = segments
        .iter()
        .map(|seg| format!(r"\b{}\w*\b", regex::escape(seg)))
        .collect();
    format!("(?i){}", parts.join(".*"))
}

fn matches_numeric(query: &NumericQuery, value: f32) -> bool {
    match query {
        NumericQuery::Exact(v) => (value - v).abs() < 0.5,
        NumericQuery::Range(lo, hi) => value >= *lo && value <= *hi,
        NumericQuery::Tolerance {
            value: center,
            up,
            down,
        } => value >= center - down && value <= center + up,
    }
}

fn matches_duration(query: &DurationQuery, secs: u32) -> bool {
    match query {
        DurationQuery::Exact(target) => secs == *target,
        DurationQuery::AtLeast(min) => secs >= *min,
        DurationQuery::AtMost(max) => secs < *max,
        DurationQuery::Range(lo, hi) => secs >= *lo && secs <= *hi,
        DurationQuery::WithPrecision(base, unit) => {
            let bucket = match unit {
                DurationUnit::Hours => 3600,
                DurationUnit::Minutes => 60,
                DurationUnit::Seconds => 1,
            };
            secs >= *base && secs < base + bucket
        }
    }
}

/// Evaluate a `DateQuery` against an optional UTC timestamp.
///
/// - `NA`: matches when `val` is `None`
/// - `After(date, prec)`: matches when val > end_of_period(date, prec)
/// - `Before(date, prec)`: matches when val < start_of_period(date, prec)
/// - `Range(lo, lo_prec, hi, hi_prec)`: matches when start_of_period(lo, lo_prec) <= val <= end_of_period(hi, hi_prec)
fn matches_date(query: &DateQuery, val: Option<DateTime<Utc>>) -> bool {
    match query {
        DateQuery::NA => val.is_none(),
        DateQuery::After(date, prec) => {
            let Some(ts) = val else { return false };
            let end = period_end(*date, *prec);
            ts.date_naive() > end
        }
        DateQuery::Before(date, prec) => {
            let Some(ts) = val else { return false };
            let start = period_start(*date, *prec);
            ts.date_naive() < start
        }
        DateQuery::Range(lo, lo_prec, hi, hi_prec) => {
            let Some(ts) = val else { return false };
            let start = period_start(*lo, *lo_prec);
            let end = period_end(*hi, *hi_prec);
            let d = ts.date_naive();
            d >= start && d <= end
        }
    }
}

/// Returns the first day of the period described by `date` at `prec`.
fn period_start(date: NaiveDate, prec: DatePrecision) -> NaiveDate {
    match prec {
        DatePrecision::Year => NaiveDate::from_ymd_opt(date.year(), 1, 1)
            .expect("period_start: invalid date calculation"),
        DatePrecision::YearMonth => NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .expect("period_start: invalid date calculation"),
        DatePrecision::YearMonthDay => date,
    }
}

/// Returns the last day of the period described by `date` at `prec`.
fn period_end(date: NaiveDate, prec: DatePrecision) -> NaiveDate {
    match prec {
        DatePrecision::Year => NaiveDate::from_ymd_opt(date.year(), 12, 31)
            .expect("period_end: invalid date calculation"),
        DatePrecision::YearMonth => {
            // Last day of the month: go to first day of next month, subtract 1
            let (y, m) = if date.month() == 12 {
                (date.year() + 1, 1)
            } else {
                (date.year(), date.month() + 1)
            };
            NaiveDate::from_ymd_opt(y, m, 1)
                .expect("period_end: invalid date calculation")
                .pred_opt()
                .expect("period_end: invalid date calculation")
        }
        DatePrecision::YearMonthDay => date,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{DatePrecision, DateQuery, NumericQuery, StringQuery, TrackQuery};
    use rstest::rstest;

    fn empty_fields() -> TrackFields<'static> {
        TrackFields {
            title: None,
            artist: None,
            album: None,
            label: None,
            genre: None,
            styles: &[],
            bpm: None,
            key: None,
            duration: None,
            year: None,
            source: None,
            last_started: None,
            last_stopped: None,
            added: None,
            role: None,
            energy: None,
            marks: Vec::new(),
        }
    }

    fn sm(q: &str) -> StringMatcher {
        StringMatcher::new(&crate::parse::parse_string_query(q))
    }

    #[test]
    fn upper_case_initialism_parses_and_matches_the_artist() {
        let q = crate::parse::parse_string_query("CBL");
        assert!(matches!(q, StringQuery::Initialism(_)));
        let query = TrackQuery {
            any_text: Some(q),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Carbon Based Lifeforms");
        assert!(TrackMatcher::new(&query).matches(&fields));
        fields.artist = Some("Rymden");
        assert!(!TrackMatcher::new(&query).matches(&fields));
    }

    #[test]
    fn lower_case_cbl_is_contains_and_does_not_match() {
        let q = crate::parse::parse_string_query("cbl");
        assert!(matches!(q, StringQuery::Contains(_)));
        let query = TrackQuery {
            any_text: Some(q),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Carbon Based Lifeforms");
        assert!(!TrackMatcher::new(&query).matches(&fields));
    }

    #[test]
    fn regex_matches_through_the_compiled_matcher() {
        let q = crate::parse::parse_string_query("/^(?i)carbon/");
        assert!(matches!(q, StringQuery::Regex(_)));
        let query = TrackQuery {
            title: Some(q),
            ..Default::default()
        };
        let matcher = TrackMatcher::new(&query);
        let mut fields = empty_fields();
        fields.title = Some("Carbon Based Lifeforms");
        assert!(matcher.matches(&fields));
        fields.title = Some("Rymden carbon");
        assert!(!matcher.matches(&fields));
    }

    #[test]
    fn invalid_regex_matches_nothing() {
        assert!(!sm("/(/").matches("anything"));
    }

    #[rstest]
    #[case("CBL", "Carbon Based Lifeforms", true)]
    #[case("CBL", "Rymden", false)]
    #[case("cbl", "Carbon Based Lifeforms", false)]
    #[case("carbon based", "Carbon Based Lifeforms", true)]
    #[case("carbon rymden", "Carbon Based Lifeforms", false)]
    #[case("/^carbon/", "Carbon Based Lifeforms", false)]
    #[case("/^(?i)carbon/", "Carbon Based Lifeforms", true)]
    fn string_matcher_rules(#[case] q: &str, #[case] text: &str, #[case] expected: bool) {
        assert_eq!(sm(q).matches(text), expected, "{q} vs {text}");
    }

    #[rstest]
    #[case("Carbon Based", "carbon based lifeforms", true)]
    #[case("CARBON", "carbon based lifeforms", true)]
    #[case("Carbon Rymden", "carbon based lifeforms", false)]
    fn contains_ignores_case_of_query_words(
        #[case] q: &str,
        #[case] text: &str,
        #[case] expected: bool,
    ) {
        let sq = StringQuery::Contains(q.to_string());
        assert_eq!(
            StringMatcher::new(&sq).matches(text),
            expected,
            "{q} vs {text}"
        );
    }

    #[test]
    fn string_matcher_cbl_matches_playlist_name() {
        assert!(sm("CBL").matches("Carbon Based Lifeforms"));
        assert!(!sm("cbl").matches("Carbon Based Lifeforms"));
    }

    fn named(name: &str) -> Mark {
        name.parse().unwrap()
    }

    #[test]
    fn a_mark_query_requires_that_mark_to_be_live() {
        let query = |mark: Mark| TrackQuery {
            mark: Some(mark),
            ..Default::default()
        };
        let mut fields = empty_fields();
        assert!(!matches_query(&query(Mark::Default), &fields));
        fields.marks = vec![Mark::Default, named("banger")];
        assert!(matches_query(&query(Mark::Default), &fields));
        assert!(matches_query(&query(named("banger")), &fields));
        assert!(!matches_query(&query(named("opener")), &fields));
    }

    #[test]
    fn a_mark_query_inverts_with_not() {
        let query = TrackQuery {
            mark: Some(named("banger")),
            not: true,
            ..Default::default()
        };
        let mut fields = empty_fields();
        assert!(matches_query(&query, &fields));
        fields.marks = vec![named("banger")];
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn a_query_without_mark_decodes_from_old_json() {
        let query: TrackQuery = serde_json::from_str(r#"{"not":false}"#).unwrap();
        assert_eq!(query.mark, None);
        assert!(query.is_empty());
    }

    #[test]
    fn a_query_with_an_unknown_field_is_rejected() {
        assert!(serde_json::from_str::<TrackQuery>(r#"{"not":false,"future":1}"#).is_err());
    }

    #[test]
    fn a_mark_query_round_trips_and_is_not_empty() {
        let query = TrackQuery {
            mark: Some(named("banger")),
            ..Default::default()
        };
        assert!(!query.is_empty());
        let json = serde_json::to_string(&query).unwrap();
        let back: TrackQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mark, Some(named("banger")));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches_query(&TrackQuery::default(), &empty_fields()));
    }

    #[test]
    fn contains_match() {
        let query = TrackQuery {
            artist: Some(StringQuery::Contains("carbon based".to_string())),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Carbon Based Lifeforms");
        assert!(matches_query(&query, &fields));
    }

    #[test]
    fn contains_no_match() {
        let query = TrackQuery {
            artist: Some(StringQuery::Contains("rymden".to_string())),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Carbon Based Lifeforms");
        assert!(!matches_query(&query, &fields));
    }

    #[rstest]
    #[case("Carbon Based Lifeforms", true)]
    #[case("Carbon Only", false)]
    fn contains_all_words_required(#[case] field: &str, #[case] expected: bool) {
        let sq = StringQuery::Contains("carbon lifeforms".to_string());
        assert_eq!(StringMatcher::new(&sq).matches(field), expected);
    }

    #[rstest]
    #[case("Carbon Based Lifeforms", true)]
    #[case("carbon based lifeforms", true)]
    #[case("Rymden Vild och Vacker", false)]
    fn initialism_match(#[case] field: &str, #[case] expected: bool) {
        let sq = StringQuery::Initialism("CarbBased".to_string());
        assert_eq!(StringMatcher::new(&sq).matches(field), expected);
    }

    #[rstest]
    #[case("Carbon Based Lifeforms", true)]
    #[case("Rymden", false)]
    fn regex_match(#[case] field: &str, #[case] expected: bool) {
        let sq = StringQuery::Regex("^Carbon.*".to_string());
        assert_eq!(StringMatcher::new(&sq).matches(field), expected);
    }

    #[test]
    fn bpm_exact_match() {
        let query = TrackQuery {
            bpm: Some(NumericQuery::Exact(128.0)),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.bpm = Some(128.0);
        assert!(matches_query(&query, &fields));
    }

    #[rstest]
    #[case(130.0, true)]
    #[case(133.0, false)]
    fn bpm_tolerance_match(#[case] bpm: f32, #[case] expected: bool) {
        let query = TrackQuery {
            bpm: Some(NumericQuery::Tolerance {
                value: 128.0,
                up: 4.0,
                down: 4.0,
            }),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.bpm = Some(bpm);
        assert_eq!(matches_query(&query, &fields), expected);
    }

    #[rstest]
    #[case("artist", "Rymden", true)]
    #[case("title", "Rymden - Something", true)]
    #[case("album", "Rymden Live", true)]
    #[case("label", "Rymden Records", true)]
    #[case("genre", "Rymden Jazz", true)]
    #[case("style", "Rymden", false)]
    #[case("title", "Carbon Based Lifeforms", false)]
    fn any_text_or_semantics(
        #[case] field: &str,
        #[case] value: &'static str,
        #[case] expected: bool,
    ) {
        let query = TrackQuery {
            any_text: Some(StringQuery::Contains("rymden".to_string())),
            ..Default::default()
        };
        let styles = vec![value.to_string()];
        let mut fields = empty_fields();
        match field {
            "artist" => fields.artist = Some(value),
            "title" => fields.title = Some(value),
            "album" => fields.album = Some(value),
            "label" => fields.label = Some(value),
            "genre" => fields.genre = Some(value),
            "style" => fields.styles = &styles,
            other => panic!("unknown field {other}"),
        }
        assert_eq!(matches_query(&query, &fields), expected, "{field}: {value}");
    }

    #[test]
    fn style_matches_any() {
        let query = TrackQuery {
            style: Some(StringQuery::Contains("driving".to_string())),
            ..Default::default()
        };
        let mut fields = empty_fields();
        let styles = vec!["Peak Time".to_string(), "Driving".to_string()];
        fields.styles = &styles;
        assert!(matches_query(&query, &fields));
    }

    #[test]
    fn and_semantics_all_must_match() {
        let query = TrackQuery {
            artist: Some(StringQuery::Contains("carbon".to_string())),
            bpm: Some(NumericQuery::Exact(140.0)),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Carbon Based Lifeforms");
        fields.bpm = Some(128.0);
        assert!(!matches_query(&query, &fields)); // bpm doesn't match

        fields.bpm = Some(140.0);
        assert!(matches_query(&query, &fields)); // both match
    }

    #[test]
    fn started_na_matches_none() {
        let q = TrackQuery {
            started: Some(DateQuery::NA),
            ..Default::default()
        };
        let mut f = empty_fields();
        f.last_started = None;
        assert!(matches_query(&q, &f));
    }

    #[test]
    fn started_na_no_match_when_started() {
        let q = TrackQuery {
            started: Some(DateQuery::NA),
            ..Default::default()
        };
        let mut f = empty_fields();
        f.last_started = Some(DateTime::from_timestamp(0, 0).unwrap());
        assert!(!matches_query(&q, &f));
    }

    #[test]
    fn started_after_year_month() {
        use chrono::NaiveDate;
        // After January 2026 — matches February onwards
        let q = TrackQuery {
            started: Some(DateQuery::After(
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                DatePrecision::YearMonth,
            )),
            ..Default::default()
        };
        let mut f = empty_fields();
        // Started on 2026-02-15
        f.last_started = Some(
            chrono::DateTime::parse_from_rfc3339("2026-02-15T12:00:00Z")
                .unwrap()
                .into(),
        );
        assert!(matches_query(&q, &f));
    }

    #[test]
    fn started_after_no_match_within_period() {
        use chrono::NaiveDate;
        let q = TrackQuery {
            started: Some(DateQuery::After(
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                DatePrecision::YearMonth,
            )),
            ..Default::default()
        };
        let mut f = empty_fields();
        // Started on 2026-01-15 — inside January, NOT after it
        f.last_started = Some(
            chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                .unwrap()
                .into(),
        );
        assert!(!matches_query(&q, &f));
    }

    #[test]
    fn started_range_year_month() {
        use chrono::NaiveDate;
        let q = TrackQuery {
            started: Some(DateQuery::Range(
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                DatePrecision::YearMonth,
                NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
                DatePrecision::YearMonth,
            )),
            ..Default::default()
        };
        let mut f = empty_fields();
        // Started on 2026-02-15 — within Jan-Mar 2026
        f.last_started = Some(
            chrono::DateTime::parse_from_rfc3339("2026-02-15T12:00:00Z")
                .unwrap()
                .into(),
        );
        assert!(matches_query(&q, &f));
    }

    #[rstest]
    #[case("Carbon Based Lifeforms", false)]
    #[case("Rymden", true)]
    fn not_flag_inverts_result(#[case] artist: &str, #[case] expected: bool) {
        let query = TrackQuery {
            artist: Some(StringQuery::Contains("carbon".to_string())),
            not: true,
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some(artist);
        assert_eq!(matches_query(&query, &fields), expected);
    }

    #[test]
    fn not_false_preserves_normal_match() {
        let query = TrackQuery {
            artist: Some(StringQuery::Contains("carbon".to_string())),
            not: false,
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Carbon Based Lifeforms");
        assert!(matches_query(&query, &fields));
    }

    #[test]
    fn not_with_empty_query_matches_nothing() {
        let query = TrackQuery {
            not: true,
            ..Default::default()
        };
        let fields = empty_fields();
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn added_na_matches_none() {
        let q = TrackQuery {
            added: Some(DateQuery::NA),
            ..Default::default()
        };
        let mut f = empty_fields();
        f.added = None;
        assert!(matches_query(&q, &f));
    }

    #[test]
    fn added_na_no_match_when_added() {
        let q = TrackQuery {
            added: Some(DateQuery::NA),
            ..Default::default()
        };
        let mut f = empty_fields();
        f.added = Some(DateTime::from_timestamp(0, 0).unwrap());
        assert!(!matches_query(&q, &f));
    }

    #[test]
    fn added_after_year_matches() {
        use chrono::NaiveDate;
        let q = TrackQuery {
            added: Some(DateQuery::After(
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                DatePrecision::Year,
            )),
            ..Default::default()
        };
        let mut f = empty_fields();
        f.added = Some(
            chrono::DateTime::parse_from_rfc3339("2027-01-01T00:00:00Z")
                .unwrap()
                .into(),
        );
        assert!(matches_query(&q, &f));
    }

    // =========================================================================
    // Role query tests
    // =========================================================================

    #[test]
    fn role_exact_match() {
        use music_primitives::TrackRole;
        let query = TrackQuery {
            role: Some(TrackRole::Peak),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.role = Some(TrackRole::Peak);
        assert!(matches_query(&query, &fields));
    }

    #[test]
    fn role_no_match_different_role() {
        use music_primitives::TrackRole;
        let query = TrackQuery {
            role: Some(TrackRole::Peak),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.role = Some(TrackRole::Opener);
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn role_no_match_when_role_absent() {
        use music_primitives::TrackRole;
        let query = TrackQuery {
            role: Some(TrackRole::Banger),
            ..Default::default()
        };
        let fields = empty_fields(); // role is None
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn role_not_inverts_match() {
        use music_primitives::TrackRole;
        let query = TrackQuery {
            role: Some(TrackRole::Closer),
            not: true,
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.role = Some(TrackRole::Closer);
        // not=true + matching role → false
        assert!(!matches_query(&query, &fields));
        // not=true + non-matching role → true
        fields.role = Some(TrackRole::Opener);
        assert!(matches_query(&query, &fields));
    }

    // =========================================================================
    // Energy query tests
    // =========================================================================

    #[test]
    fn key_query_matches_real_key_not_display_string() {
        // The projection stores "A Minor" (display); the query says 8A (Camelot).
        let mut fields = empty_fields();
        fields.key = Some("A Minor".parse().unwrap());
        let query = |q: &str| TrackQuery {
            key: Some(q.parse().unwrap()),
            ..Default::default()
        };
        assert!(matches_query(&query("8A"), &fields));
        assert!(matches_query(&query("8A+-1"), &fields));
        assert!(matches_query(&query("6A..10B"), &fields));
        assert!(!matches_query(&query("8B"), &fields));
        assert!(!matches_query(&query("1A"), &fields));
        assert!(!matches_query(&query("8A"), &empty_fields()));
    }

    #[test]
    fn energy_exact_match() {
        let query = TrackQuery {
            energy: Some(NumericQuery::Exact(7.0)),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.energy = Some(7);
        assert!(matches_query(&query, &fields));
    }

    #[test]
    fn energy_exact_no_match() {
        let query = TrackQuery {
            energy: Some(NumericQuery::Exact(7.0)),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.energy = Some(5);
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn energy_range_match() {
        let query = TrackQuery {
            energy: Some(NumericQuery::Range(5.0, 9.0)),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.energy = Some(7);
        assert!(matches_query(&query, &fields));
        fields.energy = Some(5);
        assert!(matches_query(&query, &fields));
        fields.energy = Some(9);
        assert!(matches_query(&query, &fields));
        fields.energy = Some(4);
        assert!(!matches_query(&query, &fields));
        fields.energy = Some(10);
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn energy_tolerance_match() {
        let query = TrackQuery {
            energy: Some(NumericQuery::Tolerance {
                value: 7.0,
                up: 2.0,
                down: 2.0,
            }),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.energy = Some(7);
        assert!(matches_query(&query, &fields));
        fields.energy = Some(5);
        assert!(matches_query(&query, &fields));
        fields.energy = Some(9);
        assert!(matches_query(&query, &fields));
        fields.energy = Some(4);
        assert!(!matches_query(&query, &fields));
        fields.energy = Some(10);
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn energy_no_match_when_absent() {
        let query = TrackQuery {
            energy: Some(NumericQuery::Exact(7.0)),
            ..Default::default()
        };
        let fields = empty_fields(); // energy is None
        assert!(!matches_query(&query, &fields));
    }

    #[rstest]
    #[case(TrackQuery { role: Some(music_primitives::TrackRole::Banger), ..Default::default() }, false)]
    #[case(TrackQuery { energy: Some(NumericQuery::Exact(5.0)), ..Default::default() }, false)]
    fn is_empty_role_energy(#[case] query: TrackQuery, #[case] is_empty: bool) {
        assert_eq!(query.is_empty(), is_empty);
    }
}
