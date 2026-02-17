//! Bandcamp download service
//!
//! Handles collection syncing and download management.
//! Uses async throughout with NNG IPC bridged via channels.

use crate::cache::DownloadCache;
use crate::ipc::{
    BandcampRequest, BandcampResponse, BandcampUsername, DownloadState, DownloadStatus, IpcServer,
    ItemId, ProtocolError, ServiceStatus,
};
use bandcamp_api::{AudioFormat, BandcampClient, CollectionItem};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
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

/// Download queue entry
#[derive(Clone)]
struct QueuedDownload {
    item: CollectionItem,
    state: DownloadState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
}

/// Request with response channel for async processing
struct IpcMessage {
    request: BandcampRequest,
    response_tx: oneshot::Sender<BandcampResponse>,
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
}

impl BandcampService {
    /// Create a new service
    pub fn new(
        cookie_path: PathBuf,
        downloads_dir: PathBuf,
        inbox_dir: PathBuf,
        cache_path: PathBuf,
        format: AudioFormat,
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
        })
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
    pub async fn handle_request(&self, request: BandcampRequest) -> BandcampResponse {
        tracing::debug!(?request, "Handling request");

        match request {
            BandcampRequest::Ping => BandcampResponse::Pong,

            BandcampRequest::GetStatus => {
                let status = ServiceStatus {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    cookies_loaded: self.cookies_loaded.load(Ordering::Relaxed),
                    current_username: self.current_username.lock().clone(),
                    downloads_active: self.active_downloads.lock().len(),
                    downloads_queued: self.download_queue.lock().len(),
                    downloads_completed: self.downloads_completed.load(Ordering::Relaxed),
                    downloads_failed: self.downloads_failed.load(Ordering::Relaxed),
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                    paused: self.paused.load(Ordering::Relaxed),
                };
                BandcampResponse::Status(status)
            }

            BandcampRequest::ReloadCookies => {
                let client = self.try_load_client();
                let valid = client.is_some();

                BandcampResponse::CookiesReloaded {
                    valid,
                    message: if valid {
                        "Cookies loaded successfully".to_string()
                    } else {
                        "Failed to load cookies".to_string()
                    },
                }
            }

            BandcampRequest::Sync { username } => self.handle_sync(username).await,

            BandcampRequest::ListDownloads => {
                let downloads = self.list_downloads();
                BandcampResponse::Downloads(downloads)
            }

            BandcampRequest::CancelDownload { id } => {
                self.cancel_download(&id);
                BandcampResponse::Cancelled { id }
            }

            BandcampRequest::PauseAll => {
                self.paused.store(true, Ordering::Relaxed);
                tracing::info!("Downloads paused");
                BandcampResponse::Paused
            }

            BandcampRequest::ResumeAll => {
                self.paused.store(false, Ordering::Relaxed);
                tracing::info!("Downloads resumed");
                BandcampResponse::Resumed
            }
        }
    }

    /// Handle sync request - fetches collection and queues new downloads
    async fn handle_sync(&self, username: BandcampUsername) -> BandcampResponse {
        // Try to load client
        let client = match self.try_load_client() {
            Some(c) => c,
            None => {
                return BandcampResponse::Error(ProtocolError::NotAuthenticated {
                    message: "Cookies not loaded. Upload cookies first.".to_string(),
                });
            }
        };

        // Store the username
        *self.current_username.lock() = Some(username.to_string());

        tracing::info!(username = %username, "Starting collection sync");

        // Fetch the collection
        let collection = match client.get_collection(username.as_str()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch collection");
                return BandcampResponse::Error(ProtocolError::CollectionFetchFailed {
                    message: e.to_string(),
                });
            }
        };

        let total_items = collection.len();
        tracing::info!(total = total_items, "Fetched collection");

        // Filter out already downloaded items using the cache
        let cache = self.cache.lock();
        let mut new_items = 0;

        for item in collection {
            // For now, use item ID as cache key (we'll improve this when we have track info)
            let cache_key = format!("{}|{}|{}|0", item.artist, item.title, item.id.as_str());

            if !cache.is_downloaded(&cache_key) {
                new_items += 1;

                // Add to download queue
                let queued = QueuedDownload {
                    item,
                    state: DownloadState::Queued,
                    downloaded_bytes: 0,
                    total_bytes: None,
                    error: None,
                };

                self.download_queue.lock().push_back(queued);
            }
        }

        tracing::info!(
            total = total_items,
            new = new_items,
            "Sync complete, items queued"
        );

        BandcampResponse::SyncStarted {
            username: username.to_string(),
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
                id: ItemId::new(id),
                artist: dl.item.artist.to_string(),
                title: dl.item.title.to_string(),
                state: dl.state.clone(),
                downloaded_bytes: dl.downloaded_bytes,
                total_bytes: dl.total_bytes,
                error: dl.error.clone(),
            });
        }

        // Add queued downloads
        for dl in self.download_queue.lock().iter() {
            downloads.push(DownloadStatus {
                id: api_item_id_to_protocol(&dl.item.id),
                artist: dl.item.artist.to_string(),
                title: dl.item.title.to_string(),
                state: DownloadState::Queued,
                downloaded_bytes: 0,
                total_bytes: None,
                error: None,
            });
        }

        downloads
    }

    /// Cancel a download
    fn cancel_download(&self, id: &ItemId) {
        // Remove from queue if queued
        let mut queue = self.download_queue.lock();
        queue.retain(|dl| dl.item.id.as_str() != id.as_str());

        // Mark as cancelled if active
        let mut active = self.active_downloads.lock();
        if let Some(dl) = active.get_mut(id.as_str()) {
            dl.state = DownloadState::Cancelled;
        }
    }
}

/// Convert bandcamp_api::ItemId to protocol ItemId
fn api_item_id_to_protocol(id: &bandcamp_api::ItemId) -> ItemId {
    ItemId::new(id.as_str())
}

/// Supported audio file extensions
const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "wav", "aif", "aiff"];

/// Check if a file has an audio extension
fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Detect file type by magic bytes
fn detect_file_type(path: &Path) -> Option<&'static str> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).ok()?;

    match &magic {
        // ZIP: PK\x03\x04
        [0x50, 0x4B, 0x03, 0x04] => Some("zip"),
        // FLAC: fLaC
        [0x66, 0x4C, 0x61, 0x43] => Some("flac"),
        // MP3: ID3 or \xFF\xFB
        [0x49, 0x44, 0x33, _] => Some("mp3"),
        [0xFF, 0xFB, _, _] => Some("mp3"),
        // WAV: RIFF
        [0x52, 0x49, 0x46, 0x46] => Some("wav"),
        // AIFF: FORM
        [0x46, 0x4F, 0x52, 0x4D] => Some("aiff"),
        _ => None,
    }
}

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
        Some("zip") => extract_zip_to_inbox(download_path, inbox_dir),
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

/// Sanitize a string for use in filenames
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Generate a unique path, adding suffix if file exists
fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let dest_path = dir.join(filename);
    if !dest_path.exists() {
        return dest_path;
    }

    let path = Path::new(filename);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("flac");

    let mut counter = 1;
    loop {
        let new_name = format!("{}_{}.{}", stem, counter, ext);
        let new_path = dir.join(&new_name);
        if !new_path.exists() {
            return new_path;
        }
        counter += 1;
    }
}

/// Extract audio files from a ZIP archive to the inbox directory.
/// Returns the list of extracted file paths.
fn extract_zip_to_inbox(zip_path: &Path, inbox_dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut extracted = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Skip directories
        if entry.is_dir() {
            continue;
        }

        let entry_path = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue, // Skip entries with invalid paths
        };

        // Only extract audio files
        if !is_audio_file(&entry_path) {
            tracing::debug!(path = %entry_path.display(), "Skipping non-audio file");
            continue;
        }

        // Use just the filename, not the full path from the ZIP
        let filename = match entry_path.file_name().and_then(|f| f.to_str()) {
            Some(f) => f.to_string(),
            None => continue,
        };

        let final_path = unique_path(inbox_dir, &filename);

        // Extract the file
        let mut outfile = std::fs::File::create(&final_path)?;
        std::io::copy(&mut entry, &mut outfile)?;

        tracing::info!(
            source = %entry_path.display(),
            dest = %final_path.display(),
            "Extracted audio file"
        );
        extracted.push(final_path);
    }

    Ok(extracted)
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
                BandcampResponse::Error(ProtocolError::Internal {
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
        queued.state = DownloadState::Downloading;

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
                    dl.state = DownloadState::Failed;
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
                    dl.state = DownloadState::Failed;
                    dl.error = Some(format!("Failed to get details: {}", e));
                }
                service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        // Download to staging directory (use .download extension, actual type detected later)
        let staging_path = service.downloads_dir.join(format!("{}.download", item_id));

        use tokio_stream::StreamExt;
        let mut stream =
            std::pin::pin!(client.download_item(&details, service.format, &staging_path));

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
                            dl.state = DownloadState::Extracting;
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

                            // Update state to moving (already done, just update status)
                            {
                                let mut active = service.active_downloads.lock();
                                if let Some(dl) = active.get_mut(&item_id) {
                                    dl.state = DownloadState::Moving;
                                }
                            }

                            // Delete the source file (ZIP after extraction)
                            // For single tracks, the file was moved so this will fail - that's OK
                            if path.exists() {
                                if let Err(e) = tokio::fs::remove_file(&path).await {
                                    tracing::warn!(error = %e, "Failed to delete source file");
                                }
                            }

                            // Mark as completed
                            {
                                let mut active = service.active_downloads.lock();
                                if let Some(dl) = active.get_mut(&item_id) {
                                    dl.state = DownloadState::Completed;
                                }
                            }
                            service.downloads_completed.fetch_add(1, Ordering::Relaxed);

                            // Update cache for each extracted file
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
                                dl.state = DownloadState::Failed;
                                dl.error = Some(format!("Processing failed: {}", e));
                            }
                            service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::error!(error = %e, item_id = %item_id, "Extraction task panicked");
                            let mut active = service.active_downloads.lock();
                            if let Some(dl) = active.get_mut(&item_id) {
                                dl.state = DownloadState::Failed;
                                dl.error = Some("Extraction task panicked".to_string());
                            }
                            service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                bandcamp_api::DownloadEvent::Failed { error } => {
                    tracing::error!(item_id = %item_id, error = %error, "Download failed");
                    let mut active = service.active_downloads.lock();
                    if let Some(dl) = active.get_mut(&item_id) {
                        dl.state = DownloadState::Failed;
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
                if dl.state == DownloadState::Downloading {
                    dl.state = DownloadState::Failed;
                    dl.error = Some("Download stream ended unexpectedly".to_string());
                    service.downloads_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // Small delay between downloads for rate limiting
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}
