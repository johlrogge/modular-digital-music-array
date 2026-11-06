use crate::output::{lost_contact, success_unless};
use crate::source::import_status_command;
use color_eyre::Result;
use hicli::Success;
use mdma_client::SourceClient;
use playlist_import::format_import_status;
use source_protocol::{
    ImportJobStatus, ImportPhase, ImportSyncProgress, SourceRequest, SourceResponse,
};

const IMPORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Consecutive failed polls tolerated before giving up (about a minute).
const IMPORT_POLL_MAX_FAILURES: u32 = 30;

pub(crate) fn is_terminal_phase(phase: &ImportPhase) -> bool {
    matches!(phase, ImportPhase::Done(_) | ImportPhase::Failed { .. })
}

fn import_phase_line(phase: &ImportPhase) -> &'static str {
    match phase {
        ImportPhase::Fetching => "Fetching playlist...",
        ImportPhase::Matching => "Matching tracks against the library...",
        ImportPhase::Syncing => {
            "Syncing Bandcamp collection (missing tracks found), waiting for downloads..."
        }
        ImportPhase::Rematching => "Matching tracks again after downloads...",
        ImportPhase::Writing => "Writing playlist...",
        ImportPhase::Done(_) => "Import finished",
        ImportPhase::Failed { .. } => "Import failed",
    }
}

/// What one poll attempt produced.
enum PollInput {
    /// The server answered with its current/last job status.
    Status(Option<ImportJobStatus>),
    /// Request failed, timed out, or got an unusable answer.
    Failure,
}

/// What the poll loop should do next.
#[derive(Debug, PartialEq)]
enum PollFlow {
    Continue,
    /// The awaited job reached Done or Failed.
    Finished(Box<ImportJobStatus>),
    /// Too many consecutive failures.
    GiveUp,
}

#[derive(Debug, PartialEq)]
struct PollStep {
    /// Progress lines to print, in order.
    lines: Vec<String>,
    flow: PollFlow,
}

/// Pure state machine behind the import poll loop (no I/O).
struct ImportPoller {
    job_id: u64,
    last_phase: Option<ImportPhase>,
    last_sync: Option<ImportSyncProgress>,
    failures: u32,
}

impl ImportPoller {
    fn new(job_id: u64) -> Self {
        Self {
            job_id,
            last_phase: None,
            last_sync: None,
            failures: 0,
        }
    }

    /// A miss counts as a failure: failed requests, no job, or a status for
    /// a different job (which is otherwise ignored).
    fn miss(&mut self) -> PollStep {
        self.failures += 1;
        let flow = if self.failures >= IMPORT_POLL_MAX_FAILURES {
            PollFlow::GiveUp
        } else {
            PollFlow::Continue
        };
        PollStep {
            lines: Vec::new(),
            flow,
        }
    }

    fn step(&mut self, input: PollInput) -> PollStep {
        let status = match input {
            PollInput::Status(Some(s)) if s.job_id == self.job_id => s,
            PollInput::Status(_) | PollInput::Failure => return self.miss(),
        };
        self.failures = 0;

        let mut lines = Vec::new();
        if self.last_phase.as_ref() != Some(&status.phase) {
            lines.push(import_phase_line(&status.phase).to_string());
            self.last_phase = Some(status.phase.clone());
        }
        if status.phase == ImportPhase::Syncing {
            if let Some(sync) = &status.sync {
                if self.last_sync.as_ref() != Some(sync) {
                    lines.push(format!(
                        "downloads: {}/{} (failed {})",
                        sync.completed, sync.queued, sync.failed
                    ));
                    self.last_sync = Some(sync.clone());
                }
            }
        }

        let flow = if is_terminal_phase(&status.phase) {
            PollFlow::Finished(Box::new(status))
        } else {
            PollFlow::Continue
        };
        PollStep { lines, flow }
    }
}

/// Print the final report; the exit code is 1 when the job failed.
pub(crate) fn finish_import(status: &ImportJobStatus) -> Success {
    let report = format_import_status(status);
    let failed = matches!(status.phase, ImportPhase::Failed { .. });
    if failed {
        eprintln!("{}", report);
    } else {
        println!("{}", report);
    }
    success_unless(failed)
}

/// Poll the server until `job_id` finishes, then print its report.
pub(crate) fn follow_import_job(source: &SourceClient, name: &str, job_id: u64) -> Result<Success> {
    let mut poller = ImportPoller::new(job_id);
    let mut last_error: Option<String> = None;
    loop {
        let input = match source.request(name, &SourceRequest::GetImportStatus) {
            Ok(SourceResponse::ImportStatus(s)) => PollInput::Status(s),
            Ok(SourceResponse::Error(e)) => {
                last_error = Some(e.to_string());
                PollInput::Failure
            }
            Ok(_) => {
                last_error = Some("Unexpected response".to_string());
                PollInput::Failure
            }
            Err(e) => {
                last_error = Some(e.to_string());
                PollInput::Failure
            }
        };
        let step = poller.step(input);
        for line in &step.lines {
            println!("{}", line);
        }
        match step.flow {
            PollFlow::Continue => std::thread::sleep(IMPORT_POLL_INTERVAL),
            PollFlow::Finished(status) => return Ok(finish_import(&status)),
            PollFlow::GiveUp => return Err(lost_import_job(name, job_id, last_error.as_deref())),
        }
    }
}

/// The failure for an import job whose status could not be polled any more.
fn lost_import_job(name: &str, job_id: u64, last_error: Option<&str>) -> color_eyre::Report {
    lost_contact(
        &format!("the import job {job_id}"),
        &last_error.unwrap_or("no status for the job"),
        &import_status_command(name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use source_protocol::{ImportJobStatus, ImportPhase, ImportSyncProgress};

    use pretty_assertions::assert_eq;
    use source_protocol::ImportResult;

    fn job(job_id: u64, phase: ImportPhase, sync: Option<(u32, u32, u32)>) -> ImportJobStatus {
        ImportJobStatus {
            job_id,
            dry_run: false,
            playlist_title: None,
            target_name: None,
            total_count: None,
            phase,
            tracks: vec![],
            sync: sync.map(|(queued, completed, failed)| ImportSyncProgress {
                queued,
                completed,
                failed,
                ended: None,
            }),
        }
    }

    fn done() -> ImportPhase {
        ImportPhase::Done(ImportResult::NothingMatched)
    }

    #[test]
    fn lost_import_job_names_the_job_the_cause_and_how_to_check() {
        let text = |cause| {
            crate::test_support::failure_text::<()>(Err(lost_import_job("bandcamp", 7, cause)))
        };
        assert_eq!(
            text(Some("timed out")),
            "Lost contact with the import job 7 (timed out); \
             check later with 'mdma source bandcamp import-status'"
        );
        assert_eq!(
            text(None),
            "Lost contact with the import job 7 (no status for the job); \
             check later with 'mdma source bandcamp import-status'"
        );
    }

    #[test]
    fn a_failed_import_ends_with_exit_code_one_and_a_finished_one_with_zero() {
        let failed = job(
            1,
            ImportPhase::Failed {
                message: "x".into(),
            },
            None,
        );
        assert_eq!(finish_import(&failed), Success::code(1));
        assert_eq!(finish_import(&job(1, done(), None)), Success::OK);
    }

    #[test]
    fn syncing_phase_line_says_a_full_collection_sync_runs() {
        let line = import_phase_line(&ImportPhase::Syncing);
        assert!(line.contains("Syncing Bandcamp collection"), "got: {line}");
        assert!(line.contains("missing tracks found"), "got: {line}");
    }

    #[test]
    fn done_with_any_result_is_terminal() {
        assert!(is_terminal_phase(&ImportPhase::Done(
            ImportResult::DryRun { would_write: 1 }
        )));
        assert!(is_terminal_phase(&done()));
        assert!(!is_terminal_phase(&ImportPhase::Writing));
    }

    fn status_input(s: ImportJobStatus) -> PollInput {
        PollInput::Status(Some(s))
    }

    #[test]
    fn poller_prints_line_only_on_phase_change() {
        let mut p = ImportPoller::new(1);
        let s1 = p.step(status_input(job(1, ImportPhase::Fetching, None)));
        assert_eq!(s1.lines, vec!["Fetching playlist...".to_string()]);
        assert_eq!(s1.flow, PollFlow::Continue);
        let s2 = p.step(status_input(job(1, ImportPhase::Fetching, None)));
        assert!(s2.lines.is_empty());
        let s3 = p.step(status_input(job(1, ImportPhase::Matching, None)));
        assert_eq!(s3.lines.len(), 1);
    }

    #[test]
    fn poller_prints_download_progress_only_when_it_changes() {
        let mut p = ImportPoller::new(1);
        let a = p.step(status_input(job(1, ImportPhase::Syncing, Some((5, 1, 0)))));
        assert_eq!(a.lines.len(), 2);
        assert_eq!(a.lines[1], "downloads: 1/5 (failed 0)");
        let b = p.step(status_input(job(1, ImportPhase::Syncing, Some((5, 1, 0)))));
        assert!(b.lines.is_empty());
        let c = p.step(status_input(job(1, ImportPhase::Syncing, Some((5, 3, 1)))));
        assert_eq!(c.lines, vec!["downloads: 3/5 (failed 1)".to_string()]);
    }

    #[test]
    fn poller_finishes_on_done_and_failed() {
        let mut p = ImportPoller::new(1);
        let done = job(1, done(), None);
        let step = p.step(status_input(done.clone()));
        assert_eq!(step.flow, PollFlow::Finished(Box::new(done)));

        let mut p = ImportPoller::new(1);
        let failed = job(
            1,
            ImportPhase::Failed {
                message: "x".into(),
            },
            None,
        );
        let step = p.step(status_input(failed.clone()));
        assert_eq!(step.flow, PollFlow::Finished(Box::new(failed)));
    }

    #[test]
    fn poller_ignores_other_jobs_status() {
        let mut p = ImportPoller::new(1);
        let step = p.step(status_input(job(2, done(), None)));
        assert_eq!(step.flow, PollFlow::Continue);
        assert!(step.lines.is_empty());
    }

    #[test]
    fn poller_gives_up_after_consecutive_failures() {
        let mut p = ImportPoller::new(1);
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            assert_eq!(p.step(PollInput::Failure).flow, PollFlow::Continue);
        }
        assert_eq!(p.step(PollInput::Failure).flow, PollFlow::GiveUp);
    }

    #[test]
    fn poller_success_resets_failure_count() {
        let mut p = ImportPoller::new(1);
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            p.step(PollInput::Failure);
        }
        p.step(status_input(job(1, ImportPhase::Matching, None)));
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            assert_eq!(p.step(PollInput::Failure).flow, PollFlow::Continue);
        }
    }

    #[test]
    fn poller_counts_missing_job_as_failure() {
        let mut p = ImportPoller::new(1);
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            p.step(PollInput::Status(None));
        }
        assert_eq!(p.step(PollInput::Status(None)).flow, PollFlow::GiveUp);
    }
}
