//! Conversion of match outcomes to the source protocol's wire types.

use crate::TrackOutcome;
use source_protocol::{ImportTrackOutcome, ImportTrackReport, SourcePlaylistTrack};

/// Wire report line for one playlist track.
///
/// A track the user does not own on the source is reported as
/// `Missing { not_owned: true }`: a collection sync cannot fetch it.
fn to_import_report(track: &SourcePlaylistTrack, outcome: &TrackOutcome) -> ImportTrackReport {
    let outcome = match outcome {
        TrackOutcome::Matched(hash) => ImportTrackOutcome::Matched {
            hash: hash.as_str().to_string(),
        },
        TrackOutcome::Ambiguous(hashes) => ImportTrackOutcome::Ambiguous {
            candidates: hashes.iter().map(|h| h.as_str().to_string()).collect(),
        },
        TrackOutcome::DurationMismatch {
            hash,
            library_secs,
            source_secs,
        } => ImportTrackOutcome::DurationMismatch {
            hash: hash.as_str().to_string(),
            library_secs: *library_secs,
            source_secs: *source_secs,
        },
        TrackOutcome::Missing => ImportTrackOutcome::Missing {
            not_owned: !track.owned,
        },
    };
    ImportTrackReport {
        position: track.position,
        artist: track.artist.clone(),
        title: track.title.clone(),
        outcome,
    }
}

/// Wire report for every playlist track, in source order.
///
/// `tracks` and `outcomes` are the inputs and result of
/// [`match_source_playlist`](crate::match_source_playlist), so they pair up.
pub fn to_import_reports(
    tracks: &[SourcePlaylistTrack],
    outcomes: &[TrackOutcome],
) -> Vec<ImportTrackReport> {
    tracks
        .iter()
        .zip(outcomes)
        .map(|(t, o)| to_import_report(t, o))
        .collect()
}

/// Whether a sync could fetch something: some track is missing from the
/// library but owned on the source.
pub fn has_owned_missing(tracks: &[SourcePlaylistTrack], outcomes: &[TrackOutcome]) -> bool {
    tracks
        .iter()
        .zip(outcomes)
        .any(|(t, o)| t.owned && matches!(o, TrackOutcome::Missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{hash, pl_track};
    use pretty_assertions::assert_eq;

    #[test]
    fn matched_carries_hash_and_track_fields() {
        let track = pl_track(3, "A", "One", 100.0);
        let report = to_import_report(&track, &TrackOutcome::Matched(hash("1111")));
        assert_eq!(
            report,
            ImportTrackReport {
                position: 3,
                artist: "A".to_string(),
                title: "One".to_string(),
                outcome: ImportTrackOutcome::Matched {
                    hash: "sha256:1111".to_string()
                },
            }
        );
    }

    #[test]
    fn ambiguous_lists_all_candidates() {
        let track = pl_track(1, "B", "Two", 200.0);
        let report = to_import_report(
            &track,
            &TrackOutcome::Ambiguous(vec![hash("2222"), hash("3333")]),
        );
        assert_eq!(
            report.outcome,
            ImportTrackOutcome::Ambiguous {
                candidates: vec!["sha256:2222".to_string(), "sha256:3333".to_string()]
            }
        );
    }

    #[test]
    fn duration_mismatch_keeps_whole_seconds() {
        let track = pl_track(1, "A", "One", 545.0);
        let report = to_import_report(
            &track,
            &TrackOutcome::DurationMismatch {
                hash: hash("5555"),
                library_secs: 210,
                source_secs: 545,
            },
        );
        assert_eq!(
            report.outcome,
            ImportTrackOutcome::DurationMismatch {
                hash: "sha256:5555".to_string(),
                library_secs: 210,
                source_secs: 545,
            }
        );
    }

    #[test]
    fn missing_owned_track_is_not_flagged_not_owned() {
        let track = pl_track(1, "C", "Three", 150.0);
        let report = to_import_report(&track, &TrackOutcome::Missing);
        assert_eq!(
            report.outcome,
            ImportTrackOutcome::Missing { not_owned: false }
        );
    }

    #[test]
    fn missing_unowned_track_is_flagged_not_owned() {
        let mut track = pl_track(1, "C", "Three", 150.0);
        track.owned = false;
        let report = to_import_report(&track, &TrackOutcome::Missing);
        assert_eq!(
            report.outcome,
            ImportTrackOutcome::Missing { not_owned: true }
        );
    }

    #[test]
    fn reports_keep_source_order() {
        let tracks = [pl_track(1, "A", "One", 1.0), pl_track(2, "B", "Two", 1.0)];
        let outcomes = [TrackOutcome::Missing, TrackOutcome::Matched(hash("22"))];
        let reports = to_import_reports(&tracks, &outcomes);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].position, 1);
        assert_eq!(reports[1].position, 2);
    }

    #[test]
    fn has_owned_missing_ignores_unowned_and_present_tracks() {
        let mut unowned = pl_track(2, "B", "Two", 1.0);
        unowned.owned = false;
        let tracks = [pl_track(1, "A", "One", 1.0), unowned];
        let outcomes = [TrackOutcome::Matched(hash("11")), TrackOutcome::Missing];
        assert!(!has_owned_missing(&tracks, &outcomes));

        let tracks = [pl_track(1, "A", "One", 1.0)];
        assert!(has_owned_missing(&tracks, &[TrackOutcome::Missing]));
    }
}
