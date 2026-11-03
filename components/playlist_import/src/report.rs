//! Plain-text import report (data to `String`; printing is the caller's job).

use source_protocol::{
    ImportJobStatus, ImportPhase, ImportResult, ImportSyncProgress, ImportTrackOutcome,
    ImportTrackReport, SyncWaitEnd,
};

/// `m:ss` for a number of seconds.
fn format_mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Warning line when fewer tracks were fetched than the playlist reports.
fn truncation_warning(fetched: usize, total: u32) -> Option<String> {
    (fetched != total as usize).then(|| {
        format!(
            "Warning: fetched {} of {} tracks; playlist may be incomplete",
            fetched, total
        )
    })
}

/// Text for the outcome of writing a playlist, with a warning after a replace.
/// Shared by the server-side import report and the rekordbox import.
pub fn format_write_outcome(name: &str, tracks: usize, created: bool) -> String {
    if created {
        format!("Created playlist '{}' with {} tracks", name, tracks)
    } else {
        format!(
            "Replaced playlist '{}' with {} tracks\n\
             Note: re-running this import replaces playlist '{}' again, \
             including any manual edits.",
            name, tracks, name
        )
    }
}

fn short_candidate(hash: &str) -> String {
    hash.chars().take(16).collect()
}

/// Render one numbered row per track plus the summary line.
fn render_rows(tracks: &[ImportTrackReport]) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let (mut matched, mut ambiguous, mut missing) = (0, 0, 0);
    for t in tracks {
        let label = format!("{} - {}", t.artist, t.title);
        match &t.outcome {
            ImportTrackOutcome::Matched { .. } => {
                matched += 1;
                let _ = writeln!(out, "{:3}. ✓ matched        {}", t.position, label);
            }
            ImportTrackOutcome::Ambiguous { candidates } => {
                ambiguous += 1;
                let _ = writeln!(out, "{:3}. ? ambiguous      {}", t.position, label);
                for h in candidates.iter().take(3) {
                    let _ = writeln!(out, "         candidate: {}", short_candidate(h));
                }
            }
            ImportTrackOutcome::DurationMismatch {
                hash,
                library_secs,
                source_secs,
            } => {
                ambiguous += 1;
                let _ = writeln!(
                    out,
                    "{:3}. ? duration differs (library {} vs bandcamp {}) {}",
                    t.position,
                    format_mmss(*library_secs),
                    format_mmss(*source_secs),
                    label
                );
                let _ = writeln!(out, "         candidate: {}", short_candidate(hash));
            }
            ImportTrackOutcome::Missing { not_owned } => {
                missing += 1;
                let note = if *not_owned { " (not owned)" } else { "" };
                let _ = writeln!(out, "{:3}. ✗ not in library {}{}", t.position, label, note);
            }
        }
    }
    let _ = write!(
        out,
        "{} matched, {} ambiguous, {} missing of {}",
        matched,
        ambiguous,
        missing,
        tracks.len()
    );
    out
}

/// Hint when the wait for downloads ended with some not fetched.
fn sync_wait_hint(sync: &ImportSyncProgress) -> Option<String> {
    let pending = sync.queued.saturating_sub(sync.completed + sync.failed);
    match sync.ended? {
        SyncWaitEnd::AllFinished => None,
        SyncWaitEnd::Paused => Some(format!(
            "Downloads paused: {} not fetched yet; resume downloads and re-run the import",
            pending
        )),
        SyncWaitEnd::TimedOut => Some(format!(
            "Timed out waiting for {} downloads; re-run the import later",
            pending
        )),
    }
}

/// Render a server-side import job: header, truncation warning, per-track
/// report, summary and the write outcome (or the failure message).
pub fn format_import_status(status: &ImportJobStatus) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    if let Some(title) = &status.playlist_title {
        match status.total_count {
            Some(total) => {
                let _ = writeln!(
                    out,
                    "Playlist '{}' ({} of {} tracks)",
                    title,
                    status.tracks.len(),
                    total
                );
            }
            None => {
                let _ = writeln!(out, "Playlist '{}'", title);
            }
        }
    }
    if let Some(warning) = status
        .total_count
        .and_then(|total| truncation_warning(status.tracks.len(), total))
    {
        let _ = writeln!(out, "{}", warning);
    }
    out.push_str(&render_rows(&status.tracks));
    if let Some(hint) = status.sync.as_ref().and_then(sync_wait_hint) {
        let _ = write!(out, "\n{}", hint);
    }

    let name = status.target_name.as_deref().unwrap_or("?");
    match &status.phase {
        ImportPhase::Failed { message } => {
            let _ = write!(out, "\nImport failed: {}", message);
        }
        ImportPhase::Done(ImportResult::Written(w)) => {
            let _ = write!(
                out,
                "\n{}",
                format_write_outcome(name, w.tracks as usize, w.created)
            );
        }
        ImportPhase::Done(ImportResult::DryRun { would_write }) => {
            let _ = write!(
                out,
                "\n[dry-run] Would write playlist '{}' with {} tracks",
                name, would_write
            );
        }
        ImportPhase::Done(ImportResult::NothingMatched) => {
            let _ = write!(
                out,
                "\nNo definitive matches; playlist '{}' not written",
                name
            );
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{hash, pl_track, sample_lookup, sample_tracks};
    use crate::{match_source_playlist, to_import_reports, TrackOutcome};
    use pretty_assertions::assert_eq;
    use source_protocol::ImportWritten;

    fn status_with(phase: ImportPhase, tracks: Vec<ImportTrackReport>) -> ImportJobStatus {
        ImportJobStatus {
            job_id: 1,
            dry_run: false,
            playlist_title: Some("Mix".into()),
            target_name: Some("mix".into()),
            total_count: Some(tracks.len() as u32),
            phase,
            tracks,
            sync: None,
        }
    }

    fn tr(position: u32, outcome: ImportTrackOutcome) -> ImportTrackReport {
        ImportTrackReport {
            position,
            artist: "A".into(),
            title: format!("T{position}"),
            outcome,
        }
    }

    fn status(phase: ImportPhase) -> ImportJobStatus {
        let mut s = status_with(
            phase,
            vec![
                tr(
                    1,
                    ImportTrackOutcome::Matched {
                        hash: "sha256:1111".into(),
                    },
                ),
                tr(
                    2,
                    ImportTrackOutcome::Ambiguous {
                        candidates: vec!["sha256:2222".into(), "sha256:3333".into()],
                    },
                ),
                tr(
                    3,
                    ImportTrackOutcome::DurationMismatch {
                        hash: "sha256:5555".into(),
                        library_secs: 210,
                        source_secs: 545,
                    },
                ),
                tr(4, ImportTrackOutcome::Missing { not_owned: false }),
                tr(5, ImportTrackOutcome::Missing { not_owned: true }),
            ],
        );
        s.total_count = Some(5);
        s
    }

    fn written(created: bool, tracks: u32) -> ImportPhase {
        ImportPhase::Done(ImportResult::Written(ImportWritten { created, tracks }))
    }

    #[test]
    fn truncation_warning_only_when_counts_differ() {
        assert_eq!(truncation_warning(64, 64), None);
        let w = truncation_warning(49, 64).unwrap();
        assert_eq!(
            w,
            "Warning: fetched 49 of 64 tracks; playlist may be incomplete"
        );
    }

    #[test]
    fn write_outcome_created_is_one_line() {
        assert_eq!(
            format_write_outcome("mix", 3, true),
            "Created playlist 'mix' with 3 tracks"
        );
    }

    #[test]
    fn write_outcome_replaced_warns_about_manual_edits() {
        assert_eq!(
            format_write_outcome("mix", 3, false),
            "Replaced playlist 'mix' with 3 tracks\n\
             Note: re-running this import replaces playlist 'mix' again, \
             including any manual edits."
        );
    }

    #[test]
    fn matched_pipeline_lists_each_position_and_summary() {
        let tracks = sample_tracks();
        let outcomes = match_source_playlist(&tracks, &sample_lookup());
        let s = status_with(ImportPhase::Matching, to_import_reports(&tracks, &outcomes));
        let report = format_import_status(&s);
        let lines: Vec<&str> = report.lines().skip(1).collect();
        assert!(lines[0].contains("1. ✓ matched") && lines[0].contains("A - One"));
        assert!(lines[1].contains("2. ? ambiguous") && lines[1].contains("B - Two"));
        assert!(lines[2].contains("candidate: sha256:2222"));
        assert!(lines[3].contains("candidate: sha256:3333"));
        assert!(lines[4].contains("3. ✗ not in library") && lines[4].contains("C - Three"));
        assert!(lines[5].contains("4. ✓ matched"));
        assert_eq!(
            lines.last().unwrap(),
            &"2 matched, 1 ambiguous, 1 missing of 4"
        );
    }

    #[test]
    fn unowned_missing_tracks_are_flagged() {
        let mut tracks = [pl_track(1, "C", "Three", 150.0)];
        tracks[0].owned = false;
        let s = status_with(
            ImportPhase::Matching,
            to_import_reports(&tracks, &[TrackOutcome::Missing]),
        );
        assert!(format_import_status(&s).contains("(not owned)"));
    }

    #[test]
    fn duration_mismatch_report_shows_both_durations() {
        let tracks = [pl_track(1, "A", "One", 545.088)];
        let outcomes = [TrackOutcome::DurationMismatch {
            hash: hash("5555555555555555"),
            library_secs: 210,
            source_secs: 545,
        }];
        let s = status_with(ImportPhase::Matching, to_import_reports(&tracks, &outcomes));
        let report = format_import_status(&s);
        assert!(
            report.contains("? duration differs (library 3:30 vs bandcamp 9:05)"),
            "got: {report}"
        );
        assert!(report.ends_with("0 matched, 1 ambiguous, 0 missing of 1"));
    }

    #[test]
    fn renders_every_outcome_variant() {
        let out = format_import_status(&status(ImportPhase::Writing));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Playlist 'Mix' (5 of 5 tracks)");
        assert!(lines[1].contains("1. ✓ matched") && lines[1].contains("A - T1"));
        assert!(lines[2].contains("2. ? ambiguous"));
        assert!(lines[3].contains("candidate: sha256:2222"));
        assert!(lines[4].contains("candidate: sha256:3333"));
        assert!(
            lines[5].contains("? duration differs (library 3:30 vs bandcamp 9:05)"),
            "got: {out}"
        );
        assert!(lines[6].contains("candidate: sha256:5555"));
        assert!(lines[7].contains("4. ✗ not in library") && !lines[7].contains("not owned"));
        assert!(lines[8].contains("5. ✗ not in library") && lines[8].contains("(not owned)"));
        assert_eq!(lines[9], "1 matched, 2 ambiguous, 2 missing of 5");
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn done_created_and_replaced() {
        let out = format_import_status(&status(written(true, 1)));
        assert!(
            out.ends_with("Created playlist 'mix' with 1 tracks"),
            "got: {out}"
        );

        let out = format_import_status(&status(written(false, 1)));
        assert!(out.contains("Replaced playlist 'mix' with 1 tracks"));
        assert!(out.contains("replaces playlist 'mix' again"));
    }

    #[test]
    fn done_nothing_matched_says_not_written() {
        let out = format_import_status(&status(ImportPhase::Done(ImportResult::NothingMatched)));
        assert!(out.ends_with("No definitive matches; playlist 'mix' not written"));
    }

    #[test]
    fn dry_run_says_would_write() {
        let mut s = status(ImportPhase::Done(ImportResult::DryRun { would_write: 1 }));
        s.dry_run = true;
        let out = format_import_status(&s);
        assert!(
            out.ends_with("[dry-run] Would write playlist 'mix' with 1 tracks"),
            "got: {out}"
        );
    }

    #[test]
    fn dry_run_without_matches_says_not_written() {
        let mut s = status(ImportPhase::Done(ImportResult::NothingMatched));
        s.dry_run = true;
        let out = format_import_status(&s);
        assert!(
            out.ends_with("No definitive matches; playlist 'mix' not written"),
            "got: {out}"
        );
        assert!(!out.contains("Would write"));
    }

    #[test]
    fn failed_shows_message() {
        let out = format_import_status(&status(ImportPhase::Failed {
            message: "boom".into(),
        }));
        assert!(out.ends_with("Import failed: boom"));
    }

    #[test]
    fn truncation_warning_when_total_exceeds_tracks() {
        let mut s = status(ImportPhase::Done(ImportResult::NothingMatched));
        s.total_count = Some(64);
        let out = format_import_status(&s);
        assert!(out.contains("Playlist 'Mix' (5 of 64 tracks)"));
        assert!(out.contains("Warning: fetched 5 of 64 tracks; playlist may be incomplete"));
    }

    fn with_sync(completed: u32, failed: u32, ended: Option<SyncWaitEnd>) -> ImportJobStatus {
        let mut s = status(ImportPhase::Done(ImportResult::NothingMatched));
        s.sync = Some(ImportSyncProgress {
            queued: 5,
            completed,
            failed,
            ended,
        });
        s
    }

    #[test]
    fn paused_wait_says_how_many_are_not_fetched() {
        let out = format_import_status(&with_sync(1, 1, Some(SyncWaitEnd::Paused)));
        assert!(
            out.contains(
                "Downloads paused: 3 not fetched yet; resume downloads and re-run the import"
            ),
            "got: {out}"
        );
    }

    #[test]
    fn timed_out_wait_says_how_many_are_pending() {
        let out = format_import_status(&with_sync(2, 0, Some(SyncWaitEnd::TimedOut)));
        assert!(
            out.contains("Timed out waiting for 3 downloads; re-run the import later"),
            "got: {out}"
        );
    }

    #[test]
    fn finished_or_ongoing_wait_adds_no_hint() {
        for ended in [Some(SyncWaitEnd::AllFinished), None] {
            let out = format_import_status(&with_sync(5, 0, ended));
            assert!(
                !out.contains("paused") && !out.contains("Timed out"),
                "got: {out}"
            );
        }
    }

    #[test]
    fn no_removed_sync_hint() {
        let out = format_import_status(&status(ImportPhase::Done(ImportResult::NothingMatched)));
        assert!(!out.contains("mdma source sync"));
    }
}
