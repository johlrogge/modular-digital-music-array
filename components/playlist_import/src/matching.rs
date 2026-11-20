//! Classify source playlist tracks against the library.

use library_ipc_client::ContentHash;
use track_matcher::{CandidateTrack, MatchResult, TrackLookup};

/// The library tracks an ambiguous item matched: at least two, in the order
/// the matcher returned them.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidates(Vec<ContentHash>);

impl Candidates {
    /// `None` unless there are at least two hashes: fewer is not ambiguous.
    pub fn new(hashes: Vec<ContentHash>) -> Option<Self> {
        (hashes.len() >= 2).then_some(Self(hashes))
    }

    pub fn as_slice(&self) -> &[ContentHash] {
        &self.0
    }
}

/// Result of matching one source playlist track against the library.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackOutcome {
    Matched(ContentHash),
    /// Several library tracks matched; every candidate is written.
    Ambiguous(Candidates),
    /// Artist and title match but every library duration is outside the
    /// matcher's tolerance: probably a different version (e.g. a radio edit).
    /// Never written to the playlist.
    DurationMismatch {
        hash: ContentHash,
        library_secs: u32,
        source_secs: u32,
    },
    Missing,
}

/// Match every playlist track, preserving source order.
pub fn match_source_playlist(
    tracks: &[source_protocol::SourcePlaylistTrack],
    lookup: &impl TrackLookup,
) -> Vec<TrackOutcome> {
    tracks
        .iter()
        .map(|t| {
            let candidate = CandidateTrack {
                isrc: None,
                artist: Some(t.artist.clone()).filter(|s| !s.is_empty()),
                title: Some(t.title.clone()).filter(|s| !s.is_empty()),
                duration_secs: if t.duration_secs > 0.0 {
                    Some(t.duration_secs.round() as u32)
                } else {
                    None
                },
            };
            match track_matcher::match_track(&candidate, lookup) {
                MatchResult::Definitive(hash, _) => TrackOutcome::Matched(hash),
                // The matcher only says Weak when the source has a duration.
                MatchResult::Weak { hash, library_secs } => match candidate.duration_secs {
                    Some(source_secs) => TrackOutcome::DurationMismatch {
                        hash,
                        library_secs,
                        source_secs,
                    },
                    None => TrackOutcome::Matched(hash),
                },
                MatchResult::Ambiguous(hashes, _) => from_candidates(hashes),
                MatchResult::NoMatch(_) => TrackOutcome::Missing,
            }
        })
        .collect()
}

/// The outcome for the library tracks the matcher called ambiguous. The
/// matcher gives at least two; fewer are not ambiguous.
fn from_candidates(mut hashes: Vec<ContentHash>) -> TrackOutcome {
    if hashes.len() == 1 {
        return TrackOutcome::Matched(hashes.remove(0));
    }
    match Candidates::new(hashes) {
        Some(candidates) => TrackOutcome::Ambiguous(candidates),
        None => TrackOutcome::Missing,
    }
}

/// Content hashes to write, in playlist order: a matched item
/// gives its hash, an ambiguous item gives every candidate, in the order the
/// matcher returned them, side by side where the item sits (the user removes
/// the unwanted ones). Duration mismatches and missing tracks give none.
pub fn matched_hashes(outcomes: &[TrackOutcome]) -> Vec<ContentHash> {
    outcomes
        .iter()
        .flat_map(|o| match o {
            TrackOutcome::Matched(h) => vec![h.clone()],
            TrackOutcome::Ambiguous(candidates) => candidates.as_slice().to_vec(),
            TrackOutcome::DurationMismatch { .. } | TrackOutcome::Missing => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{hash, pl_track, sample_lookup, sample_tracks, FakePlaylistLookup};
    use pretty_assertions::assert_eq;

    #[test]
    fn match_source_playlist_classifies_in_order() {
        let outcomes = match_source_playlist(&sample_tracks(), &sample_lookup());
        assert_eq!(outcomes.len(), 4);
        assert_eq!(outcomes[0], TrackOutcome::Matched(hash("1111111111111111")));
        assert!(matches!(&outcomes[1], TrackOutcome::Ambiguous(c) if c.as_slice().len() == 2));
        assert_eq!(outcomes[2], TrackOutcome::Missing);
        assert_eq!(outcomes[3], TrackOutcome::Matched(hash("4444444444444444")));
    }

    #[test]
    fn duration_mismatch_is_flagged_not_matched() {
        // Library has a 210s edit; the Bandcamp track is the 545s original.
        let lookup = FakePlaylistLookup {
            entries: vec![("a", "one", vec![(hash("5555555555555555"), Some(210))])],
        };
        let tracks = [pl_track(1, "A", "One", 545.088)];
        let outcomes = match_source_playlist(&tracks, &lookup);
        assert_eq!(
            outcomes,
            [TrackOutcome::DurationMismatch {
                hash: hash("5555555555555555"),
                library_secs: 210,
                source_secs: 545,
            }]
        );
        assert!(matched_hashes(&outcomes).is_empty());
    }

    #[test]
    fn a_library_track_without_a_duration_is_matched_and_written() {
        let lookup = FakePlaylistLookup {
            entries: vec![("a", "one", vec![(hash("5555555555555555"), None)])],
        };
        let tracks = [pl_track(1, "A", "One", 545.088)];
        let outcomes = match_source_playlist(&tracks, &lookup);
        assert_eq!(outcomes, [TrackOutcome::Matched(hash("5555555555555555"))]);
        assert_eq!(matched_hashes(&outcomes), [hash("5555555555555555")]);
    }

    #[test]
    fn candidates_need_at_least_two_hashes() {
        assert!(Candidates::new(vec![]).is_none());
        assert!(Candidates::new(vec![hash("11")]).is_none());
        let two = Candidates::new(vec![hash("11"), hash("22")]).unwrap();
        assert_eq!(two.as_slice(), [hash("11"), hash("22")]);
    }

    #[test]
    fn unknown_source_duration_still_matches_on_artist_title() {
        let lookup = FakePlaylistLookup {
            entries: vec![("a", "one", vec![(hash("5555555555555555"), Some(210))])],
        };
        let tracks = [pl_track(1, "A", "One", 0.0)];
        assert_eq!(
            match_source_playlist(&tracks, &lookup),
            [TrackOutcome::Matched(hash("5555555555555555"))]
        );
    }

    #[test]
    fn matched_hashes_adds_every_ambiguous_candidate_in_place() {
        let outcomes = match_source_playlist(&sample_tracks(), &sample_lookup());
        assert_eq!(
            matched_hashes(&outcomes),
            [
                hash("1111111111111111"),
                hash("2222222222222222"),
                hash("3333333333333333"),
                hash("4444444444444444")
            ]
        );
    }
}
