use library_search::{
    parse_date_query, parse_duration_query, parse_key_query, parse_numeric_query,
    parse_string_query, StringQuery, TrackQuery,
};

/// Parse a simple query language into a `TrackQuery`.
///
/// Grammar:
/// - `:field 'quoted value'`  — single-quoted value (may contain spaces)
/// - `:field "quoted value"`  — double-quoted value (may contain spaces)
/// - `:field value`           — unquoted single word
/// - `bareword`               — any token NOT starting with `:` → added to `any_text`
///
/// Field names must be prefixed with `:`. A bare word like `artist` is treated
/// as plain text, not a field selector. This avoids false matches in titles
/// containing words like "artist", "bpm", or "added".
///
/// Recognised field names (always with `:` prefix):
///   `:artist`, `:title`, `:album`, `:label`, `:genre`, `:style`, `:bpm`,
///   `:key`, `:duration`, `:year`, `:source`, `:added`, `:started`, `:stopped`.
///
/// Unknown `:field` names (e.g. `:fizz`) fall back to bareword handling: both
/// the `:fizz` token and the following value token are added to `any_text`.
///
/// A lone `:` (no field name after the colon) is treated as a bareword.
///
/// Unparseable field values also fall back to bareword handling.
/// Never panics, never returns Err — always produces *some* query.
pub fn parse_query(input: &str) -> TrackQuery {
    let mut query = TrackQuery::default();
    let mut any_text_parts: Vec<String> = Vec::new();

    let mut remaining = input.trim();

    while !remaining.is_empty() {
        // Extract the next token (up to whitespace).
        let (token, after_token) = split_next_token(remaining);
        if token.is_empty() {
            break;
        }

        let after_token_trimmed = after_token.trim_start();

        // Only tokens starting with `:` can be field selectors.
        // A lone `:` (nothing after the colon) is a bareword.
        let maybe_field = if token.starts_with(':') && token.len() > 1 {
            Some(&token[1..])
        } else {
            None
        };

        match maybe_field {
            None => {
                // Plain bareword — goes directly into any_text.
                any_text_parts.push(token.to_string());
                remaining = after_token_trimmed;
            }
            Some(field_name) => {
                if !is_known_field(field_name) {
                    // Unknown field prefix — treat `:foo` and its value as barewords.
                    let (value_token, after_value) = split_next_value(after_token_trimmed);
                    any_text_parts.push(token.to_string());
                    if !value_token.is_empty() {
                        any_text_parts.push(value_token);
                    }
                    remaining = after_value.trim_start();
                } else if after_token_trimmed.is_empty() {
                    // Known field but no value follows — treat `:field` as a bareword.
                    any_text_parts.push(token.to_string());
                    remaining = after_token_trimmed;
                } else {
                    // Known field — extract the value.
                    let (value, after_value) = split_next_value(after_token_trimmed);

                    if value.is_empty() {
                        // Value was empty — treat the field token as a bareword.
                        any_text_parts.push(token.to_string());
                        remaining = after_token_trimmed;
                    } else {
                        // Apply the value to the appropriate query field.
                        let applied = apply_field(&mut query, field_name, &value);
                        if !applied {
                            // Parsing failed for a known field — fall back both tokens to any_text.
                            any_text_parts.push(token.to_string());
                            any_text_parts.push(value);
                        }
                        remaining = after_value.trim_start();
                    }
                }
            }
        }
    }

    if !any_text_parts.is_empty() {
        let combined = any_text_parts.join(" ");
        query.any_text = Some(StringQuery::Contains(combined));
    }

    query
}

/// Returns true if `name` is a recognised field keyword (without the `:` prefix).
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
/// `field` is the name without the `:` prefix.
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

    // --- New: bare field names are no longer treated as fields ---

    #[test]
    fn bare_artist_word_goes_to_any_text() {
        // Without `:` prefix, "artist" is just a bareword.
        let q = parse_query("artist bonobo");
        assert!(
            q.artist.is_none(),
            "bare 'artist' must not set the artist field"
        );
        let text = contains_str(&q.any_text).expect("any_text should contain both tokens");
        assert!(text.contains("artist") && text.contains("bonobo"));
    }

    #[test]
    fn bare_bpm_word_goes_to_any_text() {
        let q = parse_query("bpm 120");
        assert!(q.bpm.is_none(), "bare 'bpm' must not set the bpm field");
        let text = contains_str(&q.any_text).expect("any_text should be set");
        assert!(text.contains("bpm"));
    }

    #[test]
    fn bare_added_word_goes_to_any_text() {
        let q = parse_query("added -7");
        assert!(
            q.added.is_none(),
            "bare 'added' must not set the added field"
        );
    }

    // --- Colon-prefixed fields work correctly ---

    #[test]
    fn artist_single_quoted() {
        let q = parse_query(":artist 'bonobo'");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "bonobo"
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_double_quoted() {
        let q = parse_query(r#":artist "bonobo""#);
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "bonobo"
        ));
    }

    #[test]
    fn artist_unquoted_single_word() {
        let q = parse_query(":artist bonobo");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "bonobo"
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_with_spaces_in_quotes() {
        let q = parse_query(":artist 'carbon based lifeforms'");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "carbon based lifeforms"
        ));
    }

    #[test]
    fn added_date_expression() {
        // "-7" is a date expression meaning "7 days ago"
        let q = parse_query(":added '-7'");
        assert!(q.added.is_some(), "added should be set");
        assert!(q.any_text.is_none());
        // Verify it parsed as a date range (date_expression resolves -7 to a specific day)
        assert!(matches!(q.added.unwrap(), DateQuery::Range(..)));
    }

    #[test]
    fn added_unquoted_negative_offset() {
        let q = parse_query(":added -7");
        assert!(q.added.is_some(), "added should be set");
        assert!(q.any_text.is_none());
        assert!(matches!(q.added.unwrap(), DateQuery::Range(..)));
    }

    #[test]
    fn bpm_exact() {
        let q = parse_query(":bpm '120'");
        assert!(matches!(
            q.bpm,
            Some(NumericQuery::Exact(v)) if (v - 120.0).abs() < 0.01
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn bpm_range() {
        let q = parse_query(":bpm '120..130'");
        assert!(matches!(
            q.bpm,
            Some(NumericQuery::Range(lo, hi)) if lo == 120.0 && hi == 130.0
        ));
    }

    #[test]
    fn genre_field() {
        let q = parse_query(":genre 'ambient'");
        assert!(matches!(
            &q.genre,
            Some(StringQuery::Contains(s)) if s == "ambient"
        ));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_and_bpm_combined() {
        let q = parse_query(":artist 'bonobo' :bpm '120'");
        assert!(q.artist.is_some());
        assert!(q.bpm.is_some());
        assert!(q.any_text.is_none());
    }

    #[test]
    fn artist_and_bareword() {
        let q = parse_query(":artist 'bonobo' dest");
        assert!(q.artist.is_some());
        assert_eq!(contains_str(&q.any_text), Some("dest"));
    }

    #[test]
    fn unknown_field_falls_back_to_any_text() {
        // ":unknown" is not a known field — both tokens go to any_text
        let q = parse_query(":unknown 'foo'");
        let text = contains_str(&q.any_text).expect("any_text should be set");
        assert!(text.contains(":unknown") || text.contains("unknown"));
        assert!(text.contains("foo"));
    }

    #[test]
    fn unknown_field_fizz_falls_back() {
        // ":fizz bar" — both tokens should land in any_text, nothing silently set
        let q = parse_query(":fizz 'bar'");
        assert!(q.artist.is_none());
        assert!(q.title.is_none());
        assert!(q.bpm.is_none());
        let text = contains_str(&q.any_text).expect("any_text should be set");
        assert!(
            text.contains("bar"),
            "value should appear in any_text, got: {text}"
        );
    }

    #[test]
    fn unterminated_quote_no_panic() {
        // Should not panic; falls back gracefully
        let q = parse_query(":artist 'unterminated");
        // Either way the function must not panic and must return something.
        let _ = q; // just verify no panic
    }

    #[test]
    fn unterminated_quote_artist_set_with_remainder() {
        let q = parse_query(":artist 'unterminated");
        // The unterminated string is treated as the value "unterminated"
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "unterminated"
        ));
    }

    #[test]
    fn multiple_barewords_joined() {
        let q = parse_query("hello world");
        // Both "hello" and "world" are bare tokens (no `:` prefix) → any_text
        let text = contains_str(&q.any_text).expect("any_text should be set");
        assert!(text.contains("hello") && text.contains("world"));
    }

    #[test]
    fn source_field() {
        let q = parse_query(":source bandcamp");
        assert_eq!(q.source.as_deref(), Some("bandcamp"));
        assert!(q.any_text.is_none());
    }

    #[test]
    fn title_field() {
        let q = parse_query(":title 'Kong'");
        assert!(matches!(
            &q.title,
            Some(StringQuery::Contains(s)) if s == "Kong"
        ));
    }

    #[test]
    fn year_field() {
        let q = parse_query(":year '2024'");
        assert!(matches!(
            q.year,
            Some(NumericQuery::Exact(v)) if (v - 2024.0).abs() < 0.01
        ));
    }

    #[test]
    fn invalid_bpm_value_falls_back_to_any_text() {
        // ":bpm" is a known field but "notanumber" can't be parsed as numeric.
        let q = parse_query(":bpm 'notanumber'");
        assert!(q.bpm.is_none());
        // Both tokens should be in any_text
        let text = contains_str(&q.any_text).expect("any_text should be set for fallback");
        assert!(text.contains("bpm") || text.contains("notanumber"));
    }

    #[test]
    fn added_na() {
        let q = parse_query(":added 'N/A'");
        assert!(matches!(q.added, Some(DateQuery::NA)));
    }

    #[test]
    fn all_known_fields_no_any_text() {
        // Each known field with a valid value should not pollute any_text
        let q = parse_query(":artist 'bonobo' :genre 'jazz' :bpm '120'");
        assert!(q.artist.is_some());
        assert!(q.genre.is_some());
        assert!(q.bpm.is_some());
        assert!(q.any_text.is_none());
    }

    // --- New grammar-specific tests ---

    #[test]
    fn bareword_before_colon_field() {
        // Bare token before a colon field → bareword in any_text, field still parsed
        let q = parse_query("artist :title 'foo'");
        assert!(q.title.is_some(), "title should be set by :title");
        let text = contains_str(&q.any_text).expect("'artist' should be in any_text");
        assert_eq!(text, "artist");
    }

    #[test]
    fn artist_with_quoted_value_and_trailing_bareword() {
        // `:artist 'the prodigy' live` → artist set, any_text = "live"
        let q = parse_query(":artist 'the prodigy' live");
        assert!(matches!(
            &q.artist,
            Some(StringQuery::Contains(s)) if s == "the prodigy"
        ));
        assert_eq!(contains_str(&q.any_text), Some("live"));
    }

    #[test]
    fn lone_colon_is_bareword() {
        // A bare `:` with nothing after it → treated as a bareword
        let q = parse_query(":");
        // Must not panic; should appear in any_text
        let text = contains_str(&q.any_text).expect("':' should go into any_text");
        assert_eq!(text, ":");
    }

    #[test]
    fn title_containing_field_word_is_any_text() {
        // Tracks with "bpm", "artist", "added" in their name search correctly
        let q = parse_query("120bpm artist track added yesterday");
        assert!(q.bpm.is_none());
        assert!(q.artist.is_none());
        assert!(q.added.is_none());
        let text = contains_str(&q.any_text).expect("all tokens should be in any_text");
        assert!(text.contains("artist") && text.contains("bpm") && text.contains("added"));
    }

    #[test]
    fn unknown_field_does_not_silently_set_any_struct_field() {
        let q = parse_query(":fizz 'bar'");
        // Exhaustively verify nothing was set on the query except any_text
        assert!(q.artist.is_none());
        assert!(q.title.is_none());
        assert!(q.album.is_none());
        assert!(q.label.is_none());
        assert!(q.genre.is_none());
        assert!(q.style.is_none());
        assert!(q.bpm.is_none());
        assert!(q.key.is_none());
        assert!(q.duration.is_none());
        assert!(q.year.is_none());
        assert!(q.source.is_none());
        assert!(q.added.is_none());
        assert!(q.started.is_none());
        assert!(q.stopped.is_none());
        // any_text should have something
        assert!(q.any_text.is_some());
    }
}
