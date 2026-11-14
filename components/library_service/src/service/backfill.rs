//! Explicit identity-hash backfill (ADR-010, ADR-012).
//!
//! Started only by an `IdentityBackfill` request, never at startup. Runs on a
//! dedicated low-priority worker thread so it cannot block library requests or
//! compete with playback. Resumable: tracks that already carry the requested
//! hash facts are left out when the run is planned, and cancelling keeps progress.

use super::*;
use crate::identity::{drop_page_cache, lower_current_thread_priority};
use crate::ipc::{IdentityBackfillProgress, IdentityBackfillStart, IdentityBackfillStatus};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Duty cycle: after hashing a track the worker sleeps as long as the hashing took
/// (so it uses at most about half a core), but never less than this. Keeps a long
/// backfill from saturating the disk and CPU next to playback.
const MIN_DUTY_SLEEP: Duration = Duration::from_millis(20);
/// Granularity at which a sleeping worker notices a cancel request.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// One track to process, with only the hashes it is still missing.
struct Work {
    hash: ContentHash,
    blob: PathBuf,
    opts: IdentityOpts,
}

/// Result of re-checking a planned track right before writing.
enum Recheck {
    /// The entity is gone (retired since planning): write nothing.
    Retired,
    /// The facts the live track still lacks (possibly none).
    Write(Vec<(MusicValue, music_facts::FactSource)>),
}

fn progress_mut(status: &mut IdentityBackfillStatus) -> Option<&mut IdentityBackfillProgress> {
    match status {
        IdentityBackfillStatus::Idle => None,
        IdentityBackfillStatus::Running(p)
        | IdentityBackfillStatus::Done(p)
        | IdentityBackfillStatus::Cancelled(p) => Some(p),
    }
}

impl LibraryService {
    pub(super) fn identity_backfill_status(&self) -> IdentityBackfillStatus {
        self.identity_backfill.lock().unwrap().clone()
    }

    /// Handle `CancelIdentityBackfill`: ask a running worker to stop after the
    /// track in hand. The state flips to `Cancelled` when the worker notices.
    pub(super) fn cancel_identity_backfill(&self) -> IdentityBackfillStatus {
        let status = self.identity_backfill.lock().unwrap();
        if matches!(*status, IdentityBackfillStatus::Running(_)) {
            self.identity_cancel.store(true, Ordering::Relaxed);
        }
        status.clone()
    }

    /// Plan the run: tracks with a blob that lack a requested hash fact.
    fn plan_identity_backfill(&self, include_pcm: bool) -> Vec<Work> {
        let tracks = self.tracks.lock().unwrap();
        tracks
            .iter()
            .filter(|t| !t.blob_path.as_os_str().is_empty())
            .filter_map(|t| {
                let opts = IdentityOpts::missing(include_pcm, t.provenance.iter().map(|(v, _)| v))?;
                Some(Work {
                    hash: t.content_hash.clone(),
                    blob: self.music_dir.join(&t.blob_path),
                    opts,
                })
            })
            .collect()
    }

    /// Handle `IdentityBackfill`: start the worker, or report that one is running.
    /// If the worker thread cannot be spawned the status is left as it was.
    pub(super) fn start_identity_backfill(
        &self,
        include_pcm: bool,
    ) -> Result<IdentityBackfillStart, ProtocolError> {
        let mut status = self.identity_backfill.lock().unwrap();
        if let IdentityBackfillStatus::Running(p) = &*status {
            return Ok(IdentityBackfillStart::AlreadyRunning(p.clone()));
        }

        let shared = self
            .self_ref
            .upgrade()
            .expect("the service is alive while it handles a request");

        let work = self.plan_identity_backfill(include_pcm);
        let progress = IdentityBackfillProgress {
            include_pcm,
            total: work.len(),
            done: 0,
            failed: 0,
            skipped: 0,
            last_error: None,
        };
        self.identity_cancel.store(false, Ordering::Relaxed);
        // Running before the spawn, so the worker's first update finds it.
        let previous = std::mem::replace(
            &mut *status,
            IdentityBackfillStatus::Running(progress.clone()),
        );

        let spawned = std::thread::Builder::new()
            .name("identity-backfill".into())
            .spawn(move || shared.run_identity_backfill(work));
        if let Err(e) = spawned {
            *status = previous;
            return Err(ProtocolError::Internal {
                message: format!("failed to start identity backfill worker: {e}"),
            });
        }

        Ok(IdentityBackfillStart::Started(progress))
    }

    fn update_backfill(&self, f: impl FnOnce(&mut IdentityBackfillProgress)) {
        if let Some(p) = progress_mut(&mut self.identity_backfill.lock().unwrap()) {
            f(p);
        }
    }

    /// Worker thread body. Always ends by leaving the run `Done` or `Cancelled`.
    fn run_identity_backfill(&self, work: Vec<Work>) {
        lower_current_thread_priority();

        // Backstop only: hashing panics are already contained per hash in
        // `identity_facts_for_blob`.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.identity_backfill_loop(&work)
        }));
        if outcome.is_err() {
            self.update_backfill(|p| p.last_error = Some("backfill worker panicked".into()));
        }

        // The cancel flag is read under the status lock: a cancel that raced in after
        // the last track leaves nothing to cancel, so that run is `Done`.
        let mut status = self.identity_backfill.lock().unwrap();
        let cancelled = self.identity_cancel.load(Ordering::Relaxed);
        if let IdentityBackfillStatus::Running(p) = &*status {
            let p = p.clone();
            let cancelled = cancelled && p.done < p.total;
            tracing::info!(
                total = p.total,
                done = p.done,
                failed = p.failed,
                skipped = p.skipped,
                cancelled,
                "Identity backfill finished"
            );
            *status = if cancelled {
                IdentityBackfillStatus::Cancelled(p)
            } else {
                IdentityBackfillStatus::Done(p)
            };
        }
    }

    /// Re-check a planned track against the live index. The caller must hold
    /// `retire_gate`, otherwise the answer can be stale by the time it writes.
    fn recheck_identity_facts(
        &self,
        _gate: &super::RetireGuard<'_>,
        hash: &ContentHash,
        facts: Vec<(MusicValue, music_facts::FactSource)>,
    ) -> Recheck {
        let tracks = self.tracks.lock().unwrap();
        let Some(track) = tracks
            .iter()
            .find(|t| t.content_hash.as_str() == hash.as_str())
        else {
            return Recheck::Retired;
        };
        let Some(missing) = IdentityOpts::missing(true, track.provenance.iter().map(|(v, _)| v))
        else {
            return Recheck::Write(Vec::new());
        };
        Recheck::Write(
            facts
                .into_iter()
                .filter(|(v, _)| match v {
                    MusicValue::AudioHash(_) => missing.audio_hash,
                    MusicValue::PcmHash(_) => missing.pcm_hash,
                    _ => false,
                })
                .collect(),
        )
    }

    /// Sleep `d`, waking early when a cancel is requested.
    fn pause_unless_cancelled(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.identity_cancel.load(Ordering::Relaxed) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(CANCEL_POLL));
        }
    }

    fn identity_backfill_loop(&self, work: &[Work]) {
        // The worker has its own ACID client: nng REQ sockets must not be shared
        // between concurrent requests, and the request thread uses `self.acid_client`.
        let client = match AcidClient::connect(&self.acid_socket) {
            Ok(c) => c,
            Err(e) => {
                // Every track counts as handled and failed, as the protocol defines.
                self.update_backfill(|p| {
                    p.done = p.total;
                    p.failed = p.total;
                    p.last_error = Some(format!("cannot connect to ACID: {e}"));
                });
                return;
            }
        };
        let source = sources::derived();

        // One projection for the whole run, also on unwind: the fact subscriber
        // projects writes in production anyway, so nothing needs them sooner.
        self.write_batch_and_project_with(&client, |writer| {
            for w in work {
                if self.identity_cancel.load(Ordering::Relaxed) {
                    break;
                }

                let started = Instant::now();
                let computed = identity_facts_for_blob(&w.blob, w.opts, &source);
                drop_page_cache(&w.blob);
                let hash_failed = !computed.errors.is_empty();

                #[cfg(test)]
                if let Some(hook) = self.identity_write_hook.lock().unwrap().as_ref() {
                    hook(self, &w.hash);
                }

                // The plan is a snapshot: the track may be retired (hard replace, later
                // a duplicate merge) while its blob is being hashed, and the blob stays
                // on disk. Writing then would resurrect the entity, because identity
                // facts are provenance-tracked. So re-check liveness right before
                // writing, and hold `retire_gate` from the re-check until the write
                // returns: a retire either finished before the re-check (entity gone,
                // nothing written) or starts after the write (its read sees our fact).
                // ACID has no conditional write, so the gate is what closes the window.
                // The one case left is a retire whose catch-up failed: the entity is
                // gone in ACID but still indexed, and that failure is logged.
                let had_facts = !computed.facts.is_empty();
                let (retired, to_write_empty, write_error) = {
                    let gate = self.retire_gate.lock().unwrap();
                    match self.recheck_identity_facts(&gate, &w.hash, computed.facts) {
                        Recheck::Retired => (true, true, None),
                        Recheck::Write(to_write) if to_write.is_empty() => (false, true, None),
                        Recheck::Write(to_write) => {
                            let err = match writer.write_music_facts(&w.hash, &to_write) {
                                Ok(_) => None,
                                Err(e) => {
                                    tracing::warn!(
                                        hash = %w.hash.as_str(), error = %e,
                                        "identity fact write failed"
                                    );
                                    Some(format!("write failed: {e}"))
                                }
                            };
                            (false, false, err)
                        }
                    }
                };

                // Exclusive outcomes: retired is skipped (even if a hash failed too, the
                // failure is moot); then failed; then "hashed but nothing left to write".
                let failed = !retired && (hash_failed || write_error.is_some());
                let skipped = retired || (!failed && had_facts && to_write_empty);
                self.update_backfill(|p| {
                    p.done += 1;
                    if skipped {
                        p.skipped += 1;
                    }
                    if failed {
                        p.failed += 1;
                        p.last_error = write_error.or_else(|| computed.errors.last().cloned());
                    }
                });

                self.pause_unless_cancelled(started.elapsed().max(MIN_DUTY_SLEEP));
            }
        });
    }
}
