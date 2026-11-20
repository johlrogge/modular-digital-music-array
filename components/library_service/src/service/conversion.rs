//! The AIFF conversion worker (ADR-037, phase 6).
//!
//! An AIFF ingest request validates the file, hashes it, answers duplicates, and
//! otherwise queues a [`Job`] and answers `Pending`. One background thread (nice 19,
//! idle IO) takes the jobs in order and runs each to one of four ends ([`JobEnd`]):
//!
//! - **verified**: the FLAC goes into `blobs/`, then its facts are written, then the
//!   source AIFF is removed (the crash order of ADR-037 decision 7);
//! - **refused**: the encoder says the file cannot be converted or verified, so the
//!   AIFF is stored as an AIFF blob through the normal ingest, with a warning;
//! - **environment error** (no `flac`, disk I/O): the file stays in the inbox and the
//!   job is retried with a growing delay, a bounded number of times, and then shows
//!   as failed. It never becomes an AIFF blob because of this;
//! - **source gone**: nothing to do.
//!
//! Every job starts with the dedup check of a new ingest, so an AIFF that was
//! converted (an `EncodedFrom` alias) or stored meanwhile is not encoded again,
//! which is also how the AIFFs found in the inbox at startup are handled.

use super::*;
use crate::identity::lower_current_thread_priority;
use crate::ipc::{ConversionState, InboxConversion};
use crate::pipeline::{AudioFormat, ExtractedTrack, IngestError, IngestWarning, ValidatedAudio};
use lossless_encoder::{EncodeError, Encoder, Verified};
use std::collections::VecDeque;
use std::sync::{Condvar, MutexGuard};
use std::time::Duration;

/// Staging for converted files, under the music directory.
const STAGING_DIR: &str = "staging";
/// A SHA-256 as hex: the name of a verified FLAC waiting in staging.
const HEX_LEN: usize = 64;
/// The sidecars of the queued jobs, under staging (the sweep leaves it alone: it
/// removes files by name pattern only).
const JOBS_DIR: &str = "jobs";

/// What a queued job keeps across a restart: the request's own context.
#[derive(serde::Serialize, serde::Deserialize)]
struct JobContext {
    path: PathBuf,
    writer: Option<music_facts::FactSource>,
    facts: Vec<MusicValue>,
}

#[cfg(test)]
type SourceHook = Box<dyn Fn(&Path) + Send + Sync>;

/// How often, and how patiently, a conversion that failed for want of the machine
/// is tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Tries in all; the last failure makes the job `Failed`.
    pub max_attempts: u32,
    /// The wait after the first failure; it doubles with each one.
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(15 * 60),
        }
    }
}

impl RetryPolicy {
    /// The wait after the `failures`-th failure (1 for the first).
    fn delay(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(20);
        self.base_delay
            .saturating_mul(1u32 << doublings)
            .min(self.max_delay)
    }
}

/// What the conversion worker is built from. The default is the production one: the
/// `flac` on PATH and today's ingest policy.
pub struct ConversionConfig {
    encoder: Encoder,
    /// Take AIFF files in ingest before `AudioFormat::is_ingestible` does (ADR-037
    /// phase 8 removes this and its uses).
    accept_aiff: bool,
    retry: RetryPolicy,
    /// Test hook: called with the source AIFF after its facts are written and before
    /// it is removed, to change it in that window.
    #[cfg(test)]
    pub(super) before_source_removal: Option<SourceHook>,
}

impl Default for ConversionConfig {
    fn default() -> Self {
        Self {
            encoder: Encoder::new(),
            accept_aiff: false,
            retry: RetryPolicy::default(),
            #[cfg(test)]
            before_source_removal: None,
        }
    }
}

impl ConversionConfig {
    /// Encode with this encoder (a given `flac` binary, say).
    pub fn with_encoder(mut self, encoder: Encoder) -> Self {
        self.encoder = encoder;
        self
    }

    /// Whether ingest takes AIFF files, ahead of `AudioFormat::is_ingestible`.
    pub fn accepting_aiff(mut self, accept: bool) -> Self {
        self.accept_aiff = accept;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

/// One AIFF to convert.
pub(super) struct Job {
    path: PathBuf,
    /// The client of the ingest request, and the facts it states (as in `IngestFile`).
    writer: Option<music_facts::FactSource>,
    facts: Vec<MusicValue>,
    /// Failures so far.
    attempt: u32,
    /// Not before this, for a job waiting to retry.
    not_before: Option<Instant>,
    last_error: Option<String>,
}

impl Job {
    pub(super) fn new(
        path: PathBuf,
        writer: Option<music_facts::FactSource>,
        facts: Vec<MusicValue>,
    ) -> Self {
        Self {
            path,
            writer,
            facts,
            attempt: 0,
            not_before: None,
            last_error: None,
        }
    }
}

/// Removes the verified FLAC in staging when dropped.
struct StagedFlac(PathBuf);

impl Drop for StagedFlac {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// How a job ended.
#[derive(Debug)]
pub(super) enum JobEnd {
    /// In the library: converted, stored as AIFF, or a duplicate of what is.
    Stored {
        hash: ContentHash,
        warnings: Vec<IngestWarning>,
    },
    /// The file is gone from the inbox.
    SourceGone,
    /// The machine failed; try again later. The file stays in the inbox.
    Retry(String),
    /// Not to be retried; the file stays in the inbox.
    Failed(String),
}

#[derive(Default)]
struct Queue {
    /// In arrival order, retrying jobs among them (they wait for `not_before`).
    pending: VecDeque<Job>,
    current: Option<PathBuf>,
    /// Given up on, with the last error.
    failed: Vec<(PathBuf, String)>,
    /// Paths claimed by `try_reserve`, not queued yet.
    reserved: std::collections::HashSet<PathBuf>,
    shutdown: bool,
}

/// The job queue and what the worker needs to run it.
pub(super) struct Conversions {
    config: ConversionConfig,
    queue: Mutex<Queue>,
    wake: Condvar,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Conversions {
    pub(super) fn new(config: ConversionConfig) -> Self {
        Self {
            config,
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            worker: Mutex::new(None),
        }
    }

    pub(super) fn accepts(&self, format: &AudioFormat) -> bool {
        format.is_ingestible() || (*format == AudioFormat::Aiff && self.config.accept_aiff)
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap()
    }

    /// Claim `path` for a job about to be queued (`enqueue` releases it): false when
    /// the file is already queued, being converted or claimed, so the caller must not
    /// touch the context that job keeps.
    pub(super) fn try_reserve(&self, path: &Path) -> bool {
        let mut queue = self.lock();
        let known = queue.current.as_deref() == Some(path)
            || queue.pending.iter().any(|j| j.path == path)
            || queue.reserved.contains(path);
        if !known {
            queue.reserved.insert(path.to_path_buf());
        }
        !known
    }

    /// Queue `job`, unless that file is queued or being converted already. A file
    /// that failed before starts over.
    pub(super) fn enqueue(&self, job: Job) {
        let mut queue = self.lock();
        queue.reserved.remove(&job.path);
        queue.failed.retain(|(path, _)| *path != job.path);
        let known = queue.current.as_ref() == Some(&job.path)
            || queue.pending.iter().any(|j| j.path == job.path);
        if !known {
            queue.pending.push_back(job);
            self.wake.notify_all();
        }
    }

    /// The next job that is due, waiting for one. `None` once shut down.
    fn next_job(&self) -> Option<Job> {
        let mut queue = self.lock();
        loop {
            if queue.shutdown {
                return None;
            }
            let now = Instant::now();
            let due = queue
                .pending
                .iter()
                .position(|j| !matches!(j.not_before, Some(t) if t > now));
            if let Some(i) = due {
                let job = queue.pending.remove(i).expect("position is in range");
                queue.current = Some(job.path.clone());
                return Some(job);
            }
            let next_due = queue.pending.iter().filter_map(|j| j.not_before).min();
            queue = match next_due {
                Some(at) => {
                    self.wake
                        .wait_timeout(queue, at.saturating_duration_since(now))
                        .unwrap()
                        .0
                }
                None => self.wake.wait(queue).unwrap(),
            };
        }
    }

    fn finish(&self) {
        self.lock().current = None;
    }

    /// The machine failed `job`: wait and try again, or give up at the limit.
    fn retry_later(&self, mut job: Job, error: String) {
        let mut queue = self.lock();
        queue.current = None;
        job.attempt += 1;
        if job.attempt >= self.config.retry.max_attempts {
            queue.failed.push((job.path, error));
        } else {
            job.not_before = Some(Instant::now() + self.config.retry.delay(job.attempt));
            job.last_error = Some(error);
            queue.pending.push_back(job);
        }
    }

    fn give_up(&self, job: Job, error: String) {
        let mut queue = self.lock();
        queue.current = None;
        queue.failed.push((job.path, error));
    }

    /// Stop the worker after the job in hand, and wait for it to end.
    pub(super) fn shutdown(&self) {
        self.request_shutdown();
        let handle = self.worker.lock().unwrap().take();
        if let Some(handle) = handle {
            if handle.thread().id() != std::thread::current().id() {
                let _ = handle.join();
            }
        }
    }

    /// [`shutdown`](Self::shutdown), but give up waiting after `limit`: answers
    /// whether the worker had ended. A worker still encoding is left running (it ends
    /// with the process; the crash order makes that safe).
    pub(super) fn shutdown_timeout(&self, limit: Duration) -> bool {
        self.request_shutdown();
        let Some(handle) = self.worker.lock().unwrap().take() else {
            return true;
        };
        let deadline = Instant::now() + limit;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
        true
    }

    /// Ask the worker to stop, without waiting (for `Drop`).
    pub(super) fn request_shutdown(&self) {
        self.lock().shutdown = true;
        self.wake.notify_all();
    }

    /// Every file the worker holds, relative to the inbox: the one being converted,
    /// then the others in order, then those given up on that are still there.
    pub(super) fn status(&self, inbox: &Path) -> Vec<InboxConversion> {
        let queue = self.lock();
        let entry = |path: &Path, state: ConversionState| {
            let relative = path.strip_prefix(inbox).ok()?.to_str()?;
            Some(InboxConversion {
                path: InboxPath::new(relative).ok()?,
                state,
            })
        };
        let current = queue
            .current
            .iter()
            .filter_map(|p| entry(p, ConversionState::Converting));
        let pending = queue.pending.iter().filter_map(|j| {
            let state = match &j.last_error {
                Some(last_error) if j.attempt > 0 => ConversionState::Retrying {
                    attempt: j.attempt,
                    last_error: last_error.clone(),
                },
                _ => ConversionState::Queued,
            };
            entry(&j.path, state)
        });
        let failed = queue
            .failed
            .iter()
            .filter(|(path, _)| path.exists())
            .filter_map(|(path, message)| {
                entry(
                    path,
                    ConversionState::Failed {
                        message: message.clone(),
                    },
                )
            });
        current.chain(pending).chain(failed).collect()
    }
}

/// The wire form of the warnings of an ingest.
pub(super) fn wire_warnings(warnings: &[IngestWarning]) -> Vec<crate::ipc::IngestWarning> {
    warnings
        .iter()
        .map(|w| match w {
            IngestWarning::TagsUnreadable { detail } => crate::ipc::IngestWarning::TagsUnreadable {
                detail: detail.clone(),
            },
            IngestWarning::StoredAsAiff { reason } => crate::ipc::IngestWarning::StoredAsAiff {
                reason: reason.clone(),
            },
        })
        .collect()
}

/// How a failed store ends a job: I/O and ACID are the machine's, the rest the
/// file's.
fn end_for_error(error: ServiceError) -> JobEnd {
    match error {
        ServiceError::Io(_)
        | ServiceError::Ipc(_)
        | ServiceError::Acid(_)
        | ServiceError::Ingest(IngestError::Io(_)) => JobEnd::Retry(error.to_string()),
        // `ConversionPending` comes only from `ingest_file_internal`, which the worker
        // never calls: this arm is unreachable there, and only makes the match total.
        ServiceError::Ingest(_) | ServiceError::ConversionPending => {
            JobEnd::Failed(error.to_string())
        }
    }
}

impl LibraryService {
    pub(super) fn accepts_format(&self, format: &AudioFormat) -> bool {
        self.conversions.accepts(format)
    }

    fn staging_dir(&self) -> PathBuf {
        self.music_dir.join(STAGING_DIR)
    }

    /// Stop the conversion worker after the job in hand and wait for it. The worker
    /// also stops when the service is dropped.
    pub fn shutdown_conversions(&self) {
        self.conversions.shutdown();
    }

    /// [`shutdown_conversions`](Self::shutdown_conversions), waiting at most `limit`
    /// for a job in hand. Answers whether the worker had ended.
    pub fn shutdown_conversions_timeout(&self, limit: Duration) -> bool {
        self.conversions.shutdown_timeout(limit)
    }

    /// The conversion worker's view of the inbox, for the status.
    pub(super) fn conversion_status(&self) -> Vec<InboxConversion> {
        self.conversions.status(&self.music_dir.join("inbox"))
    }

    /// Startup: remove what a crashed conversion left in staging, never promoting any
    /// of it (the sweep runs before the worker starts, so nothing in staging is in
    /// use), and queue the AIFFs still in the inbox. Their jobs dedup first, so one
    /// that was converted but not removed is only cleaned up.
    ///
    /// A job comes back with the writer and facts of the request that queued it (its
    /// sidecar in `staging/jobs`); an AIFF with no sidecar is queued as a plain ingest.
    pub(super) fn recover_conversions(&self) {
        self.sweep_staging();
        if !self.accepts_format(&AudioFormat::Aiff) {
            return;
        }
        let mut jobs = self.load_job_contexts();
        for path in self.get_inbox_queue_internal() {
            let is_aiff = path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(AudioFormat::from_extension)
                == Some(AudioFormat::Aiff);
            if is_aiff {
                jobs.push(Job::new(path, None, Vec::new()));
            }
        }
        // The queue holds a path once, so a sidecar's job wins over a plain one.
        for job in jobs {
            self.conversions.enqueue(job);
        }
    }

    fn jobs_dir(&self) -> PathBuf {
        self.staging_dir().join(JOBS_DIR)
    }

    /// The sidecar of the job for `path`.
    fn context_file(&self, path: &Path) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        // FNV-1a: a stable name for a path (the file holds the path itself).
        let hash = path
            .as_os_str()
            .as_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
            });
        self.jobs_dir().join(format!("{hash:016x}.json"))
    }

    /// Queue `job` and keep what the request stated (writer, facts) in a sidecar
    /// until the job ends, so a restart does not lose it: the ItemId of a purchase
    /// would otherwise never be written and the purchase downloaded again. The
    /// sidecar is written (temp file, then rename) before the job is queued.
    pub(super) fn enqueue_conversion(&self, job: Job) {
        // A file already queued keeps its job, and so its sidecar: the first
        // request's writer and facts are the ones a restart must see.
        if !self.conversions.try_reserve(&job.path) {
            return;
        }
        if let Err(e) = self.save_job_context(&job) {
            tracing::warn!(file = %job.path.display(), error = %e, "Could not keep the context of a conversion job; a restart would lose it");
        }
        self.conversions.enqueue(job);
    }

    fn save_job_context(&self, job: &Job) -> Result<(), String> {
        let context = JobContext {
            path: job.path.clone(),
            writer: job.writer.clone(),
            facts: job.facts.clone(),
        };
        std::fs::create_dir_all(self.jobs_dir()).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(&context).map_err(|e| e.to_string())?;
        private_file::write_file_atomic(&self.context_file(&job.path), &bytes, 0o600)
            .map_err(|e| e.to_string())
    }

    fn remove_job_context(&self, path: &Path) {
        match std::fs::remove_file(self.context_file(path)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "Could not remove the context of a conversion job")
            }
        }
    }

    /// The jobs the sidecars describe. A sidecar that cannot be read, or whose file
    /// is no longer in the inbox, is removed.
    fn load_job_contexts(&self) -> Vec<Job> {
        let Ok(entries) = std::fs::read_dir(self.jobs_dir()) else {
            return Vec::new();
        };
        let mut jobs = Vec::new();
        for entry in entries.filter_map(|e| e.ok()) {
            let file = entry.path();
            if file.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let context = std::fs::read(&file)
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice::<JobContext>(&b).map_err(|e| e.to_string()));
            match context {
                Ok(c) if c.path.exists() => jobs.push(Job::new(c.path, c.writer, c.facts)),
                Ok(c) => {
                    tracing::info!(file = %c.path.display(), "Removing the context of a conversion job whose file is gone");
                    let _ = std::fs::remove_file(&file);
                }
                Err(e) => {
                    tracing::warn!(file = %file.display(), error = %e, "Removing an unreadable conversion job context");
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
        jobs
    }

    /// Remove `*.encoding.flac`, `*.rebuilt.aiff` and `<hex>.flac` from staging.
    fn sweep_staging(&self) {
        let staging = self.staging_dir();
        let Ok(entries) = std::fs::read_dir(&staging) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let is_leftover = name.ends_with(".encoding.flac")
                || name.ends_with(".rebuilt.aiff")
                || name.strip_suffix(".flac").is_some_and(|stem| {
                    stem.len() == HEX_LEN && stem.bytes().all(|b| b.is_ascii_hexdigit())
                });
            if !is_leftover {
                continue;
            }
            match std::fs::remove_file(entry.path()) {
                Ok(()) => tracing::info!(file = name, "Removed a leftover of a conversion"),
                Err(e) => {
                    tracing::warn!(file = name, error = %e, "Could not remove a leftover of a conversion")
                }
            }
        }
    }

    /// Start the worker thread. It holds the service weakly, so it never keeps it
    /// alive; it ends when the queue is shut down or the service is gone.
    pub(super) fn start_conversion_worker(self: &Arc<Self>) {
        let conversions = Arc::clone(&self.conversions);
        let service = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("aiff-conversion".into())
            .spawn(move || {
                lower_current_thread_priority();
                while let Some(job) = conversions.next_job() {
                    let Some(service) = service.upgrade() else {
                        break;
                    };
                    service.run_conversion_job(job);
                }
            });
        match spawned {
            Ok(handle) => *self.conversions.worker.lock().unwrap() = Some(handle),
            Err(e) => tracing::warn!(error = %e, "The conversion worker did not start"),
        }
    }

    fn run_conversion_job(&self, job: Job) {
        let ended =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.convert_job(&job)))
                .unwrap_or_else(|_| JobEnd::Failed("the conversion worker panicked".into()));
        let path = job.path.display().to_string();
        match ended {
            JobEnd::Stored { hash, warnings } => {
                tracing::info!(file = path, hash = %hash.as_str(), ?warnings, "Conversion job done");
                self.remove_job_context(&job.path);
                self.conversions.finish();
            }
            JobEnd::SourceGone => {
                tracing::warn!(file = path, "Conversion job dropped: the file is gone");
                self.remove_job_context(&job.path);
                self.conversions.finish();
            }
            JobEnd::Retry(error) => {
                tracing::warn!(file = path, %error, "Conversion failed; it will be tried again");
                self.conversions.retry_later(job, error);
            }
            JobEnd::Failed(error) => {
                tracing::error!(file = path, %error, "Conversion failed");
                self.conversions.give_up(job, error);
            }
        }
    }

    /// Run one job to its end, on the calling thread, through its own ACID client
    /// (the request thread uses the service's).
    pub(super) fn convert_job(&self, job: &Job) -> JobEnd {
        let validated = match crate::pipeline::InboxFile::new(job.path.clone())
            .validate_accepting(|f| self.accepts_format(f))
        {
            Ok(v) => v,
            Err(IngestError::FileNotFound(_)) => return JobEnd::SourceGone,
            Err(e) => return end_for_error(e.into()),
        };
        let client = match AcidClient::connect(&self.acid_socket) {
            Ok(c) => c,
            Err(e) => return end_for_error(e.into()),
        };

        // Dedup before any work, as a new ingest does.
        if let Some(entity) = self.known_entity(&validated.content_hash) {
            let writer = job.writer.as_ref();
            let provenance = self.client_provenance(writer, &job.facts);
            // No self-heal from the worker (phase 6b); the entity may have moved to
            // a merge winner, which `ingest_duplicate` answers.
            let (entity, _gained_item_id) =
                self.ingest_duplicate(&client, validated, &entity, writer, provenance);
            return JobEnd::Stored {
                hash: entity,
                warnings: Vec::new(),
            };
        }

        match self
            .conversions
            .config
            .encoder
            .encode(&job.path, &self.staging_dir())
        {
            Ok(verified) => self.store_converted(&client, job, &validated, verified),
            Err(EncodeError::Refused(refusal)) => {
                tracing::warn!(file = %job.path.display(), %refusal, "Not converted; storing the AIFF as it is");
                match self.store_validated(&client, validated, job.writer.as_ref(), &job.facts) {
                    Ok((hash, mut warnings)) => {
                        warnings.push(IngestWarning::StoredAsAiff {
                            reason: refusal.to_string(),
                        });
                        JobEnd::Stored { hash, warnings }
                    }
                    Err(e) => end_for_error(e),
                }
            }
            Err(EncodeError::Environment(e)) => JobEnd::Retry(e.to_string()),
        }
    }

    /// A verified FLAC: the blob (rename), then its facts, then the source AIFF goes.
    fn store_converted(
        &self,
        client: &AcidClient,
        job: &Job,
        source: &ValidatedAudio,
        verified: Verified,
    ) -> JobEnd {
        // Whatever happens below, a verified FLAC that did not become a blob is not
        // left in staging (the guard finds nothing once it was renamed into `blobs/`).
        let _staged = StagedFlac(verified.flac_path.clone());

        if verified.source_content_hash != source.content_hash {
            // The file changed after it was validated: what was verified is not what
            // is in the inbox. Nothing is stored; the next try starts over.
            return JobEnd::Retry("the file changed while it was converted".into());
        }

        let scan = self.sources.scan.clone();
        let derived = self.sources.derived.clone();
        let content = job.writer.clone().unwrap_or_else(|| scan.clone());

        // File facts of the FLAC, tag facts of the AIFF (proved equal to the FLAC's
        // by the encoder), name facts of the AIFF's own file name.
        let mut facts = match crate::fact_generator::file_facts(
            &verified.flac_path,
            AudioFormat::Flac,
            &scan,
        ) {
            Ok(facts) => facts,
            Err(e) => return end_for_error(e.into()),
        };
        let states = |is: fn(&MusicValue) -> bool| verified.tag_facts.iter().any(is);
        let name_facts = crate::fact_generator::name_facts(
            &job.path,
            states(|v| matches!(v, MusicValue::Title(_))),
            states(|v| matches!(v, MusicValue::Artist(_))),
            states(|v| matches!(v, MusicValue::TrackNumber(_))),
            &content,
        );
        facts.extend(
            verified
                .tag_facts
                .iter()
                .map(|v| (v.clone(), content.clone())),
        );
        facts.extend(name_facts);

        let extracted = ExtractedTrack {
            path: verified.flac_path.clone(),
            format: AudioFormat::Flac,
            content_hash: verified.flac_content_hash.clone(),
            facts,
            warnings: Vec::new(),
        };
        let identity = vec![
            (
                MusicValue::PcmHash(verified.pcm_hash.clone()),
                derived.clone(),
            ),
            (
                MusicValue::EncodedFrom(verified.source_content_hash.clone()),
                derived,
            ),
        ];
        let provenance = self.client_provenance(job.writer.as_ref(), &job.facts);
        let hash = match self.store_extracted(client, extracted, identity, provenance) {
            Ok((hash, _)) => hash,
            Err(e) => return end_for_error(e),
        };

        self.remove_converted_source(&job.path, &verified.source_content_hash);
        JobEnd::Stored {
            hash,
            warnings: Vec::new(),
        }
    }

    /// Remove the source AIFF of a conversion that is stored, if it is still the
    /// file that was converted; one that changed since is kept.
    fn remove_converted_source(&self, path: &Path, converted: &ContentHash) {
        #[cfg(test)]
        if let Some(hook) = &self.conversions.config.before_source_removal {
            hook(path);
        }
        match crate::pipeline::compute_hash(path) {
            Ok(now) if now == *converted => {
                if let Err(e) = std::fs::remove_file(path) {
                    tracing::warn!(file = %path.display(), error = %e, "Could not remove a converted source");
                }
            }
            Ok(_) => tracing::warn!(
                file = %path.display(),
                "The source changed after its conversion was verified; keeping it"
            ),
            Err(IngestError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                file = %path.display(), error = %e,
                "Could not check the source after its conversion; keeping it"
            ),
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn conversions(max_attempts: u32) -> Conversions {
        Conversions::new(ConversionConfig::default().with_retry(RetryPolicy {
            max_attempts,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        }))
    }

    fn job(inbox: &Path, name: &str) -> Job {
        let path = inbox.join(name);
        std::fs::write(&path, b"x").unwrap();
        Job::new(path, None, Vec::new())
    }

    fn states(c: &Conversions, inbox: &Path) -> Vec<(String, ConversionState)> {
        c.status(inbox)
            .into_iter()
            .map(|i| (i.path.as_str().to_string(), i.state))
            .collect()
    }

    #[test]
    fn jobs_are_taken_in_order_and_the_status_follows() {
        let inbox = tempfile::tempdir().unwrap();
        let c = conversions(3);
        for name in ["a.aiff", "b.aiff", "c.aiff"] {
            c.enqueue(job(inbox.path(), name));
        }
        // Queued twice is queued once.
        c.enqueue(job(inbox.path(), "b.aiff"));
        assert_eq!(
            states(&c, inbox.path()),
            [
                ("a.aiff".to_string(), ConversionState::Queued),
                ("b.aiff".to_string(), ConversionState::Queued),
                ("c.aiff".to_string(), ConversionState::Queued),
            ]
        );

        let first = c.next_job().unwrap();
        assert_eq!(first.path, inbox.path().join("a.aiff"));
        assert_eq!(
            states(&c, inbox.path())[..2],
            [
                ("a.aiff".to_string(), ConversionState::Converting),
                ("b.aiff".to_string(), ConversionState::Queued),
            ]
        );
        // The one being converted is not queued again.
        c.enqueue(job(inbox.path(), "a.aiff"));
        assert_eq!(c.status(inbox.path()).len(), 3);

        c.finish();
        assert_eq!(c.next_job().unwrap().path, inbox.path().join("b.aiff"));
    }

    #[test]
    fn a_failing_job_retries_with_backoff_and_then_fails() {
        let inbox = tempfile::tempdir().unwrap();
        let c = conversions(3);
        c.enqueue(job(inbox.path(), "a.aiff"));

        let first = c.next_job().unwrap();
        c.retry_later(first, "no flac".into());
        assert_eq!(
            states(&c, inbox.path()),
            [(
                "a.aiff".to_string(),
                ConversionState::Retrying {
                    attempt: 1,
                    last_error: "no flac".into()
                }
            )]
        );

        // It comes back after its delay, and the second failure waits again.
        let second = c.next_job().unwrap();
        assert_eq!(second.attempt, 1);
        c.retry_later(second, "still no flac".into());
        let third = c.next_job().unwrap();
        assert_eq!(third.attempt, 2);
        // The third failure is the last of 3 tries.
        c.retry_later(third, "gave up".into());
        assert_eq!(
            states(&c, inbox.path()),
            [(
                "a.aiff".to_string(),
                ConversionState::Failed {
                    message: "gave up".into()
                }
            )]
        );

        // Ingested again, the file starts over.
        c.enqueue(job(inbox.path(), "a.aiff"));
        assert_eq!(
            states(&c, inbox.path()),
            [("a.aiff".to_string(), ConversionState::Queued)]
        );
    }

    #[test]
    fn a_failed_file_that_is_gone_is_not_listed() {
        let inbox = tempfile::tempdir().unwrap();
        let c = conversions(1);
        let j = job(inbox.path(), "a.aiff");
        let path = j.path.clone();
        c.enqueue(j);
        let taken = c.next_job().unwrap();
        c.give_up(taken, "broken".into());
        assert_eq!(c.status(inbox.path()).len(), 1);
        std::fs::remove_file(path).unwrap();
        assert_eq!(c.status(inbox.path()), vec![]);
    }

    #[test]
    fn the_retry_delay_doubles_up_to_its_cap() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(1), Duration::from_secs(30));
        assert_eq!(policy.delay(2), Duration::from_secs(60));
        assert_eq!(policy.delay(3), Duration::from_secs(120));
        assert_eq!(policy.delay(30), policy.max_delay);
    }

    #[test]
    fn a_waiting_worker_wakes_for_a_shutdown() {
        let c = Arc::new(conversions(3));
        let waiting = Arc::clone(&c);
        let handle = std::thread::spawn(move || waiting.next_job().is_none());
        std::thread::sleep(Duration::from_millis(50));
        c.request_shutdown();
        assert!(handle.join().unwrap());
    }
}
