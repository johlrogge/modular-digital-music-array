use crate::query::{
    CamelotLetter, DurationQuery, DurationUnit, KeyQuery, NumericQuery, StringQuery, TrackQuery,
};
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
    /// Key stored as display string (e.g. "8B", "Am").
    pub key: Option<&'a str>,
    /// Duration in seconds.
    pub duration: Option<u32>,
    pub year: Option<u32>,
    pub source: Option<&'a str>,
}

/// Evaluate a `TrackQuery` against a set of track fields.
///
/// Returns `true` only if ALL non-None query fields match.
/// `any_text` uses OR semantics across title/artist/album/label/genre.
pub fn matches_query(query: &TrackQuery, track: &TrackFields) -> bool {
    if let Some(sq) = &query.any_text {
        let text_fields = [
            track.title,
            track.artist,
            track.album,
            track.label,
            track.genre,
        ];
        if !text_fields
            .iter()
            .any(|f| f.map_or(false, |s| matches_string(sq, s)))
        {
            return false;
        }
    }

    if let Some(sq) = &query.artist {
        if !track.artist.map_or(false, |s| matches_string(sq, s)) {
            return false;
        }
    }

    if let Some(sq) = &query.title {
        if !track.title.map_or(false, |s| matches_string(sq, s)) {
            return false;
        }
    }

    if let Some(sq) = &query.album {
        if !track.album.map_or(false, |s| matches_string(sq, s)) {
            return false;
        }
    }

    if let Some(sq) = &query.label {
        if !track.label.map_or(false, |s| matches_string(sq, s)) {
            return false;
        }
    }

    if let Some(sq) = &query.genre {
        if !track.genre.map_or(false, |s| matches_string(sq, s)) {
            return false;
        }
    }

    if let Some(sq) = &query.style {
        if !track.styles.iter().any(|s| matches_string(sq, s.as_str())) {
            return false;
        }
    }

    if let Some(nq) = &query.bpm {
        if !track.bpm.map_or(false, |b| matches_numeric(nq, b)) {
            return false;
        }
    }

    if let Some(nq) = &query.year {
        if !track.year.map_or(false, |y| matches_numeric(nq, y as f32)) {
            return false;
        }
    }

    if let Some(dq) = &query.duration {
        if !track.duration.map_or(false, |d| matches_duration(dq, d)) {
            return false;
        }
    }

    if let Some(kq) = &query.key {
        if !track.key.map_or(false, |k| matches_key(kq, k)) {
            return false;
        }
    }

    if let Some(src) = &query.source {
        if !track.source.map_or(false, |s| s.eq_ignore_ascii_case(src)) {
            return false;
        }
    }

    true
}

fn matches_string(query: &StringQuery, field: &str) -> bool {
    match query {
        StringQuery::Contains(q) => {
            let field_lower = field.to_lowercase();
            q.split_whitespace()
                .all(|word| field_lower.contains(&word.to_lowercase()))
        }
        StringQuery::Initialism(q) => {
            let pattern = build_initialism_regex(q);
            Regex::new(&pattern)
                .map(|re| re.is_match(field))
                .unwrap_or(false)
        }
        StringQuery::Regex(pattern) => Regex::new(pattern)
            .map(|re| re.is_match(field))
            .unwrap_or(false),
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

/// Key matching stub: compares the Camelot string against the stored key display string.
///
/// Full Camelot tolerance math (circular arithmetic on the wheel) is deferred.
fn matches_key(query: &KeyQuery, key_str: &str) -> bool {
    let (number, letter) = match query {
        KeyQuery::Exact { number, letter } => (number, letter),
        KeyQuery::Tolerance { number, letter, .. } => (number, letter),
    };
    let target = format!(
        "{}{}",
        number,
        match letter {
            CamelotLetter::A => "A",
            CamelotLetter::B => "B",
        }
    );
    key_str.eq_ignore_ascii_case(&target) || key_str.to_uppercase().contains(&target.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{NumericQuery, StringQuery, TrackQuery};

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
        }
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

    #[test]
    fn contains_all_words_required() {
        let sq = StringQuery::Contains("carbon lifeforms".to_string());
        assert!(matches_string(&sq, "Carbon Based Lifeforms"));
        assert!(!matches_string(&sq, "Carbon Only"));
    }

    #[test]
    fn initialism_match() {
        let sq = StringQuery::Initialism("CarbBased".to_string());
        assert!(matches_string(&sq, "Carbon Based Lifeforms"));
        assert!(matches_string(&sq, "carbon based lifeforms"));
        assert!(!matches_string(&sq, "Rymden Vild och Vacker"));
    }

    #[test]
    fn regex_match() {
        let sq = StringQuery::Regex("^Carbon.*".to_string());
        assert!(matches_string(&sq, "Carbon Based Lifeforms"));
        assert!(!matches_string(&sq, "Rymden"));
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

    #[test]
    fn bpm_tolerance_match() {
        let query = TrackQuery {
            bpm: Some(NumericQuery::Tolerance {
                value: 128.0,
                up: 4.0,
                down: 4.0,
            }),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.bpm = Some(130.0);
        assert!(matches_query(&query, &fields));
        fields.bpm = Some(133.0);
        assert!(!matches_query(&query, &fields));
    }

    #[test]
    fn any_text_or_semantics() {
        let query = TrackQuery {
            any_text: Some(StringQuery::Contains("rymden".to_string())),
            ..Default::default()
        };
        let mut fields = empty_fields();
        fields.artist = Some("Rymden");
        assert!(matches_query(&query, &fields));

        fields.artist = None;
        fields.title = Some("Rymden - Something");
        assert!(matches_query(&query, &fields));

        fields.title = Some("Carbon Based Lifeforms");
        assert!(!matches_query(&query, &fields));
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
}
