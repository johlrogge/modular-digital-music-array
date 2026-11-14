//! Bandcamp download service
//!
//! Handles collection syncing and download management.
//! Uses async throughout with NNG IPC bridged via channels.

use crate::cache::DownloadCache;
use crate::ipc::{
    parse_request, DownloadId, DownloadState, DownloadStatus, IpcServer, SourceError,
    SourcePlaylist, SourcePlaylistSummary, SourcePlaylistTrack, SourceRequest, SourceResponse,
    SourceStatus,
};
use bandcamp_api::{AudioFormat, BandcampClient, CollectionItem, CookieJar};
use library_ipc_client::PlaylistName;
use library_ipc_client::{
    AttributeList, FactSource, InboxPath, LibraryClient, MusicValue, TrackInfo, TrackQuery,
};
use library_search::StringQuery;
use parking_lot::Mutex;
use playlist_import::{
    default_import_name, has_owned_missing, match_source_playlist, matched_hashes,
    sanitize_playlist_name, to_import_reports, write_playlist, LibraryTrackLookup, TrackOutcome,
};
use source_protocol::{
    ImportJobStatus, ImportPhase, ImportResult, ImportSyncProgress, ImportWritten, SyncWaitEnd,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use track_matcher::normalize;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("IPC error: {0}")]
    Ipc(#[from] crate::ipc::IpcError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Bandcamp API error: {0}")]
    Bandcamp(#[from] bandcamp_api::BandcampError),

    #[error("Cache error: {0}")]
    Cache(#[from] crate::cache::CacheError),

    #[error("Channel error")]
    Channel,
}

/// Internal download state — maps to source_protocol::DownloadState for wire format.
#[derive(Clone, PartialEq, Eq)]
enum InternalDownloadState {
    Queued,
    Downloading,
    Extracting,
    Moving,
    Completed,
    Failed,
    Cancelled,
}

impl InternalDownloadState {
    /// Whether the download has ended (completed, failed or cancelled).
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    fn to_protocol(&self, error: &Option<String>) -> DownloadState {
        match self {
            InternalDownloadState::Queued => DownloadState::Queued,
            InternalDownloadState::Downloading => DownloadState::Downloading,
            InternalDownloadState::Extracting | InternalDownloadState::Moving => {
                DownloadState::Processing
            }
            InternalDownloadState::Completed => DownloadState::Completed,
            InternalDownloadState::Failed => DownloadState::Failed {
                message: error.clone().unwrap_or_default(),
            },
            InternalDownloadState::Cancelled => DownloadState::Cancelled,
        }
    }
}

/// Tool of the source this service states for everything it writes to the library.
const BANDCAMP_TOOL: &str = "mdma-bandcamp";

/// Origin kind of what this service gets from Bandcamp. With the tool it is the source
/// identity shared by an ingest, an ItemId backfill and a resync retract, so a resync
/// retracts exactly what this service wrote.
const BANDCAMP_ORIGIN: &str = "Bandcamp";

/// The source this service states on its ingests, backfills and retracts.
fn bandcamp_source() -> FactSource {
    FactSource::new(BANDCAMP_TOOL, env!("CARGO_PKG_VERSION"), BANDCAMP_ORIGIN)
}

/// The provenance facts this service states about a file it ingests for `item_id`.
fn bandcamp_provenance(item_id: &str) -> Vec<MusicValue> {
    vec![
        MusicValue::Source("bandcamp".to_string()),
        MusicValue::ItemId(item_id.to_string()),
    ]
}

/// The attributes a resync retracts before it re-downloads: what a Bandcamp ingest
/// writes from the download's tags. ItemId and Source identify the item across
/// resyncs and stay.
fn resync_attributes() -> AttributeList {
    AttributeList::try_new(["Album", "Title", "Artist", "TrackNumber", "Year"])
        .expect("the resync attributes are known attribute names")
}

/// Message returned to clients while no usable cookie file exists.
const NOT_CONFIGURED_MESSAGE: &str = "Bandcamp not configured: upload cookies via the web console.";

/// Whether a usable cookie file is present.
///
/// The cookie file is re-checked on demand, so uploading cookies (which also
/// restarts this service) never requires polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieState {
    Configured,
    NotConfigured { reason: String },
}

impl From<Result<CookieJar, bandcamp_api::BandcampError>> for CookieState {
    fn from(loaded: Result<CookieJar, bandcamp_api::BandcampError>) -> Self {
        match loaded {
            Ok(_) => Self::Configured,
            Err(e) => Self::NotConfigured {
                reason: e.to_string(),
            },
        }
    }
}

fn not_configured_error() -> SourceError {
    SourceError::NotAuthenticated {
        message: NOT_CONFIGURED_MESSAGE.to_string(),
    }
}

/// Download queue entry
#[derive(Clone)]
struct QueuedDownload {
    item: CollectionItem,
    state: InternalDownloadState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
}

/// Request with response channel for async processing
struct IpcMessage {
    request: SourceRequest,
    response_tx: oneshot::Sender<SourceResponse>,
}

/// Bandcamp download service
pub struct BandcampService {
    /// Cookie file path
    cookie_path: PathBuf,
    /// Downloads staging directory
    downloads_dir: PathBuf,
    /// Inbox directory (where completed downloads go)
    inbox_dir: PathBuf,
    /// Download cache
    cache: Mutex<DownloadCache>,
    /// Current username
    current_username: Mutex<Option<String>>,
    /// Download queue
    download_queue: Mutex<VecDeque<QueuedDownload>>,
    /// Active downloads
    active_downloads: Mutex<HashMap<String, QueuedDownload>>,
    /// Completed downloads count
    downloads_completed: AtomicUsize,
    /// Failed downloads count
    downloads_failed: AtomicUsize,
    /// Service start time
    start_time: Instant,
    /// Whether downloads are paused
    paused: AtomicBool,
    /// Audio format to download
    format: AudioFormat,
    /// Library service socket address for auto-ingest
    library_socket: String,
    /// Bandcamp username (read from cookies/config)
    username: Option<String>,
    /// Current or last playlist import job
    import_status: Mutex<Option<ImportJobStatus>>,
    /// Last issued import job id
    last_import_job_id: AtomicU64,
}

/// How often an import job polls the downloads it is waiting for.
const IMPORT_DOWNLOAD_POLL: Duration = Duration::from_secs(2);
/// How long an import job waits for downloads before carrying on.
const IMPORT_DOWNLOAD_LIMIT: Duration = Duration::from_secs(30 * 60);

/// What a collection sync queued.
#[derive(Debug)]
struct SyncOutcome {
    total_items: usize,
    /// Ids of the items that are missing from the library and now queued or
    /// downloading: freshly queued by this sync, plus ones already in flight
    /// from an earlier sync. Anything waiting for downloads waits on these.
    queued_ids: Vec<String>,
    /// How many of `queued_ids` this sync queued itself.
    newly_queued: usize,
}

/// Why a collection sync queued nothing.
#[derive(Debug)]
enum SyncError {
    /// The library did not answer: without it the whole collection would
    /// look new, so the sync is deferred (ADR-013).
    LibraryUnavailable(String),
    Source(SourceError),
}

impl From<SourceError> for SyncError {
    fn from(e: SourceError) -> Self {
        SyncError::Source(e)
    }
}

impl SyncError {
    /// The error as reported to the requester.
    fn into_source(self) -> SourceError {
        match self {
            SyncError::LibraryUnavailable(reason) => SourceError::SyncFailed {
                message: format!("library unavailable, sync deferred: {reason}"),
            },
            SyncError::Source(e) => e,
        }
    }
}

/// Whether the job is still doing work (not Done or Failed).
fn import_phase_is_running(phase: &ImportPhase) -> bool {
    !matches!(phase, ImportPhase::Done(_) | ImportPhase::Failed { .. })
}

/// Describe why a spawned import task did not return.
fn join_error_message(e: &tokio::task::JoinError) -> String {
    if e.is_panic() {
        format!("import job panicked: {e}")
    } else {
        format!("import job cancelled: {e}")
    }
}

/// Sanitise and validate a target playlist name.
fn validated_playlist_name(raw: &str) -> Result<(String, PlaylistName), String> {
    let sanitized = sanitize_playlist_name(raw);
    PlaylistName::new(&sanitized)
        .map(|name| (sanitized, name))
        .map_err(|e| format!("invalid playlist name {raw:?}: {e}"))
}

/// Count queued downloads of `ids` that finished: `(completed, failed)`.
///
/// An id absent from `active` is still pending: it may be waiting in the
/// queue, or between `pop_front` and the insert into `active`. Cancelled
/// counts as failed, since it will never complete. Cancelling a queued item
/// records a Cancelled entry in `active`, so such a wait ends too.
fn tally_downloads(active: &HashMap<String, QueuedDownload>, ids: &[String]) -> (u32, u32) {
    let (mut completed, mut failed) = (0, 0);
    for id in ids {
        match active.get(id).map(|dl| &dl.state) {
            Some(InternalDownloadState::Completed) => completed += 1,
            Some(InternalDownloadState::Failed | InternalDownloadState::Cancelled) => failed += 1,
            _ => {}
        }
    }
    (completed, failed)
}

/// Pure helper: decide whether a collection item should be skipped based on library search results.
///
/// Returns `true` IFF at least one `TrackInfo` in `search_results` has a normalized-equal
/// artist AND a normalized-equal album to the collection item. The probe uses `Contains` to
/// narrow the candidate set, but this predicate enforces exact normalized equality at the
/// decision boundary to prevent substring-only false positives.
pub(crate) fn should_skip_by_work_identity(
    item: &CollectionItem,
    search_results: &[TrackInfo],
) -> bool {
    let item_artist = normalize(item.artist.as_str());
    let item_album = normalize(item.title.as_str());
    search_results.iter().any(|track| {
        let track_artist = normalize(track.artist.as_deref().unwrap_or(""));
        let track_album = normalize(track.album.as_deref().unwrap_or(""));
        track_artist == item_artist && track_album == item_album
    })
}

/// Drop the collection items the library already has, by ItemId or by work
/// identity (artist + album; the new ItemId is then backfilled onto the
/// matching track so later syncs short-circuit).
///
/// Makes blocking library calls for every item: run it in `spawn_blocking`
/// and never with a service lock held. When the library is unreachable, or
/// cannot answer the ItemId query, the sync is deferred (an error): without
/// the library the whole collection would look new (ADR-013).
fn drop_items_known_to_library(
    library_socket: &str,
    collection: Vec<CollectionItem>,
) -> Result<Vec<CollectionItem>, SyncError> {
    let deferred = |e: &dyn std::fmt::Display| {
        tracing::warn!(error = %e, socket = %library_socket, "Library unavailable, sync deferred");
        SyncError::LibraryUnavailable(e.to_string())
    };
    let lib_client = LibraryClient::connect(library_socket).map_err(|e| deferred(&e))?;

    // Primary dedup: ItemIds the library already knows.
    let all_item_ids: Vec<String> = collection
        .iter()
        .map(|i| i.id.as_str().to_string())
        .collect();
    let library_known: HashSet<String> = lib_client
        .has_facts("ItemId", all_item_ids)
        .map_err(|e| deferred(&e))?
        .into_iter()
        .collect();
    tracing::info!(
        count = library_known.len(),
        "Library reports items already ingested"
    );

    let mut unknown = Vec::new();
    for item in collection {
        let item_id_str = item.id.as_str().to_string();

        if library_known.contains(&item_id_str) {
            continue;
        }

        // Not found by ItemId — try work-identity dedup (artist + album).
        // This catches duplicate purchases (e.g. vinyl + digital) that share the
        // same work but carry different ItemIds.
        let query = TrackQuery {
            artist: Some(StringQuery::Contains(normalize(item.artist.as_str()))),
            album: Some(StringQuery::Contains(normalize(item.title.as_str()))),
            ..Default::default()
        };
        match lib_client.search(&query) {
            Ok(results) if should_skip_by_work_identity(&item, &results) => {
                let matched = results.first().expect("non-empty checked above");
                let matched_hash = matched.content_hash.clone();
                tracing::info!(
                    artist = %item.artist,
                    album = %item.title,
                    live_item_id = %item_id_str,
                    matched_hash = %matched_hash.as_str(),
                    "Work already in library under different ItemId — skipping download"
                );

                // Backfill: record the new ItemId so future syncs short-circuit via
                // the cheap has_facts("ItemId", ...) path. The id came from Bandcamp,
                // so say so, as the same source an ingest of that item states.
                if let Err(e) = lib_client.write_fact_as(
                    &matched_hash,
                    MusicValue::ItemId(item_id_str.clone()),
                    &bandcamp_source(),
                ) {
                    tracing::warn!(
                        error = %e,
                        item_id = %item_id_str,
                        "Failed to backfill ItemId fact — skip still applies"
                    );
                }
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    item_id = %item_id_str,
                    "Work-identity search failed — falling through to download"
                );
            }
        }
        unknown.push(item);
    }
    Ok(unknown)
}

/// Pure helper: determine whether a bandcamp item is stale based on track counts.
///
/// `stale` means `live_count > stored_count` — bandcamp has more tracks than we have.
/// Same count is fine. Fewer live tracks than stored is unusual (bandcamp removed tracks)
/// and is NOT flagged as stale here — the caller may wish to log a warning separately.
fn classify_check_result(live_count: usize, stored_count: usize) -> bool {
    live_count > stored_count
}

/// Pure helper: turn the live count (if Bandcamp told us) and the stored
/// count into `(live, stale)`. An unknown live count is an error — it must
/// never be reported as OK or as zero.
fn check_outcome(live: Option<usize>, stored: usize) -> Result<(usize, bool), String> {
    let live = live.ok_or_else(|| {
        "live track count unknown: the download page gives no item type or track count".to_string()
    })?;
    Ok((live, classify_check_result(live, stored)))
}

/// Pure mapping: bandcamp_api playlist summary -> wire type.
fn to_source_playlist_summary(summary: bandcamp_api::PlaylistSummary) -> SourcePlaylistSummary {
    SourcePlaylistSummary {
        id: summary.id.to_string(),
        title: summary.title,
        url: summary.url,
        slug: summary.slug,
        track_count: summary.track_count,
        is_private: summary.is_private,
    }
}

/// Pure mapping: bandcamp_api playlist -> wire type (positions preserved).
fn to_source_playlist(playlist: bandcamp_api::Playlist) -> SourcePlaylist {
    SourcePlaylist {
        id: playlist.id.to_string(),
        title: playlist.title,
        url: playlist.url,
        total_count: playlist.total_count,
        tracks: playlist
            .tracks
            .into_iter()
            .map(|t| SourcePlaylistTrack {
                position: t.position,
                source_track_id: t.track_id.to_string(),
                title: t.title,
                artist: t.artist,
                album: t.album_title,
                duration_secs: t.duration_secs,
                owned: t.is_owned,
            })
            .collect(),
    }
}

/// Pure error mapping for playlist fetches: HTTP 404 on a named playlist means
/// it does not exist; everything else is a generic fetch failure. `playlist` is
/// `None` when listing (nothing specific can be "not found").
fn playlist_error_for_status(
    status: Option<u16>,
    message: &str,
    playlist: Option<&str>,
) -> SourceError {
    match (status, playlist) {
        (Some(404), Some(playlist)) => SourceError::ItemNotFound {
            identifier: playlist.to_string(),
        },
        _ => SourceError::SyncFailed {
            message: message.to_string(),
        },
    }
}

/// Map a Bandcamp error from a playlist request to a wire error.
fn playlist_error(e: &bandcamp_api::BandcampError, playlist: Option<&str>) -> SourceError {
    use bandcamp_api::BandcampError;
    match e {
        BandcampError::NotLoggedIn
        | BandcampError::Auth(_)
        | BandcampError::CookiesExpired { .. } => SourceError::NotAuthenticated {
            message: e.to_string(),
        },
        BandcampError::RateLimited { retry_after_secs } => SourceError::RateLimited {
            retry_after_secs: *retry_after_secs,
        },
        BandcampError::Http(inner) => {
            playlist_error_for_status(inner.status().map(|s| s.as_u16()), &e.to_string(), playlist)
        }
        _ => playlist_error_for_status(None, &e.to_string(), playlist),
    }
}

impl BandcampService {
    /// Create a new service
    pub fn new(
        cookie_path: PathBuf,
        downloads_dir: PathBuf,
        inbox_dir: PathBuf,
        cache_path: PathBuf,
        format: AudioFormat,
        library_socket: String,
        username: Option<String>,
    ) -> Result<Self, ServiceError> {
        // Load cache
        let cache = DownloadCache::open(&cache_path)?;

        // A missing or invalid cookie file is not fatal: stay up in the
        // NotConfigured state (warn once) so supervisors don't crash-loop us.
        if let CookieState::NotConfigured { reason } =
            bandcamp_api::load_cookies(&cookie_path).into()
        {
            tracing::warn!(
                path = %cookie_path.display(),
                %reason,
                "Bandcamp not configured: upload cookies via the web console (see bandcamp-setup.md). Running in not-configured state"
            );
        }

        Ok(Self {
            cookie_path,
            downloads_dir,
            inbox_dir,
            cache: Mutex::new(cache),
            current_username: Mutex::new(None),
            download_queue: Mutex::new(VecDeque::new()),
            active_downloads: Mutex::new(HashMap::new()),
            downloads_completed: AtomicUsize::new(0),
            downloads_failed: AtomicUsize::new(0),
            start_time: Instant::now(),
            paused: AtomicBool::new(false),
            format,
            library_socket,
            username,
            import_status: Mutex::new(None),
            last_import_job_id: AtomicU64::new(0),
        })
    }

    /// Current cookie state, re-read from disk.
    pub fn cookie_state(&self) -> CookieState {
        self.load_jar().into()
    }

    /// The single cookie-loading primitive: silent, re-reads the file each call.
    fn load_jar(&self) -> Result<CookieJar, bandcamp_api::BandcampError> {
        bandcamp_api::load_cookies(&self.cookie_path)
    }

    /// Build a Bandcamp client from the cookie file, or the not-configured error.
    fn client(&self) -> Result<BandcampClient, SourceError> {
        match self.load_jar() {
            Ok(jar) => {
                tracing::info!(path = %self.cookie_path.display(), "Loaded cookies");
                Ok(BandcampClient::new(jar))
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %self.cookie_path.display(), "Failed to load cookies");
                Err(not_configured_error())
            }
        }
    }

    /// The configured username, or a NotAuthenticated error explaining how to set it.
    fn require_username(&self) -> Result<String, SourceError> {
        self.username
            .clone()
            .ok_or_else(|| SourceError::NotAuthenticated {
                message: "No username configured. Set --username or MDMA_BANDCAMP_USERNAME."
                    .to_string(),
            })
    }

    /// Try to connect to the library service
    fn try_library_client(&self) -> Option<LibraryClient> {
        match LibraryClient::connect(&self.library_socket) {
            Ok(client) => Some(client),
            Err(e) => {
                tracing::warn!(error = %e, socket = %self.library_socket, "Failed to connect to library");
                None
            }
        }
    }

    /// Check that the library answers. The calls block, so they run on the
    /// blocking pool.
    async fn ping_library(&self) -> Result<(), String> {
        let socket = self.library_socket.clone();
        tokio::task::spawn_blocking(move || {
            LibraryClient::connect(&socket)
                .and_then(|client| client.ping())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("library check task failed: {e}"))?
    }

    /// The number of tracks the library stores for a Bandcamp item.
    async fn stored_track_count(&self, item_id: &str) -> Result<usize, String> {
        let socket = self.library_socket.clone();
        let item_id = item_id.to_string();
        tokio::task::spawn_blocking(move || {
            LibraryClient::connect(&socket)
                .and_then(|client| client.get_track_count_for_item_id(&item_id))
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("library query task failed: {e}"))?
    }

    /// Handle a request asynchronously
    pub async fn handle_request(self: &Arc<Self>, request: SourceRequest) -> SourceResponse {
        tracing::debug!(?request, "Handling request");

        match request {
            SourceRequest::Ping => SourceResponse::Pong,

            SourceRequest::GetStatus => {
                let status = SourceStatus {
                    name: "bandcamp".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    auth: match self.cookie_state() {
                        CookieState::Configured => source_protocol::AuthStatus::Authenticated,
                        CookieState::NotConfigured { .. } => {
                            source_protocol::AuthStatus::NotAuthenticated
                        }
                    },
                    downloads_active: self.active_downloads.lock().len(),
                    downloads_queued: self.download_queue.lock().len(),
                    downloads_completed: self.downloads_completed.load(Ordering::Relaxed),
                    downloads_failed: self.downloads_failed.load(Ordering::Relaxed),
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                    queue: if self.paused.load(Ordering::Relaxed) {
                        source_protocol::QueueState::Paused
                    } else {
                        source_protocol::QueueState::Active
                    },
                };
                SourceResponse::Status(status)
            }

            SourceRequest::Sync => self.handle_sync().await,

            SourceRequest::ListDownloads => {
                let downloads = self.list_downloads();
                SourceResponse::Downloads(downloads)
            }

            SourceRequest::CancelDownload { id } => {
                self.cancel_download(id.as_str());
                SourceResponse::Cancelled { id }
            }

            SourceRequest::PauseAll => {
                self.paused.store(true, Ordering::Relaxed);
                tracing::info!("Downloads paused");
                SourceResponse::Paused
            }

            SourceRequest::ResumeAll => {
                self.paused.store(false, Ordering::Relaxed);
                tracing::info!("Downloads resumed");
                SourceResponse::Resumed
            }

            SourceRequest::ResyncItem { identifier } => self.handle_resync(identifier).await,

            SourceRequest::CheckItem { identifier } => self.handle_check_item(identifier).await,

            SourceRequest::ListPlaylists => self.handle_list_playlists().await,

            SourceRequest::GetPlaylist { playlist } => self.handle_get_playlist(playlist).await,

            SourceRequest::ImportPlaylist {
                playlist,
                name,
                dry_run,
            } => self.handle_import_playlist(playlist, name, dry_run),

            SourceRequest::GetImportStatus => {
                SourceResponse::ImportStatus(self.import_status.lock().clone())
            }
        }
    }

    /// List the configured user's playlists.
    async fn handle_list_playlists(&self) -> SourceResponse {
        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return SourceResponse::Error(e),
        };
        let username = match self.require_username() {
            Ok(u) => u,
            Err(e) => return SourceResponse::Error(e),
        };

        match client.list_playlists(&username).await {
            Ok(playlists) => SourceResponse::Playlists(
                playlists
                    .into_iter()
                    .map(to_source_playlist_summary)
                    .collect(),
            ),
            Err(e) => {
                tracing::error!(error = %e, "Failed to list playlists");
                SourceResponse::Error(playlist_error(&e, None))
            }
        }
    }

    /// Fetch one playlist by slug or URL.
    async fn handle_get_playlist(&self, playlist: String) -> SourceResponse {
        match self.fetch_playlist(&playlist).await {
            Ok(p) => SourceResponse::Playlist(p),
            Err(e) => SourceResponse::Error(e),
        }
    }

    /// Fetch one playlist by slug or URL from Bandcamp.
    async fn fetch_playlist(&self, playlist: &str) -> Result<SourcePlaylist, SourceError> {
        let client = self.client()?;
        let username = self.require_username()?;

        let url = BandcampClient::playlist_url(&username, playlist).map_err(|e| {
            tracing::warn!(error = %e, %playlist, "Rejected playlist argument");
            playlist_error(&e, Some(playlist))
        })?;
        match client.get_playlist(&url).await {
            Ok(p) => Ok(to_source_playlist(p)),
            Err(e) => {
                tracing::error!(error = %e, %url, "Failed to fetch playlist");
                Err(playlist_error(&e, Some(playlist)))
            }
        }
    }

    /// Force-queue an item, bypassing library and cache dedup.
    ///
    /// Retracts existing library facts, evicts the cache entry (best-effort),
    /// then queues the item by the common rule (`enqueue_items`). If the
    /// library is unreachable or the retract fails nothing is evicted or
    /// queued and an error is returned: re-downloading without retracting
    /// creates duplicates (ADR-013).
    async fn queue_item_forcibly(&self, item: CollectionItem) -> Result<(), SourceError> {
        let item_id = item.id.as_str().to_string();
        let socket = self.library_socket.clone();
        let retract_id = item_id.clone();
        // Blocking library calls: run on the blocking pool.
        tokio::task::spawn_blocking(move || -> Result<(), SourceError> {
            let client = LibraryClient::connect(&socket).map_err(|e| {
                tracing::warn!(error = %e, %socket, "Library unavailable, resync refused");
                SourceError::SyncFailed {
                    message: format!("library unavailable, resync of {retract_id} refused: {e}"),
                }
            })?;
            client
                .retract_source_facts(&retract_id, &bandcamp_source(), &resync_attributes())
                .map_err(|e| {
                    tracing::error!(error = %e, item_id = %retract_id, "Failed to retract, resync refused");
                    SourceError::SyncFailed {
                        message: format!("retract failed, resync of {retract_id} refused: {e}"),
                    }
                })
        })
        .await
        .map_err(|e| SourceError::Internal {
            message: format!("retract task failed: {e}"),
        })??;

        // Best-effort: evict cache entry
        match self.cache.lock().forget_item(item.id.as_str()) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    item_id = %item.id.as_str(),
                    "Failed to evict cache entry — continuing anyway"
                );
            }
        }

        // Queue regardless of library and cache state (just cleared above);
        // an item already queued or downloading is not queued twice.
        self.enqueue_items(vec![item]);
        Ok(())
    }

    /// Handle a forced resync of a specific item by identifier.
    ///
    /// The library is checked first: a refused resync must not cost a
    /// Bandcamp request.
    async fn handle_resync(&self, identifier: String) -> SourceResponse {
        if let Err(reason) = self.ping_library().await {
            tracing::warn!(%identifier, %reason, "Library unavailable, resync refused");
            return SourceResponse::Error(SourceError::SyncFailed {
                message: format!("library unavailable, resync of {identifier} refused: {reason}"),
            });
        }

        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return SourceResponse::Error(e),
        };

        let username = match self.require_username() {
            Ok(u) => u,
            Err(e) => return SourceResponse::Error(e),
        };

        let collection = match client.get_collection(&username).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch collection for resync");
                return SourceResponse::Error(SourceError::SyncFailed {
                    message: e.to_string(),
                });
            }
        };

        let item = match collection.into_iter().find(|i| i.id.as_str() == identifier) {
            Some(i) => i,
            None => {
                return SourceResponse::Error(SourceError::ItemNotFound { identifier });
            }
        };

        if let Err(e) = self.queue_item_forcibly(item).await {
            return SourceResponse::Error(e);
        }

        SourceResponse::ResyncQueued {
            identifier,
            tracks_queued: 1,
        }
    }

    /// Handle a check-item request: fetch live track count from bandcamp and compare
    /// against the library's stored count for the given item identifier.
    ///
    /// The stored count comes first: without the library every item would
    /// look stale, so an unavailable library is an error, and it costs no
    /// Bandcamp request.
    async fn handle_check_item(&self, identifier: String) -> SourceResponse {
        let stored_track_count = match self.stored_track_count(&identifier).await {
            Ok(count) => count,
            Err(reason) => {
                tracing::warn!(item_id = %identifier, %reason, "CheckItem: library unavailable");
                return SourceResponse::Error(SourceError::CheckFailed {
                    identifier,
                    reason: format!("library unavailable: {reason}"),
                });
            }
        };

        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return SourceResponse::Error(e),
        };

        let username = match self.require_username() {
            Ok(u) => u,
            Err(e) => return SourceResponse::Error(e),
        };

        // Fetch the collection to find the item's download URL
        let collection = match client.get_collection(&username).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "CheckItem: failed to fetch collection");
                return SourceResponse::Error(SourceError::CheckFailed {
                    identifier,
                    reason: e.to_string(),
                });
            }
        };

        let item = match collection.into_iter().find(|i| i.id.as_str() == identifier) {
            Some(i) => i,
            None => {
                return SourceResponse::Error(SourceError::ItemNotFound { identifier });
            }
        };

        // Fetch item details to get the live track count
        let details = match client.get_item_details(&item.download_url).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, item_id = %identifier, "CheckItem: failed to fetch item details");
                return SourceResponse::Error(SourceError::CheckFailed {
                    identifier,
                    reason: e.to_string(),
                });
            }
        };

        let (live_track_count, stale) = match check_outcome(
            details.live_track_count,
            stored_track_count,
        ) {
            Ok(outcome) => outcome,
            Err(reason) => {
                tracing::warn!(item_id = %identifier, %reason, "CheckItem: cannot determine live count");
                return SourceResponse::Error(SourceError::CheckFailed { identifier, reason });
            }
        };

        if stored_track_count > live_track_count {
            tracing::warn!(
                item_id = %identifier,
                live = live_track_count,
                stored = stored_track_count,
                "CheckItem: stored track count exceeds live count — bandcamp may have removed tracks"
            );
        }

        SourceResponse::ItemChecked {
            identifier,
            live_track_count,
            stored_track_count,
            stale,
        }
    }

    /// Handle sync request - fetches collection and queues new downloads.
    /// Username is read from the service config (no longer passed in the request).
    async fn handle_sync(&self) -> SourceResponse {
        match self.queue_collection_sync().await {
            Ok(outcome) => SourceResponse::SyncStarted {
                total_items: outcome.total_items,
                new_items: outcome.newly_queued,
            },
            Err(e) => SourceResponse::Error(e.into_source()),
        }
    }

    /// Fetch the collection and queue the items that are new.
    /// Returns the ids of the items it queued. The library is checked first:
    /// if it is unavailable the sync is deferred without a Bandcamp request.
    async fn queue_collection_sync(&self) -> Result<SyncOutcome, SyncError> {
        if let Err(reason) = self.ping_library().await {
            tracing::warn!(%reason, "Library unavailable, sync deferred");
            return Err(SyncError::LibraryUnavailable(reason));
        }

        // Try to load client (also reloads cookies)
        let client = self.client()?;

        // Get username from config
        let username = self.require_username()?;

        // Store the username
        *self.current_username.lock() = Some(username.clone());

        tracing::info!(username = %username, "Starting collection sync");

        // Fetch the collection
        let collection = match client.get_collection(&username).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch collection");
                return Err(SourceError::SyncFailed {
                    message: e.to_string(),
                }
                .into());
            }
        };

        self.queue_new_collection_items(collection).await
    }

    /// Queue the items of a fetched collection that are new to the library
    /// and the download cache.
    async fn queue_new_collection_items(
        &self,
        collection: Vec<CollectionItem>,
    ) -> Result<SyncOutcome, SyncError> {
        let total_items = collection.len();
        tracing::info!(total = total_items, "Fetched collection");

        // Library dedup makes blocking IPC calls for every item, so it runs
        // on the blocking pool with no service lock held: status polls, the
        // download worker and cancels must not wait on it.
        let socket = self.library_socket.clone();
        let unknown =
            tokio::task::spawn_blocking(move || drop_items_known_to_library(&socket, collection))
                .await
                .map_err(|e| {
                    SyncError::Source(SourceError::SyncFailed {
                        message: format!("library dedup task failed: {e}"),
                    })
                })??;

        let (queued_ids, newly_queued) = self.enqueue_new_items(unknown);

        tracing::info!(
            total = total_items,
            new = newly_queued,
            awaiting = queued_ids.len(),
            "Sync complete, items queued"
        );

        Ok(SyncOutcome {
            total_items,
            queued_ids,
            newly_queued,
        })
    }

    /// Queue the items the library does not have, unless the download cache
    /// says they were fetched already.
    ///
    /// Returns `(ids to wait on, how many were newly queued)`. An item already
    /// in the queue or downloading is not queued twice, but its id is still
    /// returned so an import waits for it. No blocking calls: the queue locks
    /// are held only for this in-memory bookkeeping.
    fn enqueue_new_items(&self, items: Vec<CollectionItem>) -> (Vec<String>, usize) {
        let items: Vec<CollectionItem> = {
            let cache = self.cache.lock();
            items
                .into_iter()
                .filter(|item| {
                    let cache_key =
                        format!("{}|{}|{}|0", item.artist, item.title, item.id.as_str());
                    !(cache.is_downloaded(&cache_key) || cache.is_item_downloaded(item.id.as_str()))
                })
                .collect()
        };
        self.enqueue_items(items)
    }

    /// The single queueing rule: queue each item unless it is already in the
    /// queue or downloading, and clear a stale finished entry first. Returns
    /// `(ids to wait on, how many were newly queued)`; in-flight ids are
    /// returned too. Callers apply any library or cache dedup beforehand.
    fn enqueue_items(&self, items: Vec<CollectionItem>) -> (Vec<String>, usize) {
        let mut wanted_ids = Vec::new();
        let mut newly_queued = 0;
        // Lock order: queue, then active (as `cancel_download` and the worker).
        let mut queue = self.download_queue.lock();
        let mut active = self.active_downloads.lock();
        for item in items {
            let id = item.id.as_str().to_string();
            let in_flight = queue.iter().any(|dl| dl.item.id.as_str() == id)
                || active.get(&id).is_some_and(|dl| !dl.state.is_terminal());
            wanted_ids.push(id.clone());
            if in_flight {
                continue;
            }
            // A finished entry from an earlier run must not be mistaken
            // for this one by anything waiting on the item.
            active.remove(&id);
            queue.push_back(QueuedDownload {
                item,
                state: InternalDownloadState::Queued,
                downloaded_bytes: 0,
                total_bytes: None,
                error: None,
            });
            newly_queued += 1;
        }
        (wanted_ids, newly_queued)
    }

    /// Start a playlist import job in the background and return at once.
    ///
    /// Only one job runs at a time. The job ends in Done or Failed, whatever
    /// happens inside it.
    fn handle_import_playlist(
        self: &Arc<Self>,
        playlist: String,
        name: Option<String>,
        dry_run: bool,
    ) -> SourceResponse {
        let target = match name.as_deref().map(validated_playlist_name).transpose() {
            Ok(target) => target,
            Err(message) => return SourceResponse::Error(SourceError::InvalidRequest { message }),
        };

        let job_id = match self.begin_import_job(dry_run) {
            Ok(job_id) => job_id,
            Err(running) => {
                return SourceResponse::Error(SourceError::ImportInProgress { job_id: running })
            }
        };
        tracing::info!(job_id, %playlist, dry_run, "Import job started");

        let service = Arc::clone(self);
        tokio::spawn(async move {
            // The job runs in its own task so a panic surfaces as a JoinError
            // here instead of leaving the status stuck in a running phase.
            let job = tokio::spawn({
                let service = Arc::clone(&service);
                async move {
                    service
                        .run_import_job(job_id, playlist, target, dry_run)
                        .await
                }
            });
            if let Err(e) = job.await {
                service.fail_import(job_id, join_error_message(&e));
            }
        });

        SourceResponse::ImportStarted { job_id }
    }

    /// Create the status of a new import job (phase Fetching) and return its id.
    /// `Err` carries the id of the job that is still running.
    fn begin_import_job(&self, dry_run: bool) -> Result<u64, u64> {
        let mut status = self.import_status.lock();
        if let Some(running) = status
            .as_ref()
            .filter(|s| import_phase_is_running(&s.phase))
        {
            return Err(running.job_id);
        }
        let job_id = self.last_import_job_id.fetch_add(1, Ordering::Relaxed) + 1;
        *status = Some(ImportJobStatus {
            job_id,
            dry_run,
            playlist_title: None,
            target_name: None,
            total_count: None,
            phase: ImportPhase::Fetching,
            tracks: Vec::new(),
            sync: None,
        });
        Ok(job_id)
    }

    /// End import job `job_id` as Failed.
    fn fail_import(&self, job_id: u64, message: String) {
        tracing::error!(job_id, error = %message, "Import job failed");
        self.set_import_phase(job_id, ImportPhase::Failed { message });
    }

    /// Update the status of import job `job_id`. Ignored when the current
    /// status belongs to another job, so a stale task cannot overwrite the
    /// status of a newer one.
    fn update_import(&self, job_id: u64, update: impl FnOnce(&mut ImportJobStatus)) {
        if let Some(status) = self
            .import_status
            .lock()
            .as_mut()
            .filter(|s| s.job_id == job_id)
        {
            update(status);
        }
    }

    /// Move import job `job_id` to `phase`, logging the change.
    fn set_import_phase(&self, job_id: u64, phase: ImportPhase) {
        self.update_import(job_id, |status| {
            tracing::info!(job_id = status.job_id, ?phase, "Import phase changed");
            status.phase = phase;
        });
    }

    /// Fetch the playlist, then run the import.
    async fn run_import_job(
        &self,
        job_id: u64,
        playlist_arg: String,
        target: Option<(String, PlaylistName)>,
        dry_run: bool,
    ) {
        match self.fetch_playlist(&playlist_arg).await {
            Ok(source) => {
                self.run_import(job_id, source, &playlist_arg, target, dry_run)
                    .await
            }
            Err(e) => self.fail_import(job_id, e.to_string()),
        }
    }

    /// Run the import steps; an error ends the job as Failed.
    async fn run_import(
        &self,
        job_id: u64,
        source: SourcePlaylist,
        playlist_arg: &str,
        target: Option<(String, PlaylistName)>,
        dry_run: bool,
    ) {
        if let Err(message) = self
            .import_steps(job_id, source, playlist_arg, target, dry_run)
            .await
        {
            self.fail_import(job_id, message);
        }
    }

    /// Match, sync if needed, rematch and write.
    ///
    /// A dry run skips the sync and the write. Otherwise, when an owned track
    /// is missing from the library it runs a full collection sync, waits for
    /// the downloads it needs, and matches again before writing.
    ///
    /// If downloads are paused while the import waits for them, the wait ends
    /// at once and the import carries on to the rematch with what the library
    /// has (as it does after a timeout or failed downloads); the report lists
    /// what is still missing, and the queued downloads resume with the queue.
    ///
    /// If the sync is deferred because the library is unavailable, the
    /// import likewise carries on without the rematch: it writes the
    /// first-pass definitive matches and finishes, with `sync` recorded as
    /// nothing queued and no end reason, and the missing tracks reported as
    /// missing. Other sync errors fail the job.
    async fn import_steps(
        &self,
        job_id: u64,
        source: SourcePlaylist,
        playlist_arg: &str,
        target: Option<(String, PlaylistName)>,
        dry_run: bool,
    ) -> Result<(), String> {
        let (target_name, playlist_name) = match target {
            Some(target) => target,
            None => validated_playlist_name(&default_import_name(&source.title, playlist_arg))?,
        };
        self.update_import(job_id, |status| {
            status.playlist_title = Some(source.title.clone());
            status.target_name = Some(target_name.clone());
            status.total_count = Some(source.total_count);
        });

        let tracks: Arc<[SourcePlaylistTrack]> = source.tracks.into();

        self.set_import_phase(job_id, ImportPhase::Matching);
        let mut outcomes = self.match_import_tracks(&tracks).await?;
        self.update_import(job_id, |status| {
            status.tracks = to_import_reports(&tracks, &outcomes)
        });

        // A dry run never syncs: it reports what the library has now.
        if !dry_run && has_owned_missing(&tracks, &outcomes) {
            self.set_import_phase(job_id, ImportPhase::Syncing);
            match self.queue_collection_sync().await {
                Ok(sync) => {
                    self.wait_for_downloads(
                        job_id,
                        &sync.queued_ids,
                        IMPORT_DOWNLOAD_POLL,
                        IMPORT_DOWNLOAD_LIMIT,
                    )
                    .await;

                    self.set_import_phase(job_id, ImportPhase::Rematching);
                    outcomes = self.match_import_tracks(&tracks).await?;
                    self.update_import(job_id, |status| {
                        status.tracks = to_import_reports(&tracks, &outcomes)
                    });
                }
                Err(SyncError::LibraryUnavailable(reason)) => {
                    // Carry on with the first-pass matches; the report lists
                    // the rest as missing. The status has no field for the
                    // reason: a sync that never waited is `sync` with
                    // nothing queued and no end reason.
                    tracing::warn!(%reason, "Import sync deferred, library unavailable");
                    self.update_import(job_id, |status| {
                        status.sync = Some(ImportSyncProgress {
                            queued: 0,
                            completed: 0,
                            failed: 0,
                            ended: None,
                        })
                    });
                }
                Err(SyncError::Source(e)) => return Err(e.to_string()),
            }
        }

        let hashes = matched_hashes(&outcomes);
        if hashes.is_empty() {
            // Never replace an existing playlist with an empty one; a dry
            // run reports the same, not "would write 0 tracks".
            tracing::warn!("Import matched no tracks, playlist not written");
            self.set_import_phase(job_id, ImportPhase::Done(ImportResult::NothingMatched));
            return Ok(());
        }

        if dry_run {
            self.set_import_phase(
                job_id,
                ImportPhase::Done(ImportResult::DryRun {
                    would_write: hashes.len() as u32,
                }),
            );
            return Ok(());
        }

        self.set_import_phase(job_id, ImportPhase::Writing);
        let written_tracks = hashes.len() as u32;
        let socket = self.library_socket.clone();
        let created = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            let client = LibraryClient::connect(&socket)
                .map_err(|e| format!("library not available: {e}"))?;
            write_playlist(&client, &playlist_name, &hashes)
                .map_err(|e| format!("failed to write playlist: {e}"))
        })
        .await
        .map_err(|e| format!("write task failed: {e}"))??;

        self.set_import_phase(
            job_id,
            ImportPhase::Done(ImportResult::Written(ImportWritten {
                created,
                tracks: written_tracks,
            })),
        );
        Ok(())
    }

    /// Match the playlist tracks against the library. The library calls
    /// block, so they run on the blocking pool with their own client.
    /// A failed library lookup is an error: nothing may be written from a
    /// partial match.
    async fn match_import_tracks(
        &self,
        tracks: &Arc<[SourcePlaylistTrack]>,
    ) -> Result<Vec<TrackOutcome>, String> {
        let socket = self.library_socket.clone();
        let tracks = Arc::clone(tracks);
        tokio::task::spawn_blocking(move || -> Result<Vec<TrackOutcome>, String> {
            let client = LibraryClient::connect(&socket)
                .map_err(|e| format!("library not available: {e}"))?;
            let lookup = LibraryTrackLookup::new(|query| client.search(query));
            let outcomes = match_source_playlist(&tracks, &lookup);
            lookup.check().map_err(|e| e.to_string())?;
            Ok(outcomes)
        })
        .await
        .map_err(|e| format!("match task failed: {e}"))?
    }

    /// Wait until every id in `ids` has finished downloading (completed or
    /// failed) or `limit` has passed, updating the job's sync progress on
    /// each poll. On timeout, or when downloads are paused (the remaining
    /// ones would not arrive), it only logs: the import carries on with
    /// whatever has arrived.
    async fn wait_for_downloads(
        &self,
        job_id: u64,
        ids: &[String],
        poll: Duration,
        limit: Duration,
    ) -> SyncWaitEnd {
        let queued = ids.len() as u32;
        let started = Instant::now();
        loop {
            let (completed, failed) = tally_downloads(&self.active_downloads.lock(), ids);
            let ended = if completed + failed >= queued {
                Some(SyncWaitEnd::AllFinished)
            } else if self.paused.load(Ordering::Relaxed) {
                tracing::warn!(
                    queued,
                    completed,
                    failed,
                    "Downloads are paused, continuing import with what has arrived"
                );
                Some(SyncWaitEnd::Paused)
            } else if started.elapsed() >= limit {
                tracing::warn!(
                    queued,
                    completed,
                    failed,
                    "Timed out waiting for import downloads, continuing with what has arrived"
                );
                Some(SyncWaitEnd::TimedOut)
            } else {
                None
            };
            self.update_import(job_id, |status| {
                status.sync = Some(ImportSyncProgress {
                    queued,
                    completed,
                    failed,
                    ended,
                });
            });
            if let Some(end) = ended {
                return end;
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// List current downloads
    fn list_downloads(&self) -> Vec<DownloadStatus> {
        let mut downloads = Vec::new();

        // Add active downloads
        for (id, dl) in self.active_downloads.lock().iter() {
            downloads.push(DownloadStatus {
                id: DownloadId::new(id.clone()),
                artist: dl.item.artist.to_string(),
                title: dl.item.title.to_string(),
                state: dl.state.to_protocol(&dl.error),
                downloaded_bytes: dl.downloaded_bytes,
                total_bytes: dl.total_bytes,
            });
        }

        // Add queued downloads
        for dl in self.download_queue.lock().iter() {
            downloads.push(DownloadStatus {
                id: DownloadId::new(dl.item.id.as_str()),
                artist: dl.item.artist.to_string(),
                title: dl.item.title.to_string(),
                state: DownloadState::Queued,
                downloaded_bytes: 0,
                total_bytes: None,
            });
        }

        downloads
    }

    /// Cancel a download.
    ///
    /// A queued item leaves the queue and gets a Cancelled entry in
    /// `active_downloads`, the record anything waiting on the item reads. An
    /// active one is marked Cancelled. Finished downloads stay as they are.
    fn cancel_download(&self, id: &str) {
        // Lock order: queue, then active (as the worker and enqueue do).
        let mut queue = self.download_queue.lock();
        let mut active = self.active_downloads.lock();

        let queued = queue.iter().find(|dl| dl.item.id.as_str() == id).cloned();
        queue.retain(|dl| dl.item.id.as_str() != id);
        if let Some(mut dl) = queued {
            dl.state = InternalDownloadState::Cancelled;
            active.insert(id.to_string(), dl);
        } else if let Some(dl) = active.get_mut(id).filter(|dl| !dl.state.is_terminal()) {
            dl.state = InternalDownloadState::Cancelled;
        }
    }
}

use inbox_utils::{detect_file_type, extract_zip, sanitize_filename, unique_path};

/// Process a downloaded file - extract if ZIP, move if audio file.
/// Returns the list of files moved to inbox.
fn process_download_to_inbox(
    download_path: &Path,
    inbox_dir: &Path,
    artist: &str,
    title: &str,
) -> Result<Vec<PathBuf>, std::io::Error> {
    let file_type = detect_file_type(download_path);
    tracing::debug!(path = %download_path.display(), file_type = ?file_type, "Detected file type");

    match file_type {
        Some("zip") => extract_zip(download_path, inbox_dir),
        Some(ext @ ("flac" | "mp3" | "wav" | "aiff")) => {
            // Single track - move directly to inbox with proper name
            let safe_artist = sanitize_filename(artist);
            let safe_title = sanitize_filename(title);
            let filename = format!("{} - {}.{}", safe_artist, safe_title, ext);
            let dest_path = unique_path(inbox_dir, &filename);

            std::fs::rename(download_path, &dest_path)?;
            tracing::info!(dest = %dest_path.display(), "Moved audio file to inbox");
            Ok(vec![dest_path])
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Unknown file type for {:?}", download_path),
        )),
    }
}

/// Run the async IPC server
///
/// This spawns a blocking task for NNG recv/send and bridges to async via channels.
/// The caller is responsible for creating and binding the socket via `service::create_sockets`.
pub async fn run_async_ipc_server(
    service: Arc<BandcampService>,
    socket: nng::Socket,
) -> Result<(), ServiceError> {
    // Create channel for requests from NNG thread to async runtime
    let (request_tx, mut request_rx) = mpsc::channel::<IpcMessage>(32);

    // Spawn the NNG server in a blocking task
    let nng_handle = { tokio::task::spawn_blocking(move || run_nng_bridge(socket, request_tx)) };

    tracing::info!("Async IPC server running");

    // Process requests from the NNG bridge
    while let Some(msg) = request_rx.recv().await {
        let response = service.handle_request(msg.request).await;

        // Send response back (ignore error if receiver dropped)
        let _ = msg.response_tx.send(response);
    }

    // Wait for NNG thread to finish (it won't normally)
    nng_handle.await.map_err(|_| ServiceError::Channel)??;

    Ok(())
}

/// NNG bridge - runs in a blocking thread, communicates via channels
fn run_nng_bridge(
    socket: nng::Socket,
    request_tx: mpsc::Sender<IpcMessage>,
) -> Result<(), ServiceError> {
    let server = IpcServer::new(socket);

    tracing::info!("NNG server ready to receive requests");

    loop {
        // Blocking recv
        let raw = match server.recv_raw() {
            Ok(r) => r,
            Err(e) => {
                // Nothing was received, so there is nothing to reply to
                tracing::error!(error = %e, "Failed to receive request");
                continue;
            }
        };
        let request = match parse_request(&raw) {
            Ok(r) => r,
            Err(e) => {
                // Reply so the gateway doesn't stall until its timeout
                tracing::warn!(error = %e, "Failed to parse request");
                let reply = SourceResponse::Error(SourceError::unsupported(format!(
                    "unparseable request: {e}"
                )));
                if let Err(e) = server.send(&reply) {
                    tracing::error!(error = %e, "Failed to send parse-error reply");
                }
                continue;
            }
        };

        // Create oneshot channel for response
        let (response_tx, response_rx) = oneshot::channel();

        // Send to async runtime
        if request_tx
            .blocking_send(IpcMessage {
                request,
                response_tx,
            })
            .is_err()
        {
            tracing::error!("Failed to send request to async runtime - shutting down");
            break;
        }

        // Wait for response
        let response = match response_rx.blocking_recv() {
            Ok(r) => r,
            Err(_) => {
                tracing::error!("Response channel closed");
                SourceResponse::Error(SourceError::Internal {
                    message: "Internal error: response channel closed".to_string(),
                })
            }
        };

        // Send response
        if let Err(e) = server.send(&response) {
            tracing::error!(error = %e, "Failed to send response");
        }
    }

    Ok(())
}

/// Async download worker - processes the download queue
pub async fn run_download_worker(service: Arc<BandcampService>) {
    tracing::info!("Download worker started");

    loop {
        // Check if paused
        if service.paused.load(Ordering::Relaxed) {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            continue;
        }

        // Get next item from queue and move it to active downloads in one
        // step, so it is never in neither (a sync or cancel arriving in
        // between would otherwise miss it). Lock order: queue, then active.
        let next_item = {
            let mut queue = service.download_queue.lock();
            queue.pop_front().map(|mut queued| {
                queued.state = InternalDownloadState::Downloading;
                service
                    .active_downloads
                    .lock()
                    .insert(queued.item.id.as_str().to_string(), queued.clone());
                queued
            })
        };

        let Some(queued) = next_item else {
            // Queue empty, wait a bit
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            continue;
        };

        let item_id = queued.item.id.as_str().to_string();

        // Load client for this download
        let client = match service.client() {
            Ok(c) => c,
            Err(_) => {
                let mut active = service.active_downloads.lock();
                if let Some(dl) = active.get_mut(&item_id) {
                    dl.state = InternalDownloadState::Failed;
                    dl.error = Some(NOT_CONFIGURED_MESSAGE.to_string());
                }
                service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        tracing::info!(
            item_id = %item_id,
            artist = %queued.item.artist,
            title = %queued.item.title,
            "Starting download"
        );

        // Get download details
        let details = match client.get_item_details(&queued.item.download_url).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, item_id = %item_id, "Failed to get item details");
                let mut active = service.active_downloads.lock();
                if let Some(dl) = active.get_mut(&item_id) {
                    dl.state = InternalDownloadState::Failed;
                    dl.error = Some(format!("Failed to get details: {}", e));
                }
                service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        // Download to staging directory (use .download extension, actual type detected later)
        let staging_path = service.downloads_dir.join(format!("{}.download", item_id));

        use tokio_stream::StreamExt;
        let mut stream = std::pin::pin!(client.download_item(
            &details,
            service.format,
            &staging_path,
            &queued.item.download_url
        ));

        let mut download_success = false;
        while let Some(event) = stream.next().await {
            match event {
                bandcamp_api::DownloadEvent::Started { total } => {
                    let mut active = service.active_downloads.lock();
                    if let Some(dl) = active.get_mut(&item_id) {
                        dl.total_bytes = total;
                    }
                }
                bandcamp_api::DownloadEvent::Progress(progress) => {
                    let mut active = service.active_downloads.lock();
                    if let Some(dl) = active.get_mut(&item_id) {
                        dl.downloaded_bytes = progress.downloaded;
                        dl.total_bytes = progress.total;
                    }
                }
                bandcamp_api::DownloadEvent::Completed { path } => {
                    tracing::info!(item_id = %item_id, path = %path.display(), "Download completed");
                    download_success = true;

                    // Update state to extracting
                    {
                        let mut active = service.active_downloads.lock();
                        if let Some(dl) = active.get_mut(&item_id) {
                            dl.state = InternalDownloadState::Extracting;
                        }
                    }

                    // Process download - extract ZIP or move single track
                    let inbox_dir = service.inbox_dir.clone();
                    let download_path = path.clone();
                    let artist = queued.item.artist.to_string();
                    let title = queued.item.title.to_string();

                    // Run in blocking task since file I/O is sync
                    let extract_result = tokio::task::spawn_blocking(move || {
                        process_download_to_inbox(&download_path, &inbox_dir, &artist, &title)
                    })
                    .await;

                    match extract_result {
                        Ok(Ok(extracted_files)) => {
                            tracing::info!(
                                item_id = %item_id,
                                files = extracted_files.len(),
                                "Extracted audio files to inbox"
                            );

                            // Update state to moving
                            {
                                let mut active = service.active_downloads.lock();
                                if let Some(dl) = active.get_mut(&item_id) {
                                    dl.state = InternalDownloadState::Moving;
                                }
                            }

                            // Delete the source file (ZIP after extraction)
                            // For single tracks, the file was moved so this will fail - that's OK
                            if path.exists() {
                                if let Err(e) = tokio::fs::remove_file(&path).await {
                                    tracing::warn!(error = %e, "Failed to delete source file");
                                }
                            }

                            // Auto-ingest via library service
                            if let Some(lib_client) = service.try_library_client() {
                                for file_path in &extracted_files {
                                    let filename = file_path
                                        .file_name()
                                        .and_then(|f| f.to_str())
                                        .unwrap_or("unknown");

                                    match InboxPath::new(filename) {
                                        Ok(inbox_path) => {
                                            match lib_client.ingest_file_as(
                                                &inbox_path,
                                                &bandcamp_source(),
                                                bandcamp_provenance(&item_id),
                                            ) {
                                                Ok(library_ipc_client::IngestResult::Success {
                                                    hash,
                                                    ..
                                                }) => {
                                                    tracing::info!(
                                                        file = %filename,
                                                        hash = ?hash,
                                                        "Auto-ingested into library"
                                                    );
                                                }
                                                Ok(library_ipc_client::IngestResult::Failure {
                                                    message,
                                                }) => {
                                                    tracing::warn!(
                                                        file = %filename,
                                                        msg = %message,
                                                        "Library ingest returned failure"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        error = %e,
                                                        file = %filename,
                                                        "Failed to auto-ingest into library"
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                file = %filename,
                                                error = %e,
                                                "Skipping extracted file — inbox path validation failed"
                                            );
                                        }
                                    }
                                }
                            } else {
                                tracing::debug!("Library not available, skipping auto-ingest");
                            }

                            // Mark as completed
                            {
                                let mut active = service.active_downloads.lock();
                                if let Some(dl) = active.get_mut(&item_id) {
                                    dl.state = InternalDownloadState::Completed;
                                }
                            }
                            service.downloads_completed.fetch_add(1, Ordering::Relaxed);

                            // Update cache for each extracted file (fallback dedup)
                            for file_path in &extracted_files {
                                let filename = file_path
                                    .file_name()
                                    .and_then(|f| f.to_str())
                                    .unwrap_or("unknown");
                                let cache_key = format!(
                                    "{}|{}|{}|0",
                                    queued.item.artist, queued.item.title, filename
                                );
                                if let Err(e) =
                                    service.cache.lock().mark_downloaded(&cache_key, &item_id)
                                {
                                    tracing::warn!(error = %e, "Failed to update cache");
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, item_id = %item_id, "Failed to process download");
                            let mut active = service.active_downloads.lock();
                            if let Some(dl) = active.get_mut(&item_id) {
                                dl.state = InternalDownloadState::Failed;
                                dl.error = Some(format!("Processing failed: {}", e));
                            }
                            service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::error!(error = %e, item_id = %item_id, "Extraction task panicked");
                            let mut active = service.active_downloads.lock();
                            if let Some(dl) = active.get_mut(&item_id) {
                                dl.state = InternalDownloadState::Failed;
                                dl.error = Some("Extraction task panicked".to_string());
                            }
                            service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                bandcamp_api::DownloadEvent::Failed { error } => {
                    if error.contains("permanently expired") {
                        tracing::warn!(
                            item_id = %item_id,
                            artist = %queued.item.artist,
                            title = %queued.item.title,
                            "Download links expired — revalidate at bandcamp.com"
                        );
                    } else {
                        tracing::error!(item_id = %item_id, error = %error, "Download failed");
                    }
                    let mut active = service.active_downloads.lock();
                    if let Some(dl) = active.get_mut(&item_id) {
                        dl.state = InternalDownloadState::Failed;
                        dl.error = Some(error);
                    }
                    service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        if !download_success {
            // Stream ended without success event
            let mut active = service.active_downloads.lock();
            if let Some(dl) = active.get_mut(&item_id) {
                if dl.state == InternalDownloadState::Downloading {
                    dl.state = InternalDownloadState::Failed;
                    dl.error = Some("Download stream ended unexpectedly".to_string());
                    service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // Small delay between downloads for rate limiting
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_protocol::{ContentHash, TrackInfo};

    // =========================================================================
    // Source identity
    // =========================================================================

    #[test]
    fn bandcamp_states_its_own_source_provenance_and_resync_attributes() {
        let source = bandcamp_source();
        assert_eq!(source.tool, "mdma-bandcamp");
        assert_eq!(source.origin.kind, "Bandcamp");
        assert_eq!(
            bandcamp_provenance("p42"),
            vec![
                MusicValue::Source("bandcamp".to_string()),
                MusicValue::ItemId("p42".to_string()),
            ]
        );
        let attributes: Vec<String> = resync_attributes()
            .as_slice()
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        assert_eq!(
            attributes,
            ["Album", "Title", "Artist", "TrackNumber", "Year"]
        );
        assert!(!attributes.iter().any(|a| a == "ItemId" || a == "Source"));
    }

    // =========================================================================
    // classify_check_result tests
    // =========================================================================

    #[test]
    fn classify_check_result_flags_stale_when_live_exceeds_stored() {
        assert!(
            classify_check_result(7, 2),
            "live=7, stored=2 should be stale"
        );
    }

    #[test]
    fn classify_check_result_not_stale_when_counts_equal() {
        assert!(
            !classify_check_result(5, 5),
            "live=5, stored=5 should not be stale"
        );
    }

    #[test]
    fn classify_check_result_not_stale_when_live_fewer_than_stored() {
        // Bandcamp removed tracks — unusual, not flagged as stale by this helper
        assert!(
            !classify_check_result(2, 5),
            "live=2, stored=5 should not be flagged stale (bandcamp removed tracks)"
        );
    }

    #[test]
    fn classify_check_result_not_stale_when_both_zero() {
        assert!(!classify_check_result(0, 0));
    }

    #[test]
    fn check_outcome_unknown_live_count_is_an_error_not_ok() {
        let reason = check_outcome(None, 3).unwrap_err();
        assert!(reason.contains("unknown"), "got: {reason}");
    }

    #[test]
    fn check_outcome_stale_when_live_exceeds_stored() {
        assert_eq!(check_outcome(Some(7), 2), Ok((7, true)));
    }

    #[test]
    fn check_outcome_ok_when_equal() {
        assert_eq!(check_outcome(Some(5), 5), Ok((5, false)));
    }

    #[test]
    fn check_outcome_not_stale_when_stored_exceeds_live() {
        assert_eq!(check_outcome(Some(2), 5), Ok((2, false)));
    }

    // =========================================================================
    // Playlist mapping tests
    // =========================================================================

    #[test]
    fn playlist_summary_maps_to_protocol() {
        let summary = bandcamp_api::PlaylistSummary {
            id: 42,
            title: "Night drive".to_string(),
            url: "https://bandcamp.com/someone/playlist/night-drive".to_string(),
            slug: "night-drive".to_string(),
            track_count: 7,
            is_private: true,
        };
        let mapped = to_source_playlist_summary(summary);
        assert_eq!(mapped.id, "42");
        assert_eq!(mapped.title, "Night drive");
        assert_eq!(mapped.slug, "night-drive");
        assert_eq!(mapped.track_count, 7);
        assert!(mapped.is_private);
    }

    #[test]
    fn playlist_maps_to_protocol_keeping_positions_and_fields() {
        let playlist = bandcamp_api::Playlist {
            id: 9,
            title: "Mix".to_string(),
            url: "https://bandcamp.com/someone/playlist/mix".to_string(),
            is_private: false,
            total_count: 2,
            tracks: vec![
                bandcamp_api::PlaylistTrack {
                    position: 1,
                    track_id: 111,
                    title: "One".to_string(),
                    artist: "A".to_string(),
                    album_title: Some("Album".to_string()),
                    album_id: Some(5),
                    duration_secs: 12.5,
                    is_owned: true,
                    url: "https://a.bandcamp.com/track/one".to_string(),
                },
                bandcamp_api::PlaylistTrack {
                    position: 2,
                    track_id: 222,
                    title: "Two".to_string(),
                    artist: "B".to_string(),
                    album_title: None,
                    album_id: None,
                    duration_secs: 30.0,
                    is_owned: false,
                    url: "https://b.bandcamp.com/track/two".to_string(),
                },
            ],
        };
        let mapped = to_source_playlist(playlist);
        assert_eq!(mapped.id, "9");
        assert_eq!(mapped.total_count, 2);
        assert_eq!(mapped.tracks.len(), 2);
        assert_eq!(mapped.tracks[0].position, 1);
        assert_eq!(mapped.tracks[0].source_track_id, "111");
        assert_eq!(mapped.tracks[0].album.as_deref(), Some("Album"));
        assert!(mapped.tracks[0].owned);
        assert_eq!(mapped.tracks[1].position, 2);
        assert_eq!(mapped.tracks[1].album, None);
        assert!(!mapped.tracks[1].owned);
    }

    #[test]
    fn playlist_error_404_is_item_not_found() {
        match playlist_error_for_status(Some(404), "boom", Some("my-list")) {
            SourceError::ItemNotFound { identifier } => assert_eq!(identifier, "my-list"),
            other => panic!("expected ItemNotFound, got {other:?}"),
        }
    }

    #[test]
    fn playlist_error_404_when_listing_is_not_item_not_found() {
        match playlist_error_for_status(Some(404), "boom", None) {
            SourceError::SyncFailed { message } => assert_eq!(message, "boom"),
            other => panic!("expected SyncFailed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[case::server_error(Some(500))]
    #[case::no_status(None)]
    fn playlist_error_other_is_generic(#[case] status: Option<u16>) {
        match playlist_error_for_status(status, "boom", Some("my-list")) {
            SourceError::SyncFailed { message } => assert_eq!(message, "boom"),
            other => panic!("expected SyncFailed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[case::not_logged_in(bandcamp_api::BandcampError::NotLoggedIn)]
    #[case::auth(bandcamp_api::BandcampError::Auth("nope".to_string()))]
    #[case::cookies_expired(bandcamp_api::BandcampError::CookiesExpired {
        cookie: "identity".to_string(),
        expired_ago_secs: 5,
    })]
    fn playlist_error_auth_failures_are_not_authenticated(
        #[case] err: bandcamp_api::BandcampError,
    ) {
        match playlist_error(&err, Some("my-list")) {
            SourceError::NotAuthenticated { message } => assert_eq!(message, err.to_string()),
            other => panic!("expected NotAuthenticated, got {other:?}"),
        }
    }

    #[test]
    fn playlist_error_rate_limited_is_rate_limited() {
        let err = bandcamp_api::BandcampError::RateLimited {
            retry_after_secs: 42,
        };
        match playlist_error(&err, None) {
            SourceError::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 42),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn playlist_error_invalid_url_is_sync_failed_with_reason() {
        let err = bandcamp_api::BandcampError::InvalidUrl("http://evil".to_string());
        match playlist_error(&err, Some("http://evil")) {
            SourceError::SyncFailed { message } => assert!(message.contains("http://evil")),
            other => panic!("expected SyncFailed, got {other:?}"),
        }
    }

    // =========================================================================
    // should_skip_by_work_identity tests
    // =========================================================================

    fn make_collection_item(id: &str, artist: &str, title: &str) -> CollectionItem {
        use bandcamp_api::{ItemId, ItemType};
        use music_facts::{Artist, Title};
        CollectionItem {
            id: ItemId::new(id),
            artist: Artist::new(artist),
            title: Title::new(title),
            item_type: ItemType::Album,
            purchased: None,
            download_url: "https://bandcamp.com/download/test".to_string(),
        }
    }

    fn make_track_info(hash: &str, artist: &str, album: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: Some("some track".to_string()),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
        }
    }

    #[test]
    fn should_skip_by_work_identity_empty_results_returns_false() {
        let item = make_collection_item("p_vinyl_111", "Carbon Based Lifeforms", "Interloper");
        assert!(
            !should_skip_by_work_identity(&item, &[]),
            "empty search results → do NOT skip (work not yet in library)"
        );
    }

    #[test]
    fn should_skip_by_work_identity_matching_artist_and_album_returns_true() {
        let item = make_collection_item("p_digital_222", "Carbon Based Lifeforms", "Interloper");
        let existing = make_track_info("sha256:aabbcc", "Carbon Based Lifeforms", "Interloper");
        assert!(
            should_skip_by_work_identity(&item, &[existing]),
            "matching artist+album → skip download"
        );
    }

    #[test]
    fn should_skip_by_work_identity_matching_artist_wrong_album_returns_false() {
        let item = make_collection_item("p_digital_222", "Carbon Based Lifeforms", "Interloper");
        let existing = make_track_info("sha256:aabbcc", "Carbon Based Lifeforms", "Twentythree");
        assert!(
            !should_skip_by_work_identity(&item, &[existing]),
            "matching artist but different album → do NOT skip"
        );
    }

    #[test]
    fn should_skip_by_work_identity_wrong_artist_matching_album_returns_false() {
        let item = make_collection_item("p_digital_222", "Carbon Based Lifeforms", "Interloper");
        let existing = make_track_info("sha256:aabbcc", "Someone Else", "Interloper");
        assert!(
            !should_skip_by_work_identity(&item, &[existing]),
            "matching album but different artist → do NOT skip"
        );
    }

    #[test]
    fn should_skip_by_work_identity_second_of_two_matches_returns_true() {
        let item = make_collection_item("p_digital_222", "Carbon Based Lifeforms", "Interloper");
        let no_match = make_track_info("sha256:aabbcc", "Carbon Based Lifeforms", "Twentythree");
        let matching = make_track_info("sha256:ddeeff", "Carbon Based Lifeforms", "Interloper");
        assert!(
            should_skip_by_work_identity(&item, &[no_match, matching]),
            "second result matches → skip download"
        );
    }

    #[test]
    fn should_skip_by_work_identity_case_and_whitespace_normalize() {
        let item = make_collection_item("p_digital_222", "carbon based lifeforms", "interloper");
        let existing = make_track_info(
            "sha256:aabbcc",
            "  Carbon Based Lifeforms  ",
            "  Interloper  ",
        );
        assert!(
            should_skip_by_work_identity(&item, &[existing]),
            "strings differing only in case/whitespace should still match via normalize"
        );
    }

    // =========================================================================
    // Cookie state tests
    // =========================================================================

    const VALID_COOKIES: &str =
        r#"[{"name":"identity","value":"abc123","domain":".bandcamp.com"}]"#;

    fn make_service(dir: &std::path::Path, cookies: PathBuf) -> Arc<BandcampService> {
        make_service_with_library(dir, cookies, "ipc:///nonexistent/library.sock")
    }

    fn make_service_with_library(
        dir: &std::path::Path,
        cookies: PathBuf,
        library_socket: &str,
    ) -> Arc<BandcampService> {
        Arc::new(
            BandcampService::new(
                cookies,
                dir.join("downloads"),
                dir.join("inbox"),
                dir.join("cache"),
                AudioFormat::Flac,
                library_socket.to_string(),
                Some("someone".to_string()),
            )
            .expect("service should start even without cookies"),
        )
    }

    // =========================================================================
    // Playlist import tests
    // =========================================================================

    fn queued_download(id: &str, state: InternalDownloadState) -> QueuedDownload {
        QueuedDownload {
            item: make_collection_item(id, "Artist", "Album"),
            state,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        }
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn import_status(service: &Arc<BandcampService>) -> ImportJobStatus {
        service
            .import_status
            .lock()
            .clone()
            .expect("an import job should exist")
    }

    #[test]
    fn tally_counts_finished_downloads_and_treats_absent_ids_as_pending() {
        let mut active = HashMap::new();
        active.insert(
            "done".to_string(),
            queued_download("done", InternalDownloadState::Completed),
        );
        active.insert(
            "bad".to_string(),
            queued_download("bad", InternalDownloadState::Failed),
        );
        active.insert(
            "stopped".to_string(),
            queued_download("stopped", InternalDownloadState::Cancelled),
        );
        active.insert(
            "busy".to_string(),
            queued_download("busy", InternalDownloadState::Downloading),
        );
        let wanted = ids(&["done", "bad", "stopped", "busy", "not-yet-inserted"]);
        assert_eq!(tally_downloads(&active, &wanted), (1, 2));
        // Entries for other ids are ignored.
        assert_eq!(tally_downloads(&active, &ids(&["done"])), (1, 0));
    }

    #[test]
    fn validated_playlist_name_sanitises_and_rejects_empty() {
        let (sanitized, name) = validated_playlist_name("Friday Set").unwrap();
        assert_eq!(sanitized, "Friday-Set");
        assert_eq!(name.as_str(), "Friday-Set");
        assert!(validated_playlist_name("!!!").is_err());
    }

    #[tokio::test]
    async fn wait_for_downloads_returns_when_all_finished_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let job_id = service.begin_import_job(false).unwrap();
        // "a" is still absent from active_downloads (between pop_front and insert).
        service.active_downloads.lock().insert(
            "b".to_string(),
            queued_download("b", InternalDownloadState::Failed),
        );

        let finisher = Arc::clone(&service);
        let finish = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            finisher.active_downloads.lock().insert(
                "a".to_string(),
                queued_download("a", InternalDownloadState::Downloading),
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            finisher.active_downloads.lock().get_mut("a").unwrap().state =
                InternalDownloadState::Completed;
        });

        let outcome = service
            .wait_for_downloads(
                job_id,
                &ids(&["a", "b"]),
                Duration::from_millis(10),
                Duration::from_secs(10),
            )
            .await;
        finish.await.unwrap();
        assert_eq!(outcome, SyncWaitEnd::AllFinished);
        assert_eq!(
            import_status(&service).sync,
            Some(ImportSyncProgress {
                queued: 2,
                completed: 1,
                failed: 1,
                ended: Some(SyncWaitEnd::AllFinished)
            })
        );
    }

    #[tokio::test]
    async fn wait_for_downloads_gives_up_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let job_id = service.begin_import_job(false).unwrap();
        service.active_downloads.lock().insert(
            "a".to_string(),
            queued_download("a", InternalDownloadState::Downloading),
        );

        let outcome = service
            .wait_for_downloads(
                job_id,
                &ids(&["a", "never"]),
                Duration::from_millis(10),
                Duration::from_millis(50),
            )
            .await;
        assert_eq!(outcome, SyncWaitEnd::TimedOut);
        assert_eq!(
            import_status(&service).sync,
            Some(ImportSyncProgress {
                queued: 2,
                completed: 0,
                failed: 0,
                ended: Some(SyncWaitEnd::TimedOut)
            })
        );
    }

    #[tokio::test]
    async fn wait_for_downloads_with_nothing_queued_returns_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let job_id = service.begin_import_job(false).unwrap();
        service
            .wait_for_downloads(
                job_id,
                &[],
                Duration::from_secs(60),
                Duration::from_secs(60),
            )
            .await;
        assert_eq!(
            import_status(&service).sync,
            Some(ImportSyncProgress {
                queued: 0,
                completed: 0,
                failed: 0,
                ended: Some(SyncWaitEnd::AllFinished)
            })
        );
    }

    #[tokio::test]
    async fn wait_for_downloads_ends_at_once_when_downloads_are_paused() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let job_id = service.begin_import_job(false).unwrap();
        service.active_downloads.lock().insert(
            "a".to_string(),
            queued_download("a", InternalDownloadState::Downloading),
        );
        service.paused.store(true, Ordering::Relaxed);

        let started = Instant::now();
        let outcome = service
            .wait_for_downloads(
                job_id,
                &ids(&["a"]),
                Duration::from_millis(10),
                Duration::from_secs(60),
            )
            .await;
        assert_eq!(outcome, SyncWaitEnd::Paused);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            import_status(&service).sync,
            Some(ImportSyncProgress {
                queued: 1,
                completed: 0,
                failed: 0,
                ended: Some(SyncWaitEnd::Paused)
            })
        );
    }

    #[tokio::test]
    async fn wait_for_downloads_reports_finished_even_when_paused() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let job_id = service.begin_import_job(false).unwrap();
        service.active_downloads.lock().insert(
            "a".to_string(),
            queued_download("a", InternalDownloadState::Completed),
        );
        service.paused.store(true, Ordering::Relaxed);
        let outcome = service
            .wait_for_downloads(
                job_id,
                &ids(&["a"]),
                Duration::from_millis(10),
                Duration::from_secs(60),
            )
            .await;
        assert_eq!(outcome, SyncWaitEnd::AllFinished);
    }

    #[test]
    fn cancelling_a_queued_item_records_a_terminal_cancelled_entry() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        service
            .download_queue
            .lock()
            .push_back(queued_download("q", InternalDownloadState::Queued));
        service
            .download_queue
            .lock()
            .push_back(queued_download("other", InternalDownloadState::Queued));

        service.cancel_download("q");

        let queue = service.download_queue.lock();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].item.id.as_str(), "other");
        let active = service.active_downloads.lock();
        assert!(active["q"].state == InternalDownloadState::Cancelled);
        // A wait on the cancelled item ends: it counts as failed.
        assert_eq!(tally_downloads(&active, &ids(&["q"])), (0, 1));
    }

    #[test]
    fn cancelling_an_active_item_marks_it_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        service.active_downloads.lock().insert(
            "a".to_string(),
            queued_download("a", InternalDownloadState::Downloading),
        );
        service.cancel_download("a");
        assert!(service.active_downloads.lock()["a"].state == InternalDownloadState::Cancelled);
    }

    #[test]
    fn cancelling_a_finished_item_leaves_it_finished() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        service.active_downloads.lock().insert(
            "a".to_string(),
            queued_download("a", InternalDownloadState::Completed),
        );
        service.cancel_download("a");
        assert!(service.active_downloads.lock()["a"].state == InternalDownloadState::Completed);
    }

    #[test]
    fn enqueue_queues_new_items_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let (wanted, newly) = service.enqueue_new_items(vec![
            make_collection_item("a", "Artist", "One"),
            make_collection_item("b", "Artist", "Two"),
        ]);
        assert_eq!(wanted, ids(&["a", "b"]));
        assert_eq!(newly, 2);
        assert_eq!(service.download_queue.lock().len(), 2);
    }

    #[test]
    fn enqueue_skips_in_flight_items_but_still_returns_their_ids() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        service
            .download_queue
            .lock()
            .push_back(queued_download("queued", InternalDownloadState::Queued));
        service.active_downloads.lock().insert(
            "busy".to_string(),
            queued_download("busy", InternalDownloadState::Downloading),
        );

        let (wanted, newly) = service.enqueue_new_items(vec![
            make_collection_item("queued", "Artist", "One"),
            make_collection_item("busy", "Artist", "Two"),
            make_collection_item("fresh", "Artist", "Three"),
        ]);

        assert_eq!(wanted, ids(&["queued", "busy", "fresh"]));
        assert_eq!(newly, 1, "only 'fresh' is new");
        let queue = service.download_queue.lock();
        let queued_ids: Vec<&str> = queue.iter().map(|d| d.item.id.as_str()).collect();
        assert_eq!(queued_ids, vec!["queued", "fresh"], "no duplicates queued");
        assert!(
            service.active_downloads.lock()["busy"].state == InternalDownloadState::Downloading
        );
    }

    #[test]
    fn enqueue_replaces_finished_entries_from_earlier_runs() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        for (id, state) in [
            ("failed", InternalDownloadState::Failed),
            ("stopped", InternalDownloadState::Cancelled),
        ] {
            service
                .active_downloads
                .lock()
                .insert(id.to_string(), queued_download(id, state));
        }

        let (wanted, newly) = service.enqueue_new_items(vec![
            make_collection_item("failed", "Artist", "One"),
            make_collection_item("stopped", "Artist", "Two"),
        ]);

        assert_eq!(wanted, ids(&["failed", "stopped"]));
        assert_eq!(newly, 2);
        let active = service.active_downloads.lock();
        assert!(!active.contains_key("failed") && !active.contains_key("stopped"));
        // Nothing waiting on them can mistake the old outcome for the new one.
        assert_eq!(tally_downloads(&active, &wanted), (0, 0));
    }

    #[test]
    fn stale_job_cannot_update_a_newer_jobs_status() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let old = service.begin_import_job(false).unwrap();
        service.fail_import(old, "first job failed".to_string());
        let new = service.begin_import_job(false).unwrap();
        assert_ne!(old, new);

        service.set_import_phase(old, ImportPhase::Writing);
        service.update_import(old, |s| s.playlist_title = Some("stale".to_string()));
        service.fail_import(old, "late failure".to_string());

        let status = import_status(&service);
        assert_eq!(status.job_id, new);
        assert_eq!(status.phase, ImportPhase::Fetching);
        assert_eq!(status.playlist_title, None);

        service.set_import_phase(new, ImportPhase::Matching);
        assert_eq!(import_status(&service).phase, ImportPhase::Matching);
    }

    #[tokio::test]
    async fn join_error_says_panicked_only_for_a_panic() {
        let panicking = tokio::spawn(async { panic!("boom") });
        let e = panicking.await.unwrap_err();
        let message = join_error_message(&e);
        assert!(message.contains("panicked"), "got: {message}");

        let hanging = tokio::spawn(std::future::pending::<()>());
        hanging.abort();
        let e = hanging.await.unwrap_err();
        let message = join_error_message(&e);
        assert!(message.contains("cancelled"), "got: {message}");
        assert!(!message.contains("panicked"), "got: {message}");
    }

    fn import_request(name: Option<&str>) -> SourceRequest {
        SourceRequest::ImportPlaylist {
            playlist: "my-set".to_string(),
            name: name.map(str::to_string),
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn import_with_bad_name_is_rejected_without_starting_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        match service.handle_request(import_request(Some("???"))).await {
            SourceResponse::Error(SourceError::InvalidRequest { message }) => {
                assert!(message.contains("invalid playlist name"), "got: {message}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
        assert!(service.import_status.lock().is_none());
    }

    #[tokio::test]
    async fn import_while_one_is_running_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let running = service.begin_import_job(false).unwrap();
        match service.handle_request(import_request(None)).await {
            SourceResponse::Error(SourceError::ImportInProgress { job_id }) => {
                assert_eq!(job_id, running);
            }
            other => panic!("expected ImportInProgress, got {other:?}"),
        }
        // The running job's status is untouched.
        assert_eq!(import_status(&service).job_id, running);
        assert_eq!(import_status(&service).phase, ImportPhase::Fetching);
    }

    #[tokio::test]
    async fn import_without_cookies_ends_failed_and_allows_a_new_job() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        let job_id = match service.handle_request(import_request(None)).await {
            SourceResponse::ImportStarted { job_id } => job_id,
            other => panic!("expected ImportStarted, got {other:?}"),
        };
        let status = wait_for_import_end(&service).await;
        assert_eq!(status.job_id, job_id);
        match status.phase {
            ImportPhase::Failed { message } => {
                assert!(message.contains("not configured"), "got: {message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        match service.handle_request(SourceRequest::GetImportStatus).await {
            SourceResponse::ImportStatus(Some(s)) => assert_eq!(s.job_id, job_id),
            other => panic!("expected ImportStatus, got {other:?}"),
        }
        assert_eq!(service.begin_import_job(false), Ok(job_id + 1));
    }

    #[tokio::test]
    async fn import_status_is_none_before_any_job() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        assert!(matches!(
            service.handle_request(SourceRequest::GetImportStatus).await,
            SourceResponse::ImportStatus(None)
        ));
    }

    /// Poll the status until the job is Done or Failed.
    async fn wait_for_import_end(service: &Arc<BandcampService>) -> ImportJobStatus {
        for _ in 0..500 {
            let status = import_status(service);
            if !import_phase_is_running(&status.phase) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("import job did not finish");
    }

    /// Regression guard for #70: a missing cookie file must not make startup fail.
    #[test]
    fn new_succeeds_without_cookie_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let result = BandcampService::new(
            missing,
            dir.path().join("downloads"),
            dir.path().join("inbox"),
            dir.path().join("cache"),
            AudioFormat::Flac,
            "ipc:///nonexistent/library.sock".to_string(),
            None,
        );
        let service = result.expect("missing cookie file must not fail startup");
        assert!(matches!(
            service.cookie_state(),
            CookieState::NotConfigured { .. }
        ));
    }

    #[test]
    fn startup_with_invalid_cookie_file_is_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.json");
        std::fs::write(&path, "not json").unwrap();
        let service = make_service(dir.path(), path);
        assert!(matches!(
            service.cookie_state(),
            CookieState::NotConfigured { .. }
        ));
    }

    #[rstest::rstest]
    #[case::list_playlists(SourceRequest::ListPlaylists)]
    #[case::get_playlist(SourceRequest::GetPlaylist { playlist: "x".to_string() })]
    #[tokio::test]
    async fn request_without_cookies_returns_not_configured_error(#[case] request: SourceRequest) {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        match service.handle_request(request).await {
            SourceResponse::Error(SourceError::NotAuthenticated { message }) => {
                assert!(message.contains("not configured"), "got: {message}");
            }
            other => panic!("expected NotAuthenticated error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_without_cookies_reports_not_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        let service = make_service(dir.path(), dir.path().join("missing.json"));
        match service.handle_request(SourceRequest::GetStatus).await {
            SourceResponse::Status(status) => {
                assert_eq!(status.auth, source_protocol::AuthStatus::NotAuthenticated);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cookie_upload_after_startup_becomes_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.json");
        let service = make_service(dir.path(), path.clone());
        assert!(matches!(
            service.cookie_state(),
            CookieState::NotConfigured { .. }
        ));

        std::fs::write(&path, VALID_COOKIES).unwrap();

        assert_eq!(service.cookie_state(), CookieState::Configured);
        match service.handle_request(SourceRequest::GetStatus).await {
            SourceResponse::Status(status) => {
                assert_eq!(status.auth, source_protocol::AuthStatus::Authenticated);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    // =========================================================================
    // Integration test: work-identity dedup via IPC stub
    // =========================================================================

    #[cfg(test)]
    mod integration {
        use super::*;
        use library_service_stub::service::{run_ipc_server, LibraryService};
        use music_facts::{
            Album, Artist, ContentHash as MusicContentHash, MusicValue as MV, Title,
        };
        use stainless_facts::{Fact, FactStreamWriter, Operation};
        use std::sync::Arc;

        /// Seed a single track with artist, album, and ItemId facts into a facts.jsonl file.
        fn seed_track_with_item_id(
            metadata_dir: &std::path::Path,
            hash_hex: &str,
            artist: &str,
            album: &str,
            item_id: &str,
        ) {
            let facts_path = metadata_dir.join("facts.jsonl");
            let mut writer =
                FactStreamWriter::open(&facts_path).expect("failed to open fact stream");

            let hash = MusicContentHash::new(format!("sha256:{}", hash_hex));
            let source = FactSource::new("test-seed", "0.0.0", "Unknown");
            let now = chrono::Utc::now();

            let facts: Vec<Fact<MusicContentHash, MV, FactSource>> = vec![
                Fact::new(
                    hash.clone(),
                    MV::Title(Title::new(album)),
                    now,
                    source.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MV::Artist(Artist::new(artist)),
                    now,
                    source.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MV::Album(Album::new(album)),
                    now,
                    source.clone(),
                    Operation::Assert,
                ),
                Fact::new(
                    hash.clone(),
                    MV::ItemId(item_id.to_string()),
                    now,
                    source.clone(),
                    Operation::Assert,
                ),
            ];
            writer.write_batch(&facts).expect("failed to write facts");
        }

        /// Spin up a stub library IPC server on a temp socket and return the address.
        fn start_stub_library(
            metadata_dir: &std::path::Path,
            socket_path: &std::path::Path,
        ) -> std::thread::JoinHandle<()> {
            let svc = LibraryService::from_metadata_dir(metadata_dir.to_path_buf());
            let addr = format!("ipc://{}", socket_path.display());
            std::thread::spawn(move || {
                run_ipc_server(svc, &addr).ok();
            })
        }

        #[test]
        fn work_identity_dedup_skips_duplicate_purchase_and_backfills_item_id() {
            let temp = tempfile::TempDir::new().expect("tempdir");
            let metadata_dir = temp.path().to_path_buf();
            let socket_path = temp.path().join("library.sock");

            // Seed: track with ItemId="p_vinyl_111"
            seed_track_with_item_id(
                &metadata_dir,
                &format!("{:0<64}", "deadbeef"),
                "Carbon Based Lifeforms",
                "Interloper",
                "p_vinyl_111",
            );

            // Start stub server
            let _srv = start_stub_library(&metadata_dir, &socket_path);
            // Give the thread time to bind
            std::thread::sleep(std::time::Duration::from_millis(100));

            let lib_socket = format!("ipc://{}", socket_path.display());

            // Verify the stub has the seeded track via ItemId
            let client = LibraryClient::connect(&lib_socket).expect("connect");
            let known = client
                .has_facts("ItemId", vec!["p_vinyl_111".to_string()])
                .expect("has_facts");
            assert!(
                known.contains(&"p_vinyl_111".to_string()),
                "seed sanity check"
            );

            // Build collection: p_vinyl_111 (already known by ItemId) + p_digital_222 (same work)
            let vinyl_item =
                make_collection_item("p_vinyl_111", "Carbon Based Lifeforms", "Interloper");
            let digital_item =
                make_collection_item("p_digital_222", "Carbon Based Lifeforms", "Interloper");
            let collection = [vinyl_item, digital_item];

            // Collect all ItemIds from collection for has_facts call (mirrors handle_sync logic)
            let all_ids: Vec<String> = collection
                .iter()
                .map(|i| i.id.as_str().to_string())
                .collect();
            let library_known: std::collections::HashSet<String> = client
                .has_facts("ItemId", all_ids)
                .expect("has_facts batch")
                .into_iter()
                .collect();

            // p_vinyl_111 is in library_known; p_digital_222 is not
            assert!(
                library_known.contains("p_vinyl_111"),
                "vinyl known via ItemId"
            );
            assert!(
                !library_known.contains("p_digital_222"),
                "digital NOT known by ItemId yet"
            );

            // For p_digital_222: perform the work-identity search
            let digital_item2 =
                make_collection_item("p_digital_222", "Carbon Based Lifeforms", "Interloper");
            let query = TrackQuery {
                artist: Some(StringQuery::Contains(normalize(
                    digital_item2.artist.as_str(),
                ))),
                album: Some(StringQuery::Contains(normalize(
                    digital_item2.title.as_str(),
                ))),
                ..Default::default()
            };
            let results = client.search(&query).expect("search");
            assert!(
                should_skip_by_work_identity(&digital_item2, &results),
                "work-identity check should fire: Interloper already in library"
            );

            // Backfill: write ItemId="p_digital_222" onto the matched track
            let matched_hash = results.first().unwrap().content_hash.clone();
            client
                .write_fact_as(
                    &matched_hash,
                    MusicValue::ItemId("p_digital_222".to_string()),
                    &bandcamp_source(),
                )
                .expect("write_fact_as");

            // Verify backfill: now p_digital_222 is known by ItemId
            let after_backfill = client
                .has_facts("ItemId", vec!["p_digital_222".to_string()])
                .expect("has_facts after backfill");
            assert!(
                after_backfill.contains(&"p_digital_222".to_string()),
                "after backfill, p_digital_222 should be found by ItemId"
            );
        }

        fn source_track(
            position: u32,
            artist: &str,
            title: &str,
            owned: bool,
        ) -> SourcePlaylistTrack {
            SourcePlaylistTrack {
                position,
                source_track_id: format!("t{position}"),
                title: title.to_string(),
                artist: artist.to_string(),
                album: None,
                duration_secs: 0.0,
                owned,
            }
        }

        fn source_playlist(tracks: Vec<SourcePlaylistTrack>) -> SourcePlaylist {
            SourcePlaylist {
                id: "1".to_string(),
                title: "My Set".to_string(),
                url: "https://bandcamp.com/someone/playlist/my-set".to_string(),
                total_count: tracks.len() as u32,
                tracks,
            }
        }

        /// A service wired to a stub library holding one track,
        /// "Carbon Based Lifeforms - Interloper". Keeps the tempdir alive.
        fn service_with_library(
            socket_name: &str,
        ) -> (tempfile::TempDir, Arc<BandcampService>, LibraryClient) {
            let temp = tempfile::TempDir::new().expect("tempdir");
            seed_track_with_item_id(
                temp.path(),
                &format!("{:0<64}", "deadbeef"),
                "Carbon Based Lifeforms",
                "Interloper",
                "p_1",
            );
            let socket_path = temp.path().join(socket_name);
            let _srv = start_stub_library(temp.path(), &socket_path);
            std::thread::sleep(std::time::Duration::from_millis(100));
            let lib_socket = format!("ipc://{}", socket_path.display());
            let service = make_service_with_library(
                temp.path(),
                temp.path().join("missing.json"),
                &lib_socket,
            );
            let client = LibraryClient::connect(&lib_socket).expect("connect");
            (temp, service, client)
        }

        /// With the library up, a request that needs Bandcamp but has no
        /// cookies is NotAuthenticated (the library is asked first).
        #[rstest::rstest]
        #[case::sync(SourceRequest::Sync)]
        #[case::resync(SourceRequest::ResyncItem { identifier: "x".to_string() })]
        #[case::check(SourceRequest::CheckItem { identifier: "x".to_string() })]
        #[tokio::test]
        async fn request_without_cookies_and_library_up_is_not_authenticated(
            #[case] request: SourceRequest,
        ) {
            let (_temp, service, _client) = service_with_library("nocookies.sock");
            match service.handle_request(request).await {
                SourceResponse::Error(SourceError::NotAuthenticated { message }) => {
                    assert!(message.contains("not configured"), "got: {message}");
                }
                other => panic!("expected NotAuthenticated error, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn import_with_nothing_missing_writes_playlist_and_finishes_done() {
            let (_temp, service, client) = service_with_library("import-happy.sock");
            let job_id = service.begin_import_job(false).unwrap();
            let playlist = source_playlist(vec![source_track(
                1,
                "Carbon Based Lifeforms",
                "Interloper",
                true,
            )]);

            service
                .run_import(job_id, playlist, "my-set", None, false)
                .await;

            let status = import_status(&service);
            assert_eq!(status.job_id, job_id);
            assert_eq!(status.playlist_title.as_deref(), Some("My Set"));
            assert_eq!(status.target_name.as_deref(), Some("My-Set"));
            assert_eq!(status.total_count, Some(1));
            assert_eq!(status.sync, None, "nothing missing: no sync");
            assert_eq!(
                status.phase,
                ImportPhase::Done(ImportResult::Written(ImportWritten {
                    created: true,
                    tracks: 1
                }))
            );
            assert!(matches!(
                status.tracks[0].outcome,
                source_protocol::ImportTrackOutcome::Matched { .. }
            ));
            let written = client
                .playlist_get(&PlaylistName::new("My-Set").unwrap())
                .expect("playlist was written");
            assert_eq!(written.len(), 1);
        }

        #[tokio::test]
        async fn import_with_explicit_name_replaces_an_existing_playlist() {
            let (_temp, service, client) = service_with_library("import-replace.sock");
            let name = PlaylistName::new("mine").unwrap();
            client
                .playlist_new(&name, "old".to_string())
                .expect("seed playlist");
            let job_id = service.begin_import_job(false).unwrap();
            let playlist = source_playlist(vec![source_track(
                1,
                "Carbon Based Lifeforms",
                "Interloper",
                true,
            )]);

            service
                .run_import(
                    job_id,
                    playlist,
                    "my-set",
                    Some(validated_playlist_name("mine").unwrap()),
                    false,
                )
                .await;

            let status = import_status(&service);
            assert_eq!(status.target_name.as_deref(), Some("mine"));
            assert_eq!(
                status.phase,
                ImportPhase::Done(ImportResult::Written(ImportWritten {
                    created: false,
                    tracks: 1
                }))
            );
        }

        #[tokio::test]
        async fn dry_run_reports_matches_but_never_syncs_or_writes() {
            let (_temp, service, client) = service_with_library("import-dry.sock");
            let job_id = service.begin_import_job(true).unwrap();
            // The second track is owned but missing: a real run would sync.
            let playlist = source_playlist(vec![
                source_track(1, "Carbon Based Lifeforms", "Interloper", true),
                source_track(2, "Nobody", "Nothing", true),
            ]);

            service
                .run_import(job_id, playlist, "my-set", None, true)
                .await;

            let status = import_status(&service);
            assert_eq!(
                status.phase,
                ImportPhase::Done(ImportResult::DryRun { would_write: 1 })
            );
            assert!(status.dry_run);
            assert_eq!(status.sync, None);
            assert_eq!(status.tracks.len(), 2);
            assert_eq!(
                status.tracks[1].outcome,
                source_protocol::ImportTrackOutcome::Missing { not_owned: false }
            );
            assert!(
                client
                    .playlist_get(&PlaylistName::new("My-Set").unwrap())
                    .is_err(),
                "dry run must not write"
            );
        }

        #[tokio::test]
        async fn dry_run_with_no_matches_reports_nothing_matched_not_zero_tracks() {
            let (_temp, service, _client) = service_with_library("import-dry-none.sock");
            let job_id = service.begin_import_job(true).unwrap();
            let playlist = source_playlist(vec![source_track(1, "Nobody", "Nothing", true)]);

            service
                .run_import(job_id, playlist, "my-set", None, true)
                .await;

            let status = import_status(&service);
            assert_eq!(
                status.phase,
                ImportPhase::Done(ImportResult::NothingMatched)
            );
            assert!(status.dry_run);
        }

        #[test]
        fn drop_items_known_to_library_keeps_only_unknown_items_and_backfills() {
            // The stub is seeded with ItemId "p_1" for the Interloper track. It is
            // built here (not via service_with_library) to keep a handle that
            // records the source of each WriteFact.
            let temp = tempfile::TempDir::new().expect("tempdir");
            let seeded_hash = format!("{:0<64}", "deadbeef");
            seed_track_with_item_id(
                temp.path(),
                &seeded_hash,
                "Carbon Based Lifeforms",
                "Interloper",
                "p_1",
            );
            let stub = LibraryService::from_metadata_dir(temp.path().to_path_buf());
            let socket_path = temp.path().join("drop-known.sock");
            let socket = format!("ipc://{}", socket_path.display());
            let _srv = {
                let stub = Arc::clone(&stub);
                let addr = socket.clone();
                std::thread::spawn(move || {
                    run_ipc_server(stub, &addr).ok();
                })
            };
            std::thread::sleep(std::time::Duration::from_millis(100));
            let client = LibraryClient::connect(&socket).expect("connect");
            let items = vec![
                make_collection_item("p_1", "Carbon Based Lifeforms", "Interloper"),
                make_collection_item("p_2", "Carbon Based Lifeforms", "Interloper"),
                make_collection_item("p_3", "Somebody Else", "Other Album"),
            ];

            let unknown = drop_items_known_to_library(&socket, items).expect("library is up");

            let ids: Vec<&str> = unknown.iter().map(|i| i.id.as_str()).collect();
            assert_eq!(ids, vec!["p_3"]);
            let known = client
                .has_facts("ItemId", vec!["p_2".to_string()])
                .expect("has_facts");
            assert_eq!(known, vec!["p_2".to_string()], "ItemId backfilled");
            // The backfill states mdma-bandcamp/Bandcamp, the source of an ingest.
            assert_eq!(
                stub.written_source(
                    &MusicContentHash::new(format!("sha256:{seeded_hash}")),
                    &MV::ItemId("p_2".to_string()),
                ),
                Some(Some(bandcamp_source()))
            );
            assert_eq!(bandcamp_source().tool, "mdma-bandcamp");
            assert_eq!(bandcamp_source().origin.kind, "Bandcamp");
        }

        #[test]
        fn drop_items_known_to_library_defers_when_library_is_down() {
            let items = vec![make_collection_item("p_1", "A", "B")];
            let result = drop_items_known_to_library("ipc:///nonexistent/library.sock", items);
            assert!(result.is_err(), "unreachable library must defer");
        }

        /// A library that accepts connections but answers every request with
        /// a protocol error. Returns its address; the server thread is leaked.
        fn start_failing_library(dir: &std::path::Path) -> String {
            let addr = format!("ipc://{}", dir.join("failing.sock").display());
            let socket = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
            socket.listen(&addr).expect("listen");
            std::thread::spawn(move || {
                while socket.recv().is_ok() {
                    let reply = serde_json::to_vec(&library_ipc_client::LibraryResponse::Error(
                        library_ipc_client::ProtocolError::Internal {
                            message: "boom".to_string(),
                        },
                    ))
                    .expect("serialize");
                    if socket.send(nng::Message::from(&reply[..])).is_err() {
                        break;
                    }
                }
            });
            addr
        }

        /// A library that relays every request to the real one at `upstream`
        /// except `Ping`, which it answers with a protocol error.
        fn start_pingless_library(dir: &std::path::Path, upstream: &str) -> String {
            let addr = format!("ipc://{}", dir.join("pingless.sock").display());
            let socket = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
            socket.listen(&addr).expect("listen");
            let relay = nng::Socket::new(nng::Protocol::Req0).expect("req socket");
            relay.dial(upstream).expect("dial upstream");
            std::thread::spawn(move || {
                while let Ok(request) = socket.recv() {
                    let is_ping = matches!(
                        serde_json::from_slice(&request),
                        Ok(library_ipc_client::LibraryRequest::Ping)
                    );
                    let reply = if is_ping {
                        let error = library_ipc_client::LibraryResponse::Error(
                            library_ipc_client::ProtocolError::Internal {
                                message: "no ping".to_string(),
                            },
                        );
                        nng::Message::from(&serde_json::to_vec(&error).expect("serialize")[..])
                    } else {
                        relay
                            .send(request)
                            .map_err(|(_, e)| e)
                            .expect("relay request");
                        relay.recv().expect("relay reply")
                    };
                    if socket.send(reply).is_err() {
                        break;
                    }
                }
            });
            addr
        }

        fn cached_service(library_socket: &str) -> (tempfile::TempDir, Arc<BandcampService>) {
            let temp = tempfile::TempDir::new().expect("tempdir");
            let service = make_service_with_library(
                temp.path(),
                temp.path().join("missing.json"),
                library_socket,
            );
            service
                .cache
                .lock()
                .mark_downloaded("key", "p_9")
                .expect("seed cache");
            (temp, service)
        }

        fn assert_forced_resync_refused(service: &Arc<BandcampService>) {
            let err = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(
                    service.queue_item_forcibly(make_collection_item("p_9", "Artist", "Album")),
                )
                .expect_err("resync must fail");
            assert!(
                matches!(err, SourceError::SyncFailed { .. }),
                "unexpected error: {err:?}"
            );
            assert!(service.download_queue.lock().is_empty(), "nothing queued");
            assert!(
                service.cache.lock().is_item_downloaded("p_9"),
                "cache entry intact"
            );
        }

        #[test]
        fn forced_resync_with_library_unreachable_errors_and_changes_nothing() {
            let (_temp, service) = cached_service("ipc:///nonexistent/library.sock");
            assert_forced_resync_refused(&service);
        }

        #[test]
        fn forced_resync_with_failing_retract_errors_and_changes_nothing() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let addr = start_failing_library(dir.path());
            let (_temp, service) = cached_service(&addr);
            assert_forced_resync_refused(&service);
        }

        #[test]
        fn forced_resync_with_library_up_evicts_cache_and_queues() {
            let (_temp, service, _client) = service_with_library("resync-ok.sock");
            service.cache.lock().mark_downloaded("key", "p_1").unwrap();
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(
                    service.queue_item_forcibly(make_collection_item("p_1", "Artist", "Album")),
                )
                .expect("resync");
            assert_eq!(service.download_queue.lock().len(), 1);
            assert!(!service.cache.lock().is_item_downloaded("p_1"));
        }

        #[tokio::test]
        async fn forced_resync_does_not_queue_an_in_flight_item_twice() {
            let (_temp, service, _client) = service_with_library("resync-inflight.sock");
            service
                .download_queue
                .lock()
                .push_back(queued_download("q", InternalDownloadState::Queued));
            service.active_downloads.lock().insert(
                "busy".to_string(),
                queued_download("busy", InternalDownloadState::Downloading),
            );
            service.active_downloads.lock().insert(
                "old".to_string(),
                queued_download("old", InternalDownloadState::Completed),
            );

            for id in ["q", "busy", "old"] {
                service
                    .queue_item_forcibly(make_collection_item(id, "Artist", "Album"))
                    .await
                    .unwrap();
            }

            let queue = service.download_queue.lock();
            let queued: Vec<&str> = queue.iter().map(|d| d.item.id.as_str()).collect();
            assert_eq!(queued, vec!["q", "old"], "in-flight items not duplicated");
            assert!(!service.active_downloads.lock().contains_key("old"));
            assert!(service.active_downloads.lock().contains_key("busy"));
        }

        #[tokio::test]
        async fn check_item_with_library_down_is_an_error_not_stale() {
            // No cookies either: the library is asked before Bandcamp is.
            let dir = tempfile::TempDir::new().expect("tempdir");
            let service = make_service(dir.path(), dir.path().join("missing.json"));

            let response = service.handle_check_item("p_1".to_string()).await;

            match response {
                SourceResponse::Error(SourceError::CheckFailed { identifier, reason }) => {
                    assert_eq!(identifier, "p_1");
                    assert!(reason.contains("library unavailable"), "reason: {reason}");
                }
                _ => panic!("expected CheckFailed"),
            }
        }

        #[tokio::test]
        async fn resync_with_library_down_is_refused_before_bandcamp_is_asked() {
            // No cookies: reaching the Bandcamp client would be NotAuthenticated.
            let dir = tempfile::TempDir::new().expect("tempdir");
            let service = make_service(dir.path(), dir.path().join("missing.json"));

            let response = service.handle_resync("p_1".to_string()).await;

            match response {
                SourceResponse::Error(SourceError::SyncFailed { message }) => {
                    assert!(
                        message.contains("library unavailable"),
                        "message: {message}"
                    )
                }
                _ => panic!("expected SyncFailed"),
            }
        }

        #[tokio::test]
        async fn import_with_sync_deferred_writes_first_pass_matches_and_finishes() {
            let (temp, upstream, client) = service_with_library("import-upstream.sock");
            let addr = start_pingless_library(temp.path(), &upstream.library_socket);
            let service =
                make_service_with_library(temp.path(), temp.path().join("missing.json"), &addr);
            let job_id = service.begin_import_job(false).unwrap();
            // The second track is owned but missing, so the import wants a sync.
            let playlist = source_playlist(vec![
                source_track(1, "Carbon Based Lifeforms", "Interloper", true),
                source_track(2, "Nobody", "Nothing", true),
            ]);

            service
                .run_import(job_id, playlist, "my-set", None, false)
                .await;

            let status = import_status(&service);
            assert_eq!(
                status.phase,
                ImportPhase::Done(ImportResult::Written(ImportWritten {
                    created: true,
                    tracks: 1
                }))
            );
            assert_eq!(
                status.sync,
                Some(ImportSyncProgress {
                    queued: 0,
                    completed: 0,
                    failed: 0,
                    ended: None
                }),
                "sync recorded as never waited"
            );
            assert_eq!(
                status.tracks[1].outcome,
                source_protocol::ImportTrackOutcome::Missing { not_owned: false }
            );
            let written = client
                .playlist_get(&PlaylistName::new("My-Set").unwrap())
                .expect("playlist was written");
            assert_eq!(written.len(), 1);
        }

        fn run_sync(
            service: &Arc<BandcampService>,
            items: Vec<CollectionItem>,
        ) -> Result<SyncOutcome, SyncError> {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(service.queue_new_collection_items(items))
        }

        fn assert_sync_deferred(service: &Arc<BandcampService>) {
            let items = vec![make_collection_item("p_7", "Nobody", "Nothing")];
            let err = run_sync(service, items).expect_err("sync must defer");
            match err {
                SyncError::LibraryUnavailable(_) => {
                    let message = err.into_source().to_string();
                    assert!(message.contains("sync deferred"), "message: {message}")
                }
                other => panic!("unexpected error: {other:?}"),
            }
            assert!(service.download_queue.lock().is_empty(), "nothing queued");
        }

        #[test]
        fn sync_with_library_unreachable_defers_and_queues_nothing() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let service = make_service(dir.path(), dir.path().join("missing.json"));
            assert_sync_deferred(&service);
        }

        #[test]
        fn sync_with_failing_has_facts_defers_and_queues_nothing() {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let addr = start_failing_library(dir.path());
            let service = make_service_with_library(dir.path(), dir.path().join("m.json"), &addr);
            assert_sync_deferred(&service);
        }

        #[test]
        fn sync_with_library_up_skips_known_item_ids_and_queues_the_rest() {
            let (_temp, service, _client) = service_with_library("sync-ok.sock");
            let items = vec![
                make_collection_item("p_1", "Carbon Based Lifeforms", "Interloper"),
                make_collection_item("p_3", "Somebody Else", "Other Album"),
            ];
            let outcome = run_sync(&service, items).expect("sync");
            assert_eq!(outcome.total_items, 2);
            assert_eq!(outcome.queued_ids, vec!["p_3".to_string()]);
        }

        #[tokio::test]
        async fn import_with_no_matches_leaves_an_existing_playlist_untouched() {
            let (_temp, service, client) = service_with_library("import-nomatch.sock");
            let name = PlaylistName::new("mine").unwrap();
            // Unowned and absent from the library: no sync, no match.
            let playlist = source_playlist(vec![source_track(1, "Nobody", "Nothing", false)]);
            client
                .playlist_new(&name, "aaaaaaaaaaaa  Old - Track  [1:00]\n".to_string())
                .expect("seed playlist");
            let before = client.playlist_get(&name).expect("seeded");
            let job_id = service.begin_import_job(false).unwrap();

            service
                .run_import(
                    job_id,
                    playlist,
                    "my-set",
                    Some(validated_playlist_name("mine").unwrap()),
                    false,
                )
                .await;

            let status = import_status(&service);
            assert_eq!(
                status.phase,
                ImportPhase::Done(ImportResult::NothingMatched)
            );
            assert_eq!(client.playlist_get(&name).unwrap(), before);
        }

        #[tokio::test]
        async fn import_fails_and_writes_nothing_when_the_library_is_unavailable() {
            let temp = tempfile::TempDir::new().expect("tempdir");
            let service = make_service(temp.path(), temp.path().join("missing.json"));
            let job_id = service.begin_import_job(false).unwrap();
            let playlist = source_playlist(vec![source_track(1, "A", "One", true)]);

            service
                .run_import(job_id, playlist, "my-set", None, false)
                .await;

            let status = import_status(&service);
            match status.phase {
                ImportPhase::Failed { message } => {
                    assert!(message.contains("library"), "got: {message}")
                }
                other => panic!("expected Failed, got {other:?}"),
            }
        }

        /// Adversarial test: a Contains-only probe returns a result whose artist/album only
        /// substring-match the query. The tightened predicate must reject it and NOT skip.
        ///
        /// Seeded track: artist="The Flashbulb", album="Arboreal"
        /// Collection item: artist="The", album="Arbor"
        ///
        /// Contains("the") matches stored artist "The Flashbulb" (word present).
        /// Contains("arbor") matches stored album "Arboreal" (substring present).
        /// So the Contains probe returns the seeded track.
        /// But normalize("the") != normalize("the flashbulb") and
        /// normalize("arbor") != normalize("arboreal"), so `should_skip_by_work_identity`
        /// must return false.
        #[test]
        fn work_identity_dedup_does_not_skip_when_only_contains_match() {
            let temp = tempfile::TempDir::new().expect("tempdir");
            let metadata_dir = temp.path().to_path_buf();
            let socket_path = temp.path().join("library2.sock");

            // Seed: "The Flashbulb" / "Arboreal"
            seed_track_with_item_id(
                &metadata_dir,
                &format!("{:0<64}", "badf00d"),
                "The Flashbulb",
                "Arboreal",
                "p_flashbulb_111",
            );

            let _srv = start_stub_library(&metadata_dir, &socket_path);
            std::thread::sleep(std::time::Duration::from_millis(100));

            let lib_socket = format!("ipc://{}", socket_path.display());
            let client = LibraryClient::connect(&lib_socket).expect("connect");

            // Collection item: "The" / "Arbor" — substring match only
            // Contains("the") matches "The Flashbulb"; Contains("arbor") matches "Arboreal"
            let item = make_collection_item("p_flashbulb_222", "The", "Arbor");
            let query = TrackQuery {
                artist: Some(StringQuery::Contains(normalize(item.artist.as_str()))),
                album: Some(StringQuery::Contains(normalize(item.title.as_str()))),
                ..Default::default()
            };
            let results = client.search(&query).expect("search");

            // Sanity: the Contains probe should return the seeded track
            assert!(
                !results.is_empty(),
                "Contains probe should return the seeded track (test setup check)"
            );

            // The tightened predicate must reject it: no exact normalized match
            assert!(
                !should_skip_by_work_identity(&item, &results),
                "substring-only match must NOT trigger skip"
            );
        }
    }
}
