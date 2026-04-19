use library_search::{
    parse_date_query, parse_duration_query, parse_key_query, parse_numeric_query,
    parse_string_query, StringQuery, TrackQuery,
};

/// Parse a simple query language into a `TrackQuery`.
///
/// Grammar:
/// - `field 'quoted value'`  — single-quoted value (may contain spaces)
/// - `field "quoted value"`  — double-quoted value (may contain spaces)
/// - `field value`           — unquoted single word
/// - `bareword`              — no recognised field prefix → added to `any_text`
///
/// Recognised field names: artist, title, album, label, genre, style, bpm,
/// key, duration, year, source, added, started, stopped.
///
/// Unknown field names fall back to bareword handling (value added to any_text).
/// Unparseable field values also fall back to bareword handling.
/// Never panics, never returns Err — always produces *some* query.
pub fn parse_query(input: &str) -> TrackQuery {
    let mut query = TrackQuery::default();
    let mut any_text_parts: Vec<String> = Vec::new();

    let mut remaining = input.trim();

    while !remaining.is_empty() {
        // Extract the next field name token (up to whitespace).
        let (field_token, after_field) = split_next_token(remaining);
        if field_token.is_empty() {
            break;
        }

        let after_field_trimmed = after_field.trim_start();

        // Check whether the next character starts a value (quoted or unquoted).
        // If there is no more input after the field token, treat field as bareword.
        if after_field_trimmed.is_empty() {
            // No value follows — treat field_token as a bareword.
            any_text_parts.push(field_token.to_string());
            remaining = after_field_trimmed;
            continue;
        }

        // Peek at whether this looks like a known field followed by a value.
        let is_known = is_known_field(field_token);

        if !is_known {
            // Unknown field: treat the whole "field value" pair as barewords.
            // We read the value too so we don't lose it.
            let (value_token, after_value) = split_next_value(after_field_trimmed);
            any_text_parts.push(field_token.to_string());
            if !value_token.is_empty() {
                any_text_parts.push(value_token.to_string());
            }
            remaining = after_value.trim_start();
            continue;
        }

        // Known field — extract the value.
        let (value, after_value) = split_next_value(after_field_trimmed);

        if value.is_empty() {
            // Value was empty (e.g. field at end of string) — treat field as bareword.
            any_text_parts.push(field_token.to_string());
            remaining = after_field_trimmed;
            continue;
        }

        // Apply the value to the appropriate query field.
        let applied = apply_field(&mut query, field_token, &value);
        if !applied {
            // Parsing failed for a known field — fall back both tokens to any_text.
            any_text_parts.push(field_token.to_string());
            any_text_parts.push(value.clone());
        }

        remaining = after_value.trim_start();
    }

    if !any_text_parts.is_empty() {
        let combined = any_text_parts.join(" ");
        query.any_text = Some(StringQuery::Contains(combined));
    }

    query
}

/// Returns true if `name` is a recognised field keyword.
fn is_known_field(name: &str) -> bool {
    matches!(
        name,
        "artist"
            | "title"
            | "album"
            | "label"
            | "genre"
            | "style"
            | "bpm"
            | "key"
            | "duration"
            | "year"
            | "source"
            | "added"
            | "started"
            | "stopped"
    )
}

/// Apply a (field, value) pair to the query.
/// Returns false if the value could not be parsed for the field.
fn apply_field(query: &mut TrackQuery, field: &str, value: &str) -> bool {
    match field {
        "artist" => {
            query.artist = Some(parse_string_query(value));
            true
        }
        "title" => {
            query.title = Some(parse_string_query(value));
            true
        }
        "album" => {
            query.album = Some(parse_string_query(value));
            true
        }
        "label" => {
            query.label = Some(parse_string_query(value));
            true
        }
        "genre" => {
            query.genre = Some(parse_string_query(value));
            true
        }
        "style" => {
            query.style = Some(parse_string_query(value));
            true
        }
        "source" => {
            query.source = Some(value.to_string());
            true
        }
        "bpm" => match parse_numeric_query(value) {
            Ok(q) => {
                query.bpm = Some(q);
                true
            }
            Err(_) => false,
        },
        "year" => match parse_numeric_query(value) {
            Ok(q) => {
                query.year = Some(q);
                true
            }
            Err(_) => false,
        },
        "key" => match parse_key_query(value) {
            Ok(q) => {
                query.key = Some(q);
                true
            }
            Err(_) => false,
        },
        "duration" => match parse_duration_query(value) {
            Ok(q) => {
                query.duration = Some(q);
                true
            }
            Err(_) => false,
        },
        "added" => match parse_date_query(value) {
            Ok(q) => {
                query.added = Some(q);
                true
            }
            Err(_) => false,
        },
        "started" => match parse_date_query(value) {
            Ok(q) => {
                query.started = Some(q);
                true
            }
            Err(_) => false,
        },
        "stopped" => match parse_date_query(value) {
            Ok(q) => {
                query.stopped = Some(q);
                true
            }
            Err(_) => false,
        },
        _ => false,
    }
}

/// Split off the first whitespace-delimited token from `s`.
/// Returns (token, remainder_after_token).
fn split_next_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], &s[idx..]),
        None => (s, ""),
    }
}

/// Split off the next value from `s`.
///
/// - If `s` starts with `'` or `"`, reads until the matching closing quote
///   (or end of string if unterminated — graceful fallback).
/// - Otherwise reads the next whitespace-delimited word.
///
/// Returns (value_string, remainder_after_value).
fn split_next_value(s: &str) -> (String, &str) {
    let s = s.trim_start();
    if s.is_empty() {
        return (String::new(), s);
    }

    let first = s.as_bytes()[0];
    if first == b'\'' || first == b'"' {
        let quote = first as char;
        let inner = &s[1..];
        match inner.find(quote) {
            Some(end) => (inner[..end].to_string(), &inner[end + 1..]),
            // Unterminated quote — consume the rest of the string as the value.
            None => (inner.to_string(), ""),
        }
    } else {
        match s.find(char::is_whitespace) {
            Some(idx) => (s[..idx].to_string(), &s[idx..]),
            None => (s.to_string(), ""),
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use library_search::{DateQuery, NumericQuery, StringQuery};

    fn contains_str(q: &Option<StringQuery>) -> Option<&str> {
        match q {
            Some(StringQuery::Contains(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    // --- parse_query tests ---

    #[test]
    fn empty_input_returns_default_query() {
        let q = parse_query("");
        assert!(q.is_empty());
    }

    #[test]
    fn whitespace_only_returns_default_query() {
        let q = parse_query("   ");
        assert!(q.is_empty());
    }

    #[test]
    fn bareword_sets_any_text() {
        let q = parse_query("dest");
        assert_eq!(contains_str(&q.any_text), Some("dest"));
        assert!(q.artist.is_none());
    }

    #[test]
    fn artist_single_quoted() {
        let q = parse_query("artist 'bonobo'");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "bonobo"
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_double_quoted() {
        let q = parse_query(r#"artist "bonobo""#);
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "bonobo"
        ));
    }

    #[test]
    fn artist_unquoted_single_word() {
        let q = parse_query("artist bonobo");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "bonobo"
        ));
    }

    #[test]
    fn artist_with_spaces_in_quotes() {
        let q = parse_query("artist 'carbon based lifeforms'");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "carbon based lifeforms"
        ));
    }

    #[test]
    fn added_date_expression() {
        // "-7" is a date expression meaning "7 days ago"
        let q = parse_query("added '-7'");
        assert!(q.added.is_some(), "added should be set");
        assert!(q.any_text.is_none());
        // Verify it parsed as a date range (date_expression resolves -7 to a specific day)
        assert!(matches!(q.added.unwrap(), DateQuery::Range(..)));
    }

    #[test]
    fn bpm_exact() {
        let q = parse_query("bpm '120'");
        assert!(matches!(
            q.bpm,
            Some(NumericQuery::Exact(v)) if (v - 120.0).abs() < 0.01
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn bpm_range() {
        let q = parse_query("bpm '120..130'");
        assert!(matches!(
            q.bpm,
            Some(NumericQuery::Range(lo, hi)) if lo == 120.0 && hi == 130.0
        ));
    }

    #[test]
    fn genre_field() {
        let q = parse_query("genre 'ambient'");
        assert!(matches!(
            &q.genre,
            Some(StringQuery::Contains(s)) if s == "ambient"
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_and_bpm_combined() {
        let q = parse_query("artist 'bonobo' bpm '120'");
        assert!(q.artist.is_some());
        assert!(q.bpm.is_some());
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_and_bareword() {
        let q = parse_query("artist 'bonobo' dest");
        assert!(q.artist.is_some());
        assert_eq!(contains_str(&q.any_text), Some("dest"));
    }

    #[test]
    fn unknown_field_falls_back_to_any_text() {
        // "typo" is not a known field — both tokens go to any_text
        let q = parse_query("unknown 'foo'");
        // The implementation joins "unknown" and "foo" into any_text
        let text = contains_str(&q.any_text).expect("any_text should be set");
        // Both the field name and value should be captured somehow
        assert!(text.contains("unknown") || text.contains("foo"));
    }

    #[test]
    fn unterminated_quote_no_panic() {
        // Should not panic; falls back gracefully
        let q = parse_query("artist 'unterminated");
        // The value is whatever remains after the quote — could be artist or any_text
        // Either way the function must not panic and must return something.
        let _ = q; // just verify no panic
    }

    #[test]
    fn unterminated_quote_artist_set_with_remainder() {
        let q = parse_query("artist 'unterminated");
        // The unterminated string is treated as the value "unterminated"
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "unterminated"
        ));
    }

    #[test]
    fn multiple_barewords_joined() {
        let q = parse_query("hello world");
        // "world" is unquoted after "hello" — "hello" is unknown field, "world" is its value
        // Both fall into any_text
        let text = contains_str(&q.any_text).expect("any_text should be set");
        assert!(text.contains("hello") || text.contains("world"));
    }

    #[test]
    fn source_field() {
        let q = parse_query("source bandcamp");
        assert_eq!(q.source.as_deref(), Some("bandcamp"));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn title_field() {
        let q = parse_query("title 'Kong'");
        assert!(matches!(
            &q.title,
            Some(StringQuery::Contains(s)) if s == "Kong"
        ));
    }

    #[test]
    fn year_field() {
        let q = parse_query("year '2024'");
        assert!(matches!(
            q.year,
            Some(NumericQuery::Exact(v)) if (v - 2024.0).abs() < 0.01
        ));
    }

    #[test]
    fn invalid_bpm_value_falls_back_to_any_text() {
        // "bpm" is a known field but "notanumber" can't be parsed as numeric.
        let q = parse_query("bpm 'notanumber'");
        assert!(q.bpm.is_none());
        // Both tokens should be in any_text
        let text = contains_str(&q.any_text).expect("any_text should be set for fallback");
        assert!(text.contains("bpm") || text.contains("notanumber"));
    }

    #[test]
    fn added_na() {
        let q = parse_query("added 'N/A'");
        assert!(matches!(q.added, Some(DateQuery::NA)));
    }

    #[test]
    fn all_known_fields_no_any_text() {
        // Each known field with a valid value should not pollute any_text
        let q = parse_query("artist 'bonobo' genre 'jazz' bpm '120'");
        assert!(q.artist.is_some());
        assert!(q.genre.is_some());
        assert!(q.bpm.is_some());
        assert!(q.any_text.is_none());
    }
}
