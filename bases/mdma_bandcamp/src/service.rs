//! Bandcamp download service
//!
//! Handles collection syncing and download management.
//! Uses async throughout with NNG IPC bridged via channels.

use crate::cache::DownloadCache;
use crate::ipc::{
    DownloadState, DownloadStatus, IpcServer, SourceError, SourceRequest, SourceResponse,
    SourceStatus,
};
use bandcamp_api::{AudioFormat, BandcampClient, CollectionItem};
use library_ipc_client::{InboxPath, IngestSource, LibraryClient};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

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
                self.cancel_download(&id);
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
        let library_known: HashSet<String> = match self.try_library_client() {
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

        // Filter out already downloaded items using library + cache
        let cache = self.cache.lock();
        let mut new_items = 0;

        for item in collection {
            let item_id_str = item.id.as_str().to_string();

            // Primary dedup: library knows this item ID
            if library_known.contains(&item_id_str) {
                continue;
            }

            // Fallback dedup: local cache
            let cache_key = format!("{}|{}|{}|0", item.artist, item.title, item.id.as_str());
            if cache.is_downloaded(&cache_key) || cache.is_item_downloaded(item.id.as_str()) {
                continue;
            }

            new_items += 1;

            // Add to download queue
            let queued = QueuedDownload {
                item,
                state: InternalDownloadState::Queued,
                downloaded_bytes: 0,
                total_bytes: None,
                error: None,
            };

            self.download_queue.lock().push_back(queued);
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
                id: id.clone(),
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
                id: dl.item.id.as_str().to_string(),
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
pub async fn run_async_ipc_server(
    service: Arc<BandcampService>,
    address: String,
    tcp_address: Option<String>,
) -> Result<(), ServiceError> {
    // Create channel for requests from NNG thread to async runtime
    let (request_tx, mut request_rx) = mpsc::channel::<IpcMessage>(32);

    // Spawn the NNG server in a blocking task
    let nng_handle = {
        let address = address.clone();
        let tcp_address = tcp_address.clone();
        tokio::task::spawn_blocking(move || run_nng_bridge(address, tcp_address, request_tx))
    };

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
    address: String,
    tcp_address: Option<String>,
    request_tx: mpsc::Sender<IpcMessage>,
) -> Result<(), ServiceError> {
    let server = IpcServer::bind(&address)?;

    if let Some(tcp) = tcp_address {
        server.listen_also(&tcp)?;
    }

    tracing::info!(address = %address, "NNG server listening");

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
