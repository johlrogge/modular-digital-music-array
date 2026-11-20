use music_primitives::ContentHash;

/// Metadata from an external source (Rekordbox, Serato, etc.)
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateTrack {
    pub isrc: Option<String>,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub duration_secs: Option<u32>,
}

/// How confident we are in a definitive match
#[derive(Debug, Clone, PartialEq)]
pub enum MatchConfidence {
    Isrc,
    ArtistTitleDuration,
    ArtistTitleOnly,
}

/// Why nothing matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoMatchReason {
    /// Nothing to look up: no artist or no title (and no ISRC hit).
    NoArtistOrTitle,
    /// The library has no track with this artist and title.
    NoTrackWithArtistTitle,
}

impl std::fmt::Display for NoMatchReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoArtistOrTitle => write!(f, "no artist or title to match on"),
            Self::NoTrackWithArtistTitle => write!(f, "no track with this artist+title"),
        }
    }
}

/// Why several tracks matched and none could be chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbiguityReason {
    /// Several tracks share the ISRC.
    Isrc,
    /// Several tracks with this artist+title are within [`DURATION_TOLERANCE_SECS`].
    WithinTolerance,
    /// Several tracks with this artist+title and the source gave no duration.
    NoDurationToCompare,
    /// Several tracks with this artist+title, none within tolerance; the closest
    /// is off by `closest_offset_secs`.
    DurationOff { closest_offset_secs: u32 },
}

impl AmbiguityReason {
    /// A sentence for `count` candidates.
    pub fn describe(&self, count: usize) -> String {
        match self {
            Self::Isrc => format!("{count} candidates with this ISRC"),
            Self::WithinTolerance => {
                format!("{count} candidates within {DURATION_TOLERANCE_SECS} s duration tolerance")
            }
            Self::NoDurationToCompare => {
                format!("{count} candidates with this artist+title, no duration to tell them apart")
            }
            Self::DurationOff {
                closest_offset_secs,
            } => format!(
                "{count} candidates with this artist+title, none within \
                 {DURATION_TOLERANCE_SECS} s duration (closest off by {closest_offset_secs} s)"
            ),
        }
    }
}

/// The match outcome
#[derive(Debug, Clone, PartialEq)]
pub enum MatchResult {
    Definitive(ContentHash, MatchConfidence),
    /// Exactly one track has this artist+title and both durations are known, but
    /// the library's is outside [`DURATION_TOLERANCE_SECS`]: probably another
    /// version (a radio edit, say).
    Weak {
        hash: ContentHash,
        library_secs: u32,
    },
    Ambiguous(Vec<ContentHash>, AmbiguityReason),
    NoMatch(NoMatchReason),
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
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
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
                return MatchResult::Ambiguous(results, AmbiguityReason::Isrc);
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
                    // Duration filter produced nothing — fall through
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
                        AmbiguityReason::WithinTolerance,
                    );
                }
            }
        }

        // Artist+title only (no duration supplied, or the duration filter removed
        // every candidate, so every library duration is known and off).
        let closest_offset = candidate.duration_secs.and_then(|wanted| {
            raw_results
                .iter()
                .filter_map(|(_, d)| d.map(|d| d.abs_diff(wanted)))
                .min()
        });
        match raw_results.len() {
            0 => return MatchResult::NoMatch(NoMatchReason::NoTrackWithArtistTitle),
            1 => {
                let (hash, library_secs) = raw_results.into_iter().next().unwrap();
                return match (candidate.duration_secs, library_secs) {
                    (Some(_), Some(library_secs)) => MatchResult::Weak { hash, library_secs },
                    _ => MatchResult::Definitive(hash, MatchConfidence::ArtistTitleOnly),
                };
            }
            _ => {
                let reason = match closest_offset {
                    Some(closest_offset_secs) => AmbiguityReason::DurationOff {
                        closest_offset_secs,
                    },
                    None => AmbiguityReason::NoDurationToCompare,
                };
                return MatchResult::Ambiguous(
                    raw_results.into_iter().map(|(h, _)| h).collect(),
                    reason,
                );
            }
        }
    }

    // Phase 3: Nothing to search for
    MatchResult::NoMatch(NoMatchReason::NoArtistOrTitle)
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

    fn cand(artist: Option<&str>, title: Option<&str>, secs: Option<u32>) -> CandidateTrack {
        CandidateTrack {
            isrc: None,
            artist: artist.map(str::to_string),
            title: title.map(str::to_string),
            duration_secs: secs,
        }
    }

    fn at(secs: Option<u32>) -> CandidateTrack {
        cand(Some("artist"), Some("title"), secs)
    }

    fn h(s: &str) -> ContentHash {
        ContentHash::new(s)
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
        let mut c = cand(None, None, None);
        c.isrc = Some("USRC12345678".to_string());
        assert_eq!(
            match_track(&c, &lookup),
            MatchResult::Definitive(h("hash1"), MatchConfidence::Isrc)
        );
    }

    #[test]
    fn isrc_no_match_falls_through_to_no_match() {
        let lookup = MockLookup::new().with_isrc("USRC99999999", "hash1");
        let mut c = cand(None, None, None);
        c.isrc = Some("USRC12345678".to_string());
        assert_eq!(
            match_track(&c, &lookup),
            MatchResult::NoMatch(NoMatchReason::NoArtistOrTitle)
        );
    }

    #[test]
    fn isrc_ambiguous_returns_ambiguous() {
        let lookup = MockLookup::new()
            .with_isrc("X", "hash1")
            .with_isrc("X", "hash2");
        let mut c = cand(None, None, None);
        c.isrc = Some("X".to_string());
        assert_eq!(
            match_track(&c, &lookup),
            MatchResult::Ambiguous(vec![h("hash1"), h("hash2")], AmbiguityReason::Isrc)
        );
    }

    #[test]
    fn candidate_without_isrc_skips_isrc_phase() {
        let lookup = MockLookup::new()
            .with_isrc("USRC12345678", "isrc_hash")
            .with_artist_title("artist", "title", "meta_hash");
        assert_eq!(
            match_track(&at(None), &lookup),
            MatchResult::Definitive(h("meta_hash"), MatchConfidence::ArtistTitleOnly)
        );
    }

    // ---------------------------------------------------------------------------
    // Artist + title + duration tests
    // ---------------------------------------------------------------------------

    #[test]
    fn artist_title_duration_exact_match() {
        let lookup = MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        assert_eq!(
            match_track(&at(Some(300)), &lookup),
            MatchResult::Definitive(h("hash1"), MatchConfidence::ArtistTitleDuration)
        );
    }

    #[test]
    fn artist_title_duration_within_tolerance() {
        let lookup = MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        assert_eq!(
            match_track(&at(Some(302)), &lookup),
            MatchResult::Definitive(h("hash1"), MatchConfidence::ArtistTitleDuration)
        );
    }

    #[test]
    fn single_candidate_outside_tolerance_is_weak() {
        let lookup = MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        assert_eq!(
            match_track(&at(Some(305)), &lookup),
            MatchResult::Weak {
                hash: h("hash1"),
                library_secs: 300
            }
        );
    }

    #[test]
    fn artist_title_duration_multiple_within_tolerance_ambiguous() {
        let lookup = MockLookup::new()
            .with_artist_title_duration("artist", "title", "hash1", 300)
            .with_artist_title_duration("artist", "title", "hash2", 301);
        assert_eq!(
            match_track(&at(Some(300)), &lookup),
            MatchResult::Ambiguous(
                vec![h("hash1"), h("hash2")],
                AmbiguityReason::WithinTolerance
            )
        );
    }

    #[test]
    fn library_track_with_no_duration_passes_the_duration_filter() {
        let lookup = MockLookup::new().with_artist_title("artist", "title", "hash_no_dur");
        assert_eq!(
            match_track(&at(Some(300)), &lookup),
            MatchResult::Definitive(h("hash_no_dur"), MatchConfidence::ArtistTitleDuration)
        );
    }

    #[test]
    fn duration_filter_keeps_unknown_and_drops_far_away() {
        let lookup = MockLookup::new()
            .with_artist_title_duration("artist", "title", "far_away", 400)
            .with_artist_title("artist", "title", "no_duration_hash");
        assert_eq!(
            match_track(&at(Some(300)), &lookup),
            MatchResult::Definitive(h("no_duration_hash"), MatchConfidence::ArtistTitleDuration)
        );
    }

    // ---------------------------------------------------------------------------
    // Artist + title only tests
    // ---------------------------------------------------------------------------

    #[test]
    fn artist_title_only_single_match_definitive() {
        let lookup = MockLookup::new().with_artist_title("artist", "title", "hash1");
        assert_eq!(
            match_track(&at(None), &lookup),
            MatchResult::Definitive(h("hash1"), MatchConfidence::ArtistTitleOnly)
        );
    }

    #[test]
    fn artist_title_only_with_a_library_duration_is_still_definitive() {
        // The source gave no duration, so there is nothing to disagree with.
        let lookup = MockLookup::new().with_artist_title_duration("artist", "title", "hash1", 300);
        assert_eq!(
            match_track(&at(None), &lookup),
            MatchResult::Definitive(h("hash1"), MatchConfidence::ArtistTitleOnly)
        );
    }

    #[test]
    fn no_duration_to_compare_is_ambiguous_with_every_candidate() {
        let lookup = MockLookup::new()
            .with_artist_title("artist", "title", "hash1")
            .with_artist_title("artist", "title", "hash2")
            .with_artist_title("artist", "title", "hash3");
        assert_eq!(
            match_track(&at(None), &lookup),
            MatchResult::Ambiguous(
                vec![h("hash1"), h("hash2"), h("hash3")],
                AmbiguityReason::NoDurationToCompare
            )
        );
    }

    #[test]
    fn all_candidates_off_in_duration_report_the_closest() {
        let lookup = MockLookup::new()
            .with_artist_title_duration("artist", "title", "h1", 310)
            .with_artist_title_duration("artist", "title", "h2", 296);
        assert_eq!(
            match_track(&at(Some(300)), &lookup),
            MatchResult::Ambiguous(
                vec![h("h1"), h("h2")],
                AmbiguityReason::DurationOff {
                    closest_offset_secs: 4
                }
            )
        );
    }

    // ---------------------------------------------------------------------------
    // NoMatch tests
    // ---------------------------------------------------------------------------

    #[test]
    fn no_artist_or_title_is_no_match() {
        let lookup = MockLookup::new();
        for c in [
            cand(None, Some("t"), Some(100)),
            cand(Some("a"), None, None),
            cand(None, None, None),
        ] {
            assert_eq!(
                match_track(&c, &lookup),
                MatchResult::NoMatch(NoMatchReason::NoArtistOrTitle)
            );
        }
    }

    #[test]
    fn no_track_with_artist_title_is_no_match() {
        let lookup = MockLookup::new().with_artist_title("other", "t", "h1");
        assert_eq!(
            match_track(&cand(Some("a"), Some("t"), Some(100)), &lookup),
            MatchResult::NoMatch(NoMatchReason::NoTrackWithArtistTitle)
        );
    }

    #[test]
    fn isrc_miss_with_empty_lookup_is_no_match() {
        let lookup = MockLookup::new();
        let mut c = at(None);
        c.isrc = Some("USRC12345678".to_string());
        assert_eq!(
            match_track(&c, &lookup),
            MatchResult::NoMatch(NoMatchReason::NoTrackWithArtistTitle)
        );
    }

    #[test]
    fn isrc_miss_falls_through_to_metadata_match() {
        let lookup = MockLookup::new()
            .with_isrc("USRC99999999", "wrong_hash")
            .with_artist_title("artist", "title", "correct_hash");
        let mut c = at(None);
        c.isrc = Some("USRC12345678".to_string());
        assert_eq!(
            match_track(&c, &lookup),
            MatchResult::Definitive(h("correct_hash"), MatchConfidence::ArtistTitleOnly)
        );
    }

    // ---------------------------------------------------------------------------
    // Reasons read as sentences
    // ---------------------------------------------------------------------------

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(
            NoMatchReason::NoArtistOrTitle.to_string(),
            "no artist or title to match on"
        );
        assert_eq!(
            NoMatchReason::NoTrackWithArtistTitle.to_string(),
            "no track with this artist+title"
        );
        assert_eq!(
            AmbiguityReason::WithinTolerance.describe(2),
            "2 candidates within 2 s duration tolerance"
        );
        assert_eq!(
            AmbiguityReason::NoDurationToCompare.describe(3),
            "3 candidates with this artist+title, no duration to tell them apart"
        );
        assert_eq!(
            AmbiguityReason::DurationOff {
                closest_offset_secs: 4
            }
            .describe(2),
            "2 candidates with this artist+title, none within 2 s duration (closest off by 4 s)"
        );
        assert_eq!(
            AmbiguityReason::Isrc.describe(2),
            "2 candidates with this ISRC"
        );
    }

    // ---------------------------------------------------------------------------
    // Normalization integration: lookup keys are normalized before lookup
    // ---------------------------------------------------------------------------

    #[test]
    fn artist_title_normalized_before_lookup() {
        let lookup = MockLookup::new().with_artist_title("daft punk", "get lucky", "hash1");
        let c = cand(Some("  Daft   Punk  "), Some("GET LUCKY"), None);
        assert!(matches!(
            match_track(&c, &lookup),
            MatchResult::Definitive(_, _)
        ));
    }
}
