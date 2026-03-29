use music_primitives::ContentHash;

/// Metadata from an external source (Rekordbox, Serato, etc.)
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateTrack {
    pub isrc: Option<String>,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub duration_secs: Option<u32>,
}

/// How confident we are in the match
#[derive(Debug, Clone, PartialEq)]
pub enum MatchConfidence {
    Isrc,
    ArtistTitleDuration,
    ArtistTitleOnly,
}

/// The match outcome, carrying the confidence level when a match was found
#[derive(Debug, Clone, PartialEq)]
pub enum MatchResult {
    Definitive(ContentHash, MatchConfidence),
    Ambiguous(Vec<ContentHash>, MatchConfidence),
    NoMatch,
}

/// Caller-provided lookup — keeps this component free of IPC/network deps
pub trait TrackLookup {
    fn find_by_isrc(&self, isrc: &str) -> Vec<ContentHash>;
    /// Returns (content_hash, duration_secs) pairs for duration filtering.
    ///
    /// Both `artist` and `title` are pre-normalized via [`normalize`] before
    /// this method is called. Implementors should store and compare normalized
    /// values to guarantee consistent matching behaviour.
    fn find_by_artist_title(&self, artist: &str, title: &str) -> Vec<(ContentHash, Option<u32>)>;
}

pub const DURATION_TOLERANCE_SECS: u32 = 2;

/// Normalize a string for lookup comparison: lowercase, trim leading/trailing
/// whitespace, and collapse internal whitespace runs to a single space.
///
/// Implementors of [`TrackLookup::find_by_artist_title`] should use this
/// function when storing and comparing artist/title keys so that the same
/// normalization is applied on both sides.
pub fn normalize(s: &str) -> String {
    let lowered = s.to_lowercase();
    let trimmed = lowered.trim();
    // Collapse internal whitespace runs to single space
    trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn duration_matches(a: u32, b: u32, tolerance: u32) -> bool {
    a.abs_diff(b) <= tolerance
}

pub fn match_track(candidate: &CandidateTrack, lookup: &impl TrackLookup) -> MatchResult {
    // Phase 1: ISRC lookup
    if let Some(ref isrc) = candidate.isrc {
        let results = lookup.find_by_isrc(isrc);
        match results.len() {
            0 => { /* fall through */ }
            1 => {
                return MatchResult::Definitive(
                    results.into_iter().next().unwrap(),
                    MatchConfidence::Isrc,
                );
            }
            _ => {
                return MatchResult::Ambiguous(results, MatchConfidence::Isrc);
            }
        }
    }

    // Phase 2: Artist + title lookup
    if let (Some(ref artist), Some(ref title)) = (&candidate.artist, &candidate.title) {
        let raw_results = lookup.find_by_artist_title(&normalize(artist), &normalize(title));

        if let Some(candidate_duration) = candidate.duration_secs {
            // Filter by duration — include entries where track duration is unknown
            let duration_filtered: Vec<ContentHash> = raw_results
                .iter()
                .filter(|(_, track_dur)| match track_dur {
                    Some(d) => duration_matches(candidate_duration, *d, DURATION_TOLERANCE_SECS),
                    None => true, // can't filter what we don't have
                })
                .map(|(hash, _)| hash.clone())
                .collect();

            match duration_filtered.len() {
                0 => {
                    // Duration filter produced nothing — fall through to artist+title only
                }
                1 => {
                    return MatchResult::Definitive(
                        duration_filtered.into_iter().next().unwrap(),
                        MatchConfidence::ArtistTitleDuration,
                    );
                }
                _ => {
                    return MatchResult::Ambiguous(
                        duration_filtered,
                        MatchConfidence::ArtistTitleDuration,
                    );
                }
            }
        }

        // Artist+title only (no duration supplied or duration filter produced nothing)
        let unfiltered: Vec<ContentHash> = raw_results.into_iter().map(|(h, _)| h).collect();
        match unfiltered.len() {
            0 => { /* fall through */ }
            1 => {
                return MatchResult::Definitive(
                    unfiltered.into_iter().next().unwrap(),
                    MatchConfidence::ArtistTitleOnly,
                );
            }
            _ => {
                return MatchResult::Ambiguous(unfiltered, MatchConfidence::ArtistTitleOnly);
            }
        }
    }

    // Phase 3: No match
    MatchResult::NoMatch
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ---------------------------------------------------------------------------
    // MockLookup
    // ---------------------------------------------------------------------------

    struct MockLookup {
        isrc_entries: Vec<(String, ContentHash)>,
        artist_title_entries: Vec<(String, String, ContentHash, Option<u32>)>,
    }

    impl MockLookup {
        fn new() -> Self {
            Self {
                isrc_entries: Vec::new(),
                artist_title_entries: Vec::new(),
            }
        }

        fn with_isrc(mut self, isrc: &str, hash: &str) -> Self {
            self.isrc_entries
                .push((isrc.to_string(), ContentHash::new(hash)));
            self
        }

        fn with_artist_title(mut self, artist: &str, title: &str, hash: &str) -> Self {
            self.artist_title_entries.push((
                artist.to_string(),
                title.to_string(),
                ContentHash::new(hash),
                None,
            ));
            self
        }

        fn with_artist_title_duration(
            mut self,
            artist: &str,
            title: &str,
            hash: &str,
            duration: u32,
        ) -> Self {
            self.artist_title_entries.push((
                artist.to_string(),
                title.to_string(),
                ContentHash::new(hash),
                Some(duration),
            ));
            self
        }
    }

    impl TrackLookup for MockLookup {
        fn find_by_isrc(&self, isrc: &str) -> Vec<ContentHash> {
            self.isrc_entries
                .iter()
                .filter(|(i, _)| i == isrc)
                .map(|(_, h)| h.clone())
                .collect()
        }

        fn find_by_artist_title(
            &self,
            artist: &str,
            title: &str,
        ) -> Vec<(ContentHash, Option<u32>)> {
            self.artist_title_entries
                .iter()
                .filter(|(a, t, _, _)| a == artist && t == title)
                .map(|(_, _, h, d)| (h.clone(), *d))
                .collect()
        }
    }

    // ---------------------------------------------------------------------------
    // Normalization tests
    // ---------------------------------------------------------------------------

    #[test]
    fn normalize_lowercases() {
        assert_eq!(normalize("Hello World"), "hello world");
    }

    #[test]
    fn normalize_trims_whitespace() {
        assert_eq!(normalize("  hello  "), "hello");
    }

    #[test]
    fn normalize_collapses_internal_whitespace() {
        assert_eq!(normalize("hello   world"), "hello world");
    }

    #[test]
    fn normalize_handles_tabs_and_newlines() {
        assert_eq!(normalize("hello\t\nworld"), "hello world");
    }

    // ---------------------------------------------------------------------------
    // ISRC tests
    // ---------------------------------------------------------------------------

    #[test]
    fn isrc_exact_match_returns_definitive() {
        let lookup = MockLookup::new().with_isrc("USRC12345678", "hash1");
        let candidate = CandidateTrack {
            isrc: Some("USRC12345678".to_string()),
            artist: None,
            title: None,
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::Isrc)
        ));
        if let MatchResult::Definitive(h, _) = result {
            assert_eq!(h.as_str(), "hash1");
        }
    }

    #[test]
    fn isrc_no_match_falls_through_to_no_match() {
        let lookup = MockLookup::new().with_isrc("USRC99999999", "hash1");
        let candidate = CandidateTrack {
            isrc: Some("USRC12345678".to_string()),
            artist: None,
            title: None,
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(result, MatchResult::NoMatch));
    }

    #[test]
    fn isrc_ambiguous_returns_ambiguous() {
        let lookup = MockLookup::new()
            .with_isrc("USRC12345678", "hash1")
            .with_isrc("USRC12345678", "hash2");
        let candidate = CandidateTrack {
            isrc: Some("USRC12345678".to_string()),
            artist: None,
            title: None,
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Ambiguous(_, MatchConfidence::Isrc)
        ));
    }

    #[test]
    fn candidate_without_isrc_skips_isrc_phase() {
        let lookup = MockLookup::new()
            .with_isrc("USRC12345678", "isrc_hash")
            .with_artist_title("artist", "title", "meta_hash");
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        // Should match via artist/title, not ISRC
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleOnly)
        ));
        if let MatchResult::Definitive(h, _) = result {
            assert_eq!(h.as_str(), "meta_hash");
        }
    }

    // ---------------------------------------------------------------------------
    // Artist + title + duration tests
    // ---------------------------------------------------------------------------

    #[test]
    fn artist_title_duration_exact_match() {
        let lookup =
            MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(300),
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleDuration)
        ));
    }

    #[test]
    fn artist_title_duration_within_tolerance() {
        let lookup =
            MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(302), // 2 seconds off — within tolerance
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleDuration)
        ));
    }

    #[test]
    fn artist_title_duration_outside_tolerance_falls_to_title_only() {
        let lookup =
            MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(305), // 5 seconds off — outside tolerance
        };
        let result = match_track(&candidate, &lookup);
        // Duration filter produced nothing → falls to artist+title only
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleOnly)
        ));
    }

    #[test]
    fn artist_title_duration_multiple_within_tolerance_ambiguous() {
        let lookup = MockLookup::new()
            .with_artist_title_duration("artist", "title", "hash1", 300)
            .with_artist_title_duration("artist", "title", "hash2", 301);
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(300),
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Ambiguous(_, MatchConfidence::ArtistTitleDuration)
        ));
    }

    #[test]
    fn artist_title_duration_includes_entries_with_no_duration() {
        let lookup = MockLookup::new()
            .with_artist_title("artist", "title", "hash_no_dur"); // no duration
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(300),
        };
        let result = match_track(&candidate, &lookup);
        // Entry with no duration is included in duration-filtered set
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleDuration)
        ));
    }

    // ---------------------------------------------------------------------------
    // Artist + title only tests
    // ---------------------------------------------------------------------------

    #[test]
    fn artist_title_only_single_match_definitive() {
        let lookup = MockLookup::new().with_artist_title("artist", "title", "hash1");
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleOnly)
        ));
    }

    #[test]
    fn artist_title_only_multiple_matches_ambiguous() {
        let lookup = MockLookup::new()
            .with_artist_title("artist", "title", "hash1")
            .with_artist_title("artist", "title", "hash2");
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Ambiguous(_, MatchConfidence::ArtistTitleOnly)
        ));
    }

    // ---------------------------------------------------------------------------
    // NoMatch tests
    // ---------------------------------------------------------------------------

    #[test]
    fn no_match_when_no_artist_or_title_or_isrc() {
        let lookup = MockLookup::new();
        let candidate = CandidateTrack {
            isrc: None,
            artist: None,
            title: None,
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(result, MatchResult::NoMatch));
    }

    #[test]
    fn no_match_when_lookup_has_no_results() {
        let lookup = MockLookup::new();
        let candidate = CandidateTrack {
            isrc: Some("USRC12345678".to_string()),
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(result, MatchResult::NoMatch));
    }

    // ---------------------------------------------------------------------------
    // Fallthrough tests
    // ---------------------------------------------------------------------------

    #[test]
    fn isrc_miss_falls_through_to_metadata_match() {
        let lookup = MockLookup::new()
            .with_isrc("USRC99999999", "wrong_hash") // different ISRC
            .with_artist_title("artist", "title", "correct_hash");
        let candidate = CandidateTrack {
            isrc: Some("USRC12345678".to_string()), // won't match lookup
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleOnly)
        ));
        if let MatchResult::Definitive(h, _) = result {
            assert_eq!(h.as_str(), "correct_hash");
        }
    }

    #[test]
    fn duration_filter_miss_falls_through_to_title_only_match() {
        // Two entries: one has duration outside tolerance, one has no duration
        let lookup = MockLookup::new()
            .with_artist_title_duration("artist", "title", "far_away", 400) // outside tolerance
            .with_artist_title("artist", "title", "no_duration_hash"); // no duration
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(300),
        };
        let result = match_track(&candidate, &lookup);
        // Duration filter keeps no_duration_hash (unknown duration) and drops far_away.
        // So we get a single Definitive result from the duration-filtered set.
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleDuration)
        ));
        if let MatchResult::Definitive(h, _) = result {
            assert_eq!(h.as_str(), "no_duration_hash");
        }
    }

    #[test]
    fn all_duration_filtered_falls_through_to_title_only() {
        let lookup = MockLookup::new()
            .with_artist_title_duration("artist", "title", "hash1", 400); // outside tolerance
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("artist".to_string()),
            title: Some("title".to_string()),
            duration_secs: Some(300),
        };
        let result = match_track(&candidate, &lookup);
        // Duration filter returns nothing → falls to title-only using unfiltered results
        assert!(matches!(
            result,
            MatchResult::Definitive(_, MatchConfidence::ArtistTitleOnly)
        ));
        if let MatchResult::Definitive(h, _) = result {
            assert_eq!(h.as_str(), "hash1");
        }
    }

    // ---------------------------------------------------------------------------
    // Normalization integration: lookup keys are normalized before lookup
    // ---------------------------------------------------------------------------

    #[test]
    fn artist_title_normalized_before_lookup() {
        // Lookup stores normalized keys
        let lookup = MockLookup::new().with_artist_title("daft punk", "get lucky", "hash1");
        let candidate = CandidateTrack {
            isrc: None,
            artist: Some("  Daft   Punk  ".to_string()),
            title: Some("GET LUCKY".to_string()),
            duration_secs: None,
        };
        let result = match_track(&candidate, &lookup);
        assert!(matches!(result, MatchResult::Definitive(_, _)));
    }
}
