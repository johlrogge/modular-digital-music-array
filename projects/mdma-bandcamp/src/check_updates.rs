//! The check-updates job: find out which collection items have more tracks
//! on Bandcamp than the library holds.
//!
//! The caller fetches the collection once and hands it in; the download-page
//! fetches are spaced and wait out rate limits, so a run of hundreds of items
//! stays polite. The Bandcamp page fetch and the library lookup are passed in
//! as closures, so the pacing and retry rules are tested without a network.

use crate::service::check_outcome;
use bandcamp_api::{BandcampError, CollectionItem, DigitalItem};
use source_protocol::{CheckJobStatus, CheckPhase, ItemCheckOutcome, ItemCheckResult};
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

/// Pause between two download-page fetches of one run.
pub(crate) const CHECK_ITEM_DELAY: Duration = Duration::from_secs(1);

/// How often a rate-limited request is retried before it is given up on.
pub(crate) const RATE_LIMIT_MAX_RETRIES: u32 = 3;

/// The longest a single rate-limit wait may take, whatever Bandcamp asks for.
pub(crate) const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(120);

/// Progress of a run, reported as it happens.
#[derive(Debug)]
pub(crate) enum CheckUpdate {
    Phase(CheckPhase),
    Item(ItemCheckResult),
}

impl CheckUpdate {
    /// Fold the update into the job status.
    pub(crate) fn apply(self, status: &mut CheckJobStatus) {
        match self {
            CheckUpdate::Phase(phase) => status.phase = phase,
            CheckUpdate::Item(result) => status.results.push(result),
        }
    }
}

/// Run `op`, and when Bandcamp rate limits it wait as asked (at most
/// [`RATE_LIMIT_MAX_WAIT`]) and run it again, up to [`RATE_LIMIT_MAX_RETRIES`]
/// times. The last error is returned once the retries are used up.
pub(crate) async fn with_rate_limit_retry<T, F, Fut>(mut op: F) -> Result<T, BandcampError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, BandcampError>>,
{
    let mut retries = 0;
    loop {
        match op().await {
            Err(BandcampError::RateLimited { retry_after_secs })
                if retries < RATE_LIMIT_MAX_RETRIES =>
            {
                retries += 1;
                let wait = Duration::from_secs(retry_after_secs).min(RATE_LIMIT_MAX_WAIT);
                tracing::warn!(
                    retry = retries,
                    wait_secs = wait.as_secs(),
                    "Bandcamp rate limited the request, waiting before retrying"
                );
                tokio::time::sleep(wait).await;
            }
            other => return other,
        }
    }
}

/// Describe a failed request for the report.
pub(crate) fn failure_reason(e: &BandcampError) -> String {
    match e {
        BandcampError::RateLimited { .. } => format!(
            "rate limited by Bandcamp, still refused after {RATE_LIMIT_MAX_RETRIES} retries"
        ),
        other => other.to_string(),
    }
}

/// The track counts found for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Checked {
    pub(crate) live: usize,
    pub(crate) stored: usize,
    /// Bandcamp has more tracks than the library.
    pub(crate) stale: bool,
}

/// Check one item of the collection against the library: the stored count
/// first (a library failure costs no Bandcamp request), then the download
/// page. Also says whether a page was requested, for pacing.
pub(crate) async fn check_item<Fd, FdFut, Fs, FsFut>(
    item: &CollectionItem,
    fetch_details: &Fd,
    stored_count: &Fs,
) -> (Result<Checked, String>, bool)
where
    Fd: Fn(String) -> FdFut,
    FdFut: Future<Output = Result<DigitalItem, BandcampError>>,
    Fs: Fn(String) -> FsFut,
    FsFut: Future<Output = Result<usize, String>>,
{
    let id = item.id.as_str();
    let stored = match stored_count(id.to_string()).await {
        Ok(count) => count,
        Err(reason) => return (Err(format!("library unavailable: {reason}")), false),
    };

    let details = match with_rate_limit_retry(|| fetch_details(item.download_url.clone())).await {
        Ok(details) => details,
        Err(e) => {
            tracing::error!(error = %e, item_id = %id, "Check: failed to fetch item details");
            return (Err(failure_reason(&e)), true);
        }
    };

    let checked = check_outcome(details.live_track_count, stored).map(|(live, stale)| Checked {
        live,
        stored,
        stale,
    });
    (checked, true)
}

/// The wire outcome for a checked item.
fn item_outcome(checked: Result<Checked, String>) -> ItemCheckOutcome {
    match checked {
        Ok(Checked {
            live,
            stored,
            stale: true,
        }) => ItemCheckOutcome::Stale {
            live_track_count: live,
            stored_track_count: stored,
        },
        Ok(Checked { stored, .. }) => ItemCheckOutcome::UpToDate {
            track_count: stored,
        },
        Err(reason) => ItemCheckOutcome::Failed { reason },
    }
}

/// Check `item_ids` against the fetched `collection`, reporting progress
/// through `report`. Every id gets a result, in order; the run ends with the
/// phase Done. Fetches of download pages are spaced by [`CHECK_ITEM_DELAY`].
pub(crate) async fn run_check_updates<Fd, FdFut, Fs, FsFut>(
    collection: &[CollectionItem],
    item_ids: &[String],
    fetch_details: Fd,
    stored_count: Fs,
    mut report: impl FnMut(CheckUpdate) + Send,
) where
    Fd: Fn(String) -> FdFut,
    FdFut: Future<Output = Result<DigitalItem, BandcampError>>,
    Fs: Fn(String) -> FsFut,
    FsFut: Future<Output = Result<usize, String>>,
{
    let by_id: HashMap<&str, &CollectionItem> =
        collection.iter().map(|i| (i.id.as_str(), i)).collect();

    report(CheckUpdate::Phase(CheckPhase::Checking));
    for (n, id) in item_ids.iter().enumerate() {
        let (outcome, page_requested) = match by_id.get(id.as_str()) {
            None => (ItemCheckOutcome::NotInCollection, false),
            Some(item) => {
                let (checked, requested) = check_item(item, &fetch_details, &stored_count).await;
                (item_outcome(checked), requested)
            }
        };
        report(CheckUpdate::Item(ItemCheckResult {
            identifier: id.clone(),
            outcome,
        }));
        if page_requested && n + 1 < item_ids.len() {
            tokio::time::sleep(CHECK_ITEM_DELAY).await;
        }
    }
    report(CheckUpdate::Phase(CheckPhase::Done));
}

#[cfg(test)]
mod tests {
    use super::*;
    use bandcamp_api::{ItemId, ItemType};
    use music_facts::{Artist, Title};
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::Instant;

    /// What a download page fetch answers.
    #[derive(Clone)]
    enum Reply {
        Count(Option<usize>),
        Limited(u64),
    }

    /// Stands in for Bandcamp and the library, counting calls.
    #[derive(Default)]
    struct Mock {
        collection: Vec<CollectionItem>,
        collection_calls: AtomicUsize,
        /// How many collection fetches are rate limited before one succeeds.
        collection_limited: AtomicUsize,
        /// Per download url: replies in order; the last one repeats.
        replies: Mutex<HashMap<String, Vec<Reply>>>,
        details_calls: Mutex<HashMap<String, usize>>,
        stored: HashMap<String, usize>,
    }

    fn url_of(id: &str) -> String {
        format!("https://bandcamp.com/download/{id}")
    }

    fn item(id: &str) -> CollectionItem {
        CollectionItem {
            id: ItemId::new(id),
            artist: Artist::new("Artist"),
            title: Title::new("Album"),
            item_type: ItemType::Album,
            purchased: None,
            download_url: url_of(id),
        }
    }

    impl Mock {
        /// A collection of `ids`, each stored with 3 tracks and a page that
        /// says 3 live tracks.
        fn with_items(ids: &[&str]) -> Self {
            let mock = Self {
                collection: ids.iter().map(|id| item(id)).collect(),
                stored: ids.iter().map(|id| (id.to_string(), 3)).collect(),
                ..Self::default()
            };
            for id in ids {
                mock.reply(id, vec![Reply::Count(Some(3))]);
            }
            mock
        }

        fn reply(&self, id: &str, replies: Vec<Reply>) {
            self.replies.lock().insert(url_of(id), replies);
        }

        fn calls_for(&self, id: &str) -> usize {
            self.details_calls
                .lock()
                .get(&url_of(id))
                .copied()
                .unwrap_or(0)
        }

        async fn fetch_collection(&self) -> Result<Vec<CollectionItem>, BandcampError> {
            self.collection_calls.fetch_add(1, Ordering::SeqCst);
            let limited = self.collection_limited.load(Ordering::SeqCst);
            if limited > 0 {
                self.collection_limited.store(limited - 1, Ordering::SeqCst);
                return Err(BandcampError::RateLimited {
                    retry_after_secs: 10,
                });
            }
            Ok(self.collection.clone())
        }

        async fn details(&self, download_url: String) -> Result<DigitalItem, BandcampError> {
            let call = {
                let mut calls = self.details_calls.lock();
                let n = calls.entry(download_url.clone()).or_insert(0);
                *n += 1;
                *n
            };
            let reply = {
                let replies = self.replies.lock();
                let list = &replies[&download_url];
                list.get(call - 1)
                    .unwrap_or_else(|| list.last().expect("a reply"))
                    .clone()
            };
            match reply {
                Reply::Limited(secs) => Err(BandcampError::RateLimited {
                    retry_after_secs: secs,
                }),
                Reply::Count(live_track_count) => Ok(DigitalItem {
                    artist: Artist::new("Artist"),
                    title: Title::new("Album"),
                    item_type: ItemType::Album,
                    release_year: None,
                    formats: HashMap::new(),
                    live_track_count,
                }),
            }
        }

        async fn stored_count(&self, item_id: String) -> Result<usize, String> {
            self.stored
                .get(&item_id)
                .copied()
                .ok_or_else(|| "not stored".to_string())
        }
    }

    /// Run the whole check as the service does: one collection fetch (with
    /// retry), then the items. Returns the results, the phases, and the
    /// virtual time the run took.
    async fn run(mock: &Mock, ids: &[&str]) -> (Vec<ItemCheckResult>, Vec<CheckPhase>, Duration) {
        let started = Instant::now();
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        let mut results = Vec::new();
        let mut phases = Vec::new();
        match with_rate_limit_retry(|| mock.fetch_collection()).await {
            Err(e) => phases.push(CheckPhase::Failed {
                message: format!("fetching the collection failed: {}", failure_reason(&e)),
            }),
            Ok(collection) => {
                run_check_updates(
                    &collection,
                    &ids,
                    |url| mock.details(url),
                    |id| mock.stored_count(id),
                    |update| match update {
                        CheckUpdate::Item(r) => results.push(r),
                        CheckUpdate::Phase(p) => phases.push(p),
                    },
                )
                .await
            }
        }
        (results, phases, started.elapsed())
    }

    fn outcomes(results: &[ItemCheckResult]) -> Vec<&ItemCheckOutcome> {
        results.iter().map(|r| &r.outcome).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn many_items_cost_one_collection_fetch() {
        let ids = ["p1", "p2", "p3", "p4", "p5"];
        let mock = Mock::with_items(&ids);

        let (results, phases, _) = run(&mock, &ids).await;

        assert_eq!(mock.collection_calls.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 5);
        assert_eq!(
            results
                .iter()
                .map(|r| r.identifier.as_str())
                .collect::<Vec<_>>(),
            ids
        );
        assert!(results
            .iter()
            .all(|r| r.outcome == ItemCheckOutcome::UpToDate { track_count: 3 }));
        assert_eq!(phases, vec![CheckPhase::Checking, CheckPhase::Done]);
    }

    #[tokio::test(start_paused = true)]
    async fn download_pages_are_spaced_by_the_delay_constant() {
        let mock = Mock::with_items(&["p1", "p2", "p3"]);

        let (_, _, elapsed) = run(&mock, &["p1", "p2", "p3"]).await;

        assert_eq!(
            elapsed,
            CHECK_ITEM_DELAY * 2,
            "between pages, not after the last"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_then_ok_is_retried_after_the_asked_wait() {
        let mock = Mock::with_items(&["p1"]);
        mock.reply("p1", vec![Reply::Limited(30), Reply::Count(Some(3))]);

        let (results, _, elapsed) = run(&mock, &["p1"]).await;

        assert_eq!(
            outcomes(&results),
            vec![&ItemCheckOutcome::UpToDate { track_count: 3 }]
        );
        assert_eq!(mock.calls_for("p1"), 2);
        assert_eq!(elapsed, Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn persistent_rate_limit_ends_as_a_clear_item_error_and_the_run_goes_on() {
        let mock = Mock::with_items(&["p1", "p2"]);
        mock.reply("p1", vec![Reply::Limited(60)]);

        let (results, phases, elapsed) = run(&mock, &["p1", "p2"]).await;

        assert_eq!(mock.calls_for("p1"), 1 + RATE_LIMIT_MAX_RETRIES as usize);
        match &results[0].outcome {
            ItemCheckOutcome::Failed { reason } => {
                assert!(reason.contains("rate limited"), "reason: {reason}");
                assert!(reason.contains("3 retries"), "reason: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            results[1].outcome,
            ItemCheckOutcome::UpToDate { track_count: 3 }
        );
        assert_eq!(phases.last(), Some(&CheckPhase::Done));
        assert_eq!(
            elapsed,
            Duration::from_secs(60) * RATE_LIMIT_MAX_RETRIES + CHECK_ITEM_DELAY
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limit_wait_is_bounded() {
        let mock = Mock::with_items(&["p1"]);
        mock.reply("p1", vec![Reply::Limited(86_400), Reply::Count(Some(3))]);

        let (_, _, elapsed) = run(&mock, &["p1"]).await;

        assert_eq!(elapsed, RATE_LIMIT_MAX_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limited_collection_fetch_is_retried_too() {
        let mock = Mock::with_items(&["p1"]);
        mock.collection_limited.store(1, Ordering::SeqCst);

        let (results, phases, elapsed) = run(&mock, &["p1"]).await;

        assert_eq!(mock.collection_calls.load(Ordering::SeqCst), 2);
        assert_eq!(results.len(), 1);
        assert_eq!(phases.last(), Some(&CheckPhase::Done));
        assert_eq!(elapsed, Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn a_collection_that_stays_rate_limited_fails_the_run() {
        let mock = Mock::with_items(&["p1"]);
        mock.collection_limited.store(usize::MAX, Ordering::SeqCst);

        let (results, phases, _) = run(&mock, &["p1"]).await;

        assert!(results.is_empty());
        match phases.last() {
            Some(CheckPhase::Failed { message }) => {
                assert!(message.contains("rate limited"), "message: {message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stale_missing_and_unknown_items_are_told_apart() {
        let mock = Mock::with_items(&["stale", "unknown"]);
        mock.reply("stale", vec![Reply::Count(Some(7))]);
        mock.reply("unknown", vec![Reply::Count(None)]);

        let (results, _, _) = run(&mock, &["stale", "gone", "unknown"]).await;

        assert_eq!(
            results[0].outcome,
            ItemCheckOutcome::Stale {
                live_track_count: 7,
                stored_track_count: 3
            }
        );
        assert_eq!(results[1].outcome, ItemCheckOutcome::NotInCollection);
        match &results[2].outcome {
            ItemCheckOutcome::Failed { reason } => {
                assert!(reason.contains("unknown"), "reason: {reason}")
            }
            other => panic!("an unknown live count must not be OK: {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_library_failure_is_an_item_error_without_a_bandcamp_request_or_delay() {
        let mut mock = Mock::with_items(&["p1", "p2"]);
        mock.stored.remove("p1");

        let (results, _, elapsed) = run(&mock, &["p1", "p2"]).await;

        match &results[0].outcome {
            ItemCheckOutcome::Failed { reason } => {
                assert!(reason.contains("library unavailable"), "reason: {reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(mock.calls_for("p1"), 0);
        assert_eq!(elapsed, Duration::ZERO, "no page fetched, nothing to space");
    }
}
