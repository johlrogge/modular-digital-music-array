//! Plumbing shared by the background jobs (playlist import, check-updates):
//! one status slot per job kind, with its id counter, and a spawn that turns a
//! panicking or cancelled job into a reported failure.

use parking_lot::Mutex;
use source_protocol::{CheckJobStatus, ImportJobStatus, ImportPhase, PlaylistSyncJobStatus};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

/// What a job status must tell the slot.
pub(crate) trait JobStatus {
    fn job_id(&self) -> u64;
    /// Whether the job is still doing work (not Done or Failed).
    fn is_running(&self) -> bool;
}

impl JobStatus for ImportJobStatus {
    fn job_id(&self) -> u64 {
        self.job_id
    }

    fn is_running(&self) -> bool {
        !matches!(
            self.phase,
            ImportPhase::Done(_) | ImportPhase::Failed { .. }
        )
    }
}

impl JobStatus for PlaylistSyncJobStatus {
    fn job_id(&self) -> u64 {
        self.job_id
    }

    fn is_running(&self) -> bool {
        !self.phase.is_finished()
    }
}

impl JobStatus for CheckJobStatus {
    fn job_id(&self) -> u64 {
        self.job_id
    }

    fn is_running(&self) -> bool {
        !self.phase.is_finished()
    }
}

/// The current or last job of one kind, and the ids it hands out.
pub(crate) struct JobSlot<S> {
    status: Mutex<Option<S>>,
    last_id: AtomicU64,
}

impl<S: JobStatus + Clone> JobSlot<S> {
    /// An empty slot. Ids start from the clock (unix seconds, shifted), so a
    /// restarted service never reuses an id a client may still be following.
    pub(crate) fn new() -> Self {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::starting_after(seconds << 16)
    }

    fn starting_after(last_id: u64) -> Self {
        Self {
            status: Mutex::new(None),
            last_id: AtomicU64::new(last_id),
        }
    }

    /// Start a job: `initial` builds its status from the new id. `Err` carries
    /// the id of the job that is still running.
    pub(crate) fn begin(&self, initial: impl FnOnce(u64) -> S) -> Result<u64, u64> {
        let mut status = self.status.lock();
        if let Some(running) = status.as_ref().filter(|s| s.is_running()) {
            return Err(running.job_id());
        }
        let job_id = self.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        *status = Some(initial(job_id));
        Ok(job_id)
    }

    /// Update the status of job `job_id`. Ignored when the current status
    /// belongs to another job, so a stale task cannot overwrite a newer one.
    pub(crate) fn update(&self, job_id: u64, update: impl FnOnce(&mut S)) {
        if let Some(status) = self.status.lock().as_mut().filter(|s| s.job_id() == job_id) {
            update(status);
        }
    }

    /// The current or last status.
    pub(crate) fn snapshot(&self) -> Option<S> {
        self.status.lock().clone()
    }
}

/// Describe why a spawned job task did not return.
pub(crate) fn join_error_message(e: &tokio::task::JoinError) -> String {
    if e.is_panic() {
        format!("job panicked: {e}")
    } else {
        format!("job cancelled: {e}")
    }
}

/// Run `job` in its own task, so a panic surfaces as a JoinError here
/// instead of leaving the status stuck in a running phase; `on_abort` gets
/// the reason when the task did not return.
pub(crate) fn spawn_guarded(
    job: impl Future<Output = ()> + Send + 'static,
    on_abort: impl FnOnce(String) + Send + 'static,
) {
    tokio::spawn(async move {
        if let Err(e) = tokio::spawn(job).await {
            on_abort(join_error_message(&e));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_protocol::CheckPhase;

    fn check_status(job_id: u64) -> CheckJobStatus {
        CheckJobStatus {
            job_id,
            total: 0,
            phase: CheckPhase::Checking,
            results: Vec::new(),
        }
    }

    #[test]
    fn a_running_job_refuses_a_second_one_until_it_finishes() {
        let slot = JobSlot::<CheckJobStatus>::starting_after(0);
        assert_eq!(slot.begin(check_status), Ok(1));
        assert_eq!(slot.begin(check_status), Err(1));

        slot.update(1, |s| s.phase = CheckPhase::Done);
        assert_eq!(slot.begin(check_status), Ok(2));
    }

    #[test]
    fn an_update_for_another_job_is_ignored() {
        let slot = JobSlot::<CheckJobStatus>::starting_after(0);
        slot.begin(check_status).unwrap();
        slot.update(1, |s| s.phase = CheckPhase::Done);
        slot.begin(check_status).unwrap();

        slot.update(1, |s| {
            s.phase = CheckPhase::Failed {
                message: "late".into(),
            }
        });

        assert_eq!(slot.snapshot().unwrap().phase, CheckPhase::Checking);
    }

    #[test]
    fn ids_start_from_the_clock_not_from_one() {
        let slot = JobSlot::<CheckJobStatus>::new();
        assert!(slot.begin(check_status).unwrap() > 1 << 16);
    }

    #[tokio::test]
    async fn a_panicking_job_is_reported_to_on_abort() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        spawn_guarded(async { panic!("boom") }, move |message| {
            tx.send(message).unwrap();
        });
        let message = rx.await.unwrap();
        assert!(message.contains("panicked"), "got: {message}");
    }
}
