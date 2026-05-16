//! Bandcamp download service
//!
//! Handles collection syncing and download management.
//! Uses async throughout with NNG IPC bridged via channels.

use crate::cache::DownloadCache;
use crate::ipc::{
    DownloadId, DownloadState, DownloadStatus, IpcServer, SourceError, SourceRequest,
    SourceResponse, SourceStatus,
};
use bandcamp_api::{AudioFormat, BandcampClient, CollectionItem};
use library_ipc_client::{
    InboxPath, IngestSource, LibraryClient, MusicValue, TrackInfo, TrackQuery,
};
use library_search::StringQuery;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
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
    /// Whether cookies are currently loaded
    cookies_loaded: AtomicBool,
    /// Library service socket address for auto-ingest
    library_socket: String,
    /// Bandcamp username (read from cookies/config)
    username: Option<String>,
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

/// Pure helper: determine whether a bandcamp item is stale based on track counts.
///
/// `stale` means `live_count > stored_count` — bandcamp has more tracks than we have.
/// Same count is fine. Fewer live tracks than stored is unusual (bandcamp removed tracks)
/// and is NOT flagged as stale here — the caller may wish to log a warning separately.
fn classify_check_result(live_count: usize, stored_count: usize) -> bool {
    live_count > stored_count
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

        // Check if cookies exist
        let cookies_exist = cookie_path.exists();

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
            cookies_loaded: AtomicBool::new(cookies_exist),
            library_socket,
            username,
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

    /// Try to load the Bandcamp client from cookies
    fn try_load_client(&self) -> Option<BandcampClient> {
        match bandcamp_api::load_cookies(&self.cookie_path) {
            Ok(jar) => {
                tracing::info!(path = %self.cookie_path.display(), "Loaded cookies");
                self.cookies_loaded.store(true, Ordering::Relaxed);
                Some(BandcampClient::new(jar))
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %self.cookie_path.display(), "Failed to load cookies");
                self.cookies_loaded.store(false, Ordering::Relaxed);
                None
            }
        }
    }

    /// Handle a request asynchronously
    pub async fn handle_request(&self, request: SourceRequest) -> SourceResponse {
        tracing::debug!(?request, "Handling request");

        match request {
            SourceRequest::Ping => SourceResponse::Pong,

            SourceRequest::GetStatus => {
                let status = SourceStatus {
                    name: "bandcamp".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    auth: if self.cookies_loaded.load(Ordering::Relaxed) {
                        source_protocol::AuthStatus::Authenticated
                    } else {
                        source_protocol::AuthStatus::NotAuthenticated
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
        }
    }

    /// Force-queue an item, bypassing library and cache dedup.
    ///
    /// Retracts existing library facts and evicts the cache entry (best-effort),
    /// then pushes the item onto the download queue unconditionally.
    async fn queue_item_forcibly(&self, item: CollectionItem) -> Result<(), ServiceError> {
        // Best-effort: retract library facts
        if let Some(lib_client) = self.try_library_client() {
            match lib_client.retract_source_facts(item.id.as_str(), "bandcamp") {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        item_id = %item.id.as_str(),
                        "Failed to retract library facts — continuing anyway"
                    );
                }
            }
        }

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

        // Push onto download queue unconditionally
        let queued = QueuedDownload {
            item,
            state: InternalDownloadState::Queued,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        };
        self.download_queue.lock().push_back(queued);
        Ok(())
    }

    /// Handle a forced resync of a specific item by identifier.
    async fn handle_resync(&self, identifier: String) -> SourceResponse {
        let client = match self.try_load_client() {
            Some(c) => c,
            None => {
                return SourceResponse::Error(SourceError::NotAuthenticated {
                    message: "Cookies not loaded. Upload cookies first.".to_string(),
                });
            }
        };

        let username = match &self.username {
            Some(u) => u.clone(),
            None => {
                return SourceResponse::Error(SourceError::NotAuthenticated {
                    message: "No username configured. Set --username or MDMA_BANDCAMP_USERNAME."
                        .to_string(),
                });
            }
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
            return SourceResponse::Error(SourceError::Internal {
                message: e.to_string(),
            });
        }

        SourceResponse::ResyncQueued {
            identifier,
            tracks_queued: 1,
        }
    }

    /// Handle a check-item request: fetch live track count from bandcamp and compare
    /// against the library's stored count for the given item identifier.
    async fn handle_check_item(&self, identifier: String) -> SourceResponse {
        let client = match self.try_load_client() {
            Some(c) => c,
            None => {
                return SourceResponse::Error(SourceError::NotAuthenticated {
                    message: "Cookies not loaded. Upload cookies first.".to_string(),
                });
            }
        };

        let username = match &self.username {
            Some(u) => u.clone(),
            None => {
                return SourceResponse::Error(SourceError::NotAuthenticated {
                    message: "No username configured. Set --username or MDMA_BANDCAMP_USERNAME."
                        .to_string(),
                });
            }
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

        let live_track_count = details.tracks.len();

        // Query library for stored track count
        let stored_track_count = if let Some(lib_client) = self.try_library_client() {
            match lib_client.get_track_count_for_item_id(&identifier) {
                Ok(count) => count,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        item_id = %identifier,
                        "CheckItem: failed to query library track count, defaulting to 0"
                    );
                    0
                }
            }
        } else {
            tracing::warn!(
                item_id = %identifier,
                "CheckItem: library not available, defaulting stored_track_count to 0"
            );
            0
        };

        let stale = classify_check_result(live_track_count, stored_track_count);

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
        // Try to load client (also reloads cookies)
        let client = match self.try_load_client() {
            Some(c) => c,
            None => {
                return SourceResponse::Error(SourceError::NotAuthenticated {
                    message: "Cookies not loaded. Upload cookies first.".to_string(),
                });
            }
        };

        // Get username from config
        let username = match &self.username {
            Some(u) => u.clone(),
            None => {
                return SourceResponse::Error(SourceError::NotAuthenticated {
                    message: "No username configured. Set --username or MDMA_BANDCAMP_USERNAME."
                        .to_string(),
                });
            }
        };

        // Store the username
        *self.current_username.lock() = Some(username.clone());

        tracing::info!(username = %username, "Starting collection sync");

        // Fetch the collection
        let collection = match client.get_collection(&username).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch collection");
                return SourceResponse::Error(SourceError::SyncFailed {
                    message: e.to_string(),
                });
            }
        };

        let total_items = collection.len();
        tracing::info!(total = total_items, "Fetched collection");

        // Check library for already-ingested items (primary dedup)
        let maybe_lib_client = self.try_library_client();
        let library_known: HashSet<String> = match &maybe_lib_client {
            Some(lib_client) => {
                let all_item_ids: Vec<String> = collection
                    .iter()
                    .map(|i| i.id.as_str().to_string())
                    .collect();
                match lib_client.has_facts("ItemId", all_item_ids) {
                    Ok(existing) => {
                        tracing::info!(
                            count = existing.len(),
                            "Library reports items already ingested"
                        );
                        existing.into_iter().collect()
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to query library, falling back to cache only");
                        HashSet::new()
                    }
                }
            }
            None => {
                tracing::warn!("Library not available, falling back to cache-only dedup");
                HashSet::new()
            }
        };

        // Queue new items (not in library and not in fallback cache).
        let mut new_items = 0;
        {
            let mut queue = self.download_queue.lock();
            for item in collection {
                let item_id_str = item.id.as_str().to_string();

                if library_known.contains(&item_id_str) {
                    // Already in library via ItemId fast path, skip
                    continue;
                }

                // Not found by ItemId — try work-identity dedup (artist + album).
                // This catches duplicate purchases (e.g. vinyl + digital) that share the
                // same work but carry different ItemIds.
                if let Some(lib_client) = &maybe_lib_client {
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
                            // the cheap has_facts("ItemId", ...) path.
                            if let Err(e) = lib_client
                                .write_fact(&matched_hash, MusicValue::ItemId(item_id_str.clone()))
                            {
                                tracing::warn!(
                                    error = %e,
                                    item_id = %item_id_str,
                                    "Failed to backfill ItemId fact — skip still applies"
                                );
                            }

                            continue;
                        }
                        Ok(_) => {
                            // No match — fall through to cache check and queue
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                item_id = %item_id_str,
                                "Work-identity search failed — falling through to download"
                            );
                        }
                    }
                }

                // Not in library — check fallback cache
                {
                    let cache = self.cache.lock();
                    let cache_key =
                        format!("{}|{}|{}|0", item.artist, item.title, item.id.as_str());
                    if cache.is_downloaded(&cache_key) || cache.is_item_downloaded(item.id.as_str())
                    {
                        continue;
                    }
                }

                new_items += 1;
                queue.push_back(QueuedDownload {
                    item,
                    state: InternalDownloadState::Queued,
                    downloaded_bytes: 0,
                    total_bytes: None,
                    error: None,
                });
            }
        }

        tracing::info!(
            total = total_items,
            new = new_items,
            "Sync complete, items queued"
        );

        SourceResponse::SyncStarted {
            total_items,
            new_items,
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

    /// Cancel a download
    fn cancel_download(&self, id: &str) {
        // Remove from queue if queued
        let mut queue = self.download_queue.lock();
        queue.retain(|dl| dl.item.id.as_str() != id);

        // Mark as cancelled if active
        let mut active = self.active_downloads.lock();
        if let Some(dl) = active.get_mut(id) {
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
        let request = match server.recv() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "Failed to receive request");
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

        // Get next item from queue
        let next_item = {
            let mut queue = service.download_queue.lock();
            queue.pop_front()
        };

        let Some(mut queued) = next_item else {
            // Queue empty, wait a bit
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            continue;
        };

        let item_id = queued.item.id.as_str().to_string();
        queued.state = InternalDownloadState::Downloading;

        // Add to active downloads
        {
            let mut active = service.active_downloads.lock();
            active.insert(item_id.clone(), queued.clone());
        }

        // Load client for this download
        let client = match service.try_load_client() {
            Some(c) => c,
            None => {
                let mut active = service.active_downloads.lock();
                if let Some(dl) = active.get_mut(&item_id) {
                    dl.state = InternalDownloadState::Failed;
                    dl.error = Some("No authenticated client available".to_string());
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

                                    if let Ok(inbox_path) = InboxPath::new(filename) {
                                        let source = IngestSource::Bandcamp {
                                            item_id: item_id.clone(),
                                            artist_url: None,
                                        };
                                        match lib_client
                                            .ingest_file_with_source(&inbox_path, Some(source))
                                        {
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
    // Integration test: work-identity dedup via IPC stub
    // =========================================================================

    #[cfg(test)]
    mod integration {
        use super::*;
        use library_service_stub::service::{run_ipc_server, LibraryService};
        use music_facts::{
            Album, Artist, ContentHash as MusicContentHash, FactOrigin, FactSource,
            MusicValue as MV, Title,
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
            let source = FactSource::new("test-seed", "0.0.0", FactOrigin::Unknown);
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
            let svc = Arc::new(
                LibraryService::new(
                    metadata_dir.to_path_buf(),
                    metadata_dir.to_path_buf(),
                    "ipc:///dev/null",
                )
                .expect("failed to create stub library"),
            );
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
            let collection = vec![vinyl_item, digital_item];

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
                .write_fact(
                    &matched_hash,
                    MusicValue::ItemId("p_digital_222".to_string()),
                )
                .expect("write_fact");

            // Verify backfill: now p_digital_222 is known by ItemId
            let after_backfill = client
                .has_facts("ItemId", vec!["p_digital_222".to_string()])
                .expect("has_facts after backfill");
            assert!(
                after_backfill.contains(&"p_digital_222".to_string()),
                "after backfill, p_digital_222 should be found by ItemId"
            );
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
