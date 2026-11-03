//! Classify source playlist tracks against the library.

use library_ipc_client::ContentHash;
use track_matcher::{CandidateTrack, MatchConfidence, MatchResult, TrackLookup};

/// Result of matching one source playlist track against the library.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackOutcome {
    Matched(ContentHash),
    Ambiguous(Vec<ContentHash>),
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
                MatchResult::Definitive(hash, MatchConfidence::ArtistTitleOnly) => {
                    match candidate.duration_secs {
                        Some(source_secs) => {
                            definitive_with_duration(&candidate, hash, source_secs, lookup)
                        }
                        None => TrackOutcome::Matched(hash),
                    }
                }
                MatchResult::Definitive(hash, _) => TrackOutcome::Matched(hash),
                MatchResult::Ambiguous(hashes, _) => TrackOutcome::Ambiguous(hashes),
                MatchResult::NoMatch => TrackOutcome::Missing,
            }
        })
        .collect()
}

/// The matcher fell back to artist+title although the source has a duration,
/// so the duration filter removed every candidate: the library version
/// differs. Look up the library duration to report it.
fn definitive_with_duration(
    candidate: &CandidateTrack,
    hash: ContentHash,
    source_secs: u32,
    lookup: &impl TrackLookup,
) -> TrackOutcome {
    let (Some(artist), Some(title)) = (&candidate.artist, &candidate.title) else {
        return TrackOutcome::Matched(hash);
    };
    let library_secs = lookup
        .find_by_artist_title(
            &track_matcher::normalize(artist),
            &track_matcher::normalize(title),
        )
        .into_iter()
        .find(|(h, _)| *h == hash)
        .and_then(|(_, d)| d);
    match library_secs {
        Some(library_secs) => TrackOutcome::DurationMismatch {
            hash,
            library_secs,
            source_secs,
        },
        // Cannot tell how it differs: do not write it silently.
        None => TrackOutcome::Ambiguous(vec![hash]),
    }
}

/// Content hashes of the definitive matches, in playlist order.
pub fn matched_hashes(outcomes: &[TrackOutcome]) -> Vec<ContentHash> {
    outcomes
        .iter()
        .filter_map(|o| match o {
            TrackOutcome::Matched(h) => Some(h.clone()),
            _ => None,
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
        assert!(matches!(&outcomes[1], TrackOutcome::Ambiguous(h) if h.len() == 2));
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
    fn matched_hashes_keeps_only_definitive_in_order() {
        let outcomes = match_source_playlist(&sample_tracks(), &sample_lookup());
        assert_eq!(
            matched_hashes(&outcomes),
            [hash("1111111111111111"), hash("4444444444444444")]
        );
    }
}
