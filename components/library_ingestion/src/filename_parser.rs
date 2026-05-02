//! Parse music metadata from a filename stem when no embedded tags are present.
//!
//! Algorithm:
//! 1. Trim whitespace.
//! 2. Try to peel a leading track number: `<num> - <rest>` or `<num> <rest>`.
//! 3. On remainder, split on first ` - `: left = artist, right = title.
//!    With no ` - `: whole string is title, artist = None.
//! 4. Empty title → "Unknown".
//! 5. Cap artist and title at 256 chars.

/// Parsed components extracted from a filename stem.
pub(crate) struct ParsedFilename {
    pub track_number: Option<u32>,
    pub artist: Option<String>,
    /// Always populated; falls back to `"Unknown"` for empty/whitespace-only stems.
    pub title: String,
}

/// Parse a filename stem into structured metadata.
pub(crate) fn parse(stem: &str) -> ParsedFilename {
    let trimmed = stem.trim();

    let (track_number, remainder) = peel_track_number(trimmed);

    let (artist, title) = split_artist_title(remainder);

    let title = if title.is_empty() {
        "Unknown".to_string()
    } else {
        truncate(title, 256)
    };

    let artist = artist.map(|a| truncate(a, 256));

    ParsedFilename {
        track_number,
        artist,
        title,
    }
}

/// Try to consume a leading track number from the stem.
/// Returns `(Some(n), rest)` if found, else `(None, original)`.
fn peel_track_number(s: &str) -> (Option<u32>, &str) {
    // Pattern 1: "<num> - <rest>" — split on first " - "
    if let Some(dash_pos) = s.find(" - ") {
        let potential_num = &s[..dash_pos];
        if let Ok(n) = potential_num.trim().parse::<u32>() {
            if (1..=9999).contains(&n) && potential_num.trim().chars().all(|c| c.is_ascii_digit()) {
                return (Some(n), s[dash_pos + 3..].trim());
            }
        }
    }

    // Pattern 2: "<num> <rest>" — split on first whitespace, only if token is purely digits
    if let Some(space_pos) = s.find(' ') {
        let potential_num = &s[..space_pos];
        if !potential_num.is_empty() && potential_num.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = potential_num.parse::<u32>() {
                if (1..=9999).contains(&n) {
                    return (Some(n), s[space_pos + 1..].trim());
                }
            }
        }
    }

    (None, s)
}

/// Split remainder into (artist, title) on the first ` - `.
fn split_artist_title(s: &str) -> (Option<&str>, &str) {
    if let Some(dash_pos) = s.find(" - ") {
        let artist = s[..dash_pos].trim();
        let title = s[dash_pos + 3..].trim();
        let artist = if artist.is_empty() {
            None
        } else {
            Some(artist)
        };
        (artist, title)
    } else {
        (None, s.trim())
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("01 - Yagya - Empty Streets", Some(1), Some("Yagya"), "Empty Streets")]
    #[case(
        "01 - Yagya - Don't Call - Reprise",
        Some(1),
        Some("Yagya"),
        "Don't Call - Reprise"
    )]
    #[case("Yagya - Empty Streets", None, Some("Yagya"), "Empty Streets")]
    #[case("01 - Empty Streets", Some(1), None, "Empty Streets")]
    #[case("01 Empty Streets", Some(1), None, "Empty Streets")]
    #[case("Empty Streets", None, None, "Empty Streets")]
    #[case("   ", None, None, "Unknown")]
    #[case("Björk - Hyperballad", None, Some("Björk"), "Hyperballad")]
    fn parse_filename_stem(
        #[case] stem: &str,
        #[case] expected_track: Option<u32>,
        #[case] expected_artist: Option<&str>,
        #[case] expected_title: &str,
    ) {
        let parsed = parse(stem);
        assert_eq!(
            parsed.track_number, expected_track,
            "track_number for {:?}",
            stem
        );
        assert_eq!(
            parsed.artist.as_deref(),
            expected_artist,
            "artist for {:?}",
            stem
        );
        assert_eq!(parsed.title, expected_title, "title for {:?}", stem);
    }

    #[test]
    fn oversize_title_capped_at_256_chars() {
        let long = "A".repeat(300);
        let parsed = parse(&long);
        assert_eq!(parsed.title.chars().count(), 256);
        assert!(parsed.artist.is_none());
    }

    #[test]
    fn oversize_artist_capped_at_256_chars() {
        let long_artist = "A".repeat(300);
        let stem = format!("{} - Some Title", long_artist);
        let parsed = parse(&stem);
        // With a 300-char artist token, " - " separator is found
        // artist = first 256 chars, title = "Some Title"
        assert_eq!(parsed.artist.as_ref().map(|a| a.chars().count()), Some(256));
        assert_eq!(parsed.title, "Some Title");
    }

    #[test]
    fn track_number_zero_not_consumed() {
        // 0 is not in 1..=9999 range — treat as title, not track number
        let parsed = parse("00 - Some Track");
        // "00" parses as 0, which is outside 1..=9999, so no track number peeled
        // Then "00 - Some Track" is split as artist="00", title="Some Track"
        assert_eq!(parsed.track_number, None);
        assert_eq!(parsed.artist.as_deref(), Some("00"));
        assert_eq!(parsed.title, "Some Track");
    }

    #[test]
    fn track_number_9999_accepted() {
        let parsed = parse("9999 - Artist - Title");
        assert_eq!(parsed.track_number, Some(9999));
        assert_eq!(parsed.artist.as_deref(), Some("Artist"));
        assert_eq!(parsed.title, "Title");
    }

    #[test]
    fn track_number_10000_not_consumed() {
        let parsed = parse("10000 - Artist - Title");
        assert_eq!(parsed.track_number, None);
    }
}
