//! Bandcamp download service
//!
//! Handles collection syncing and download management.

use crate::cache::DownloadCache;
use crate::ipc::{
    BandcampRequest, BandcampResponse, BandcampUsername, DownloadState, DownloadStatus, IpcServer,
    ItemId, ProtocolError, ServiceStatus,
};
use bandcamp_api::{AudioFormat, BandcampClient, CollectionItem};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::mpsc;

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

/// Bandcamp download service
pub struct BandcampService {
    /// HTTP client (None if cookies not loaded)
    client: Mutex<Option<BandcampClient>>,
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
    /// Shutdown signal sender
    shutdown_tx: Option<mpsc::Sender<()>>,
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

        // Try to load cookies
        let client = Self::try_load_client(&cookie_path);

        Ok(Self {
            client: Mutex::new(client),
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
            shutdown_tx: None,
        })
    }

    /// Try to load the Bandcamp client from cookies
    fn try_load_client(cookie_path: &PathBuf) -> Option<BandcampClient> {
        match bandcamp_api::load_cookies(cookie_path) {
            Ok(jar) => {
                tracing::info!(path = %cookie_path.display(), "Loaded cookies");
                Some(BandcampClient::new(jar))
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %cookie_path.display(), "Failed to load cookies");
                None
            }
        }
    }

    /// Check if cookies are loaded
    fn cookies_loaded(&self) -> bool {
        self.client.lock().is_some()
    }

    /// Handle a request
    pub fn handle_request(&self, request: BandcampRequest) -> BandcampResponse {
        tracing::debug!(?request, "Handling request");

        match request {
            BandcampRequest::Ping => BandcampResponse::Pong,

            BandcampRequest::GetStatus => {
                let status = ServiceStatus {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    cookies_loaded: self.cookies_loaded(),
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
                let client = Self::try_load_client(&self.cookie_path);
                let valid = client.is_some();
                *self.client.lock() = client;

                BandcampResponse::CookiesReloaded {
                    valid,
                    message: if valid {
                        "Cookies loaded successfully".to_string()
                    } else {
                        "Failed to load cookies".to_string()
                    },
                }
            }

            BandcampRequest::Sync { username } => {
                // This is a blocking request, but sync is async
                // We'll return immediately and the actual sync happens in the background
                match self.start_sync(&username) {
                    Ok((total, new)) => BandcampResponse::SyncStarted {
                        username: username.to_string(),
                        total_items: total,
                        new_items: new,
                    },
                    Err(e) => BandcampResponse::Error(e),
                }
            }

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

    /// Start a collection sync (blocking for now, returns immediately with queue info)
    fn start_sync(&self, username: &BandcampUsername) -> Result<(usize, usize), ProtocolError> {
        let client = self.client.lock();
        let _client = client.as_ref().ok_or(ProtocolError::NotAuthenticated {
            message: "Cookies not loaded. Upload cookies first.".to_string(),
        })?;

        // For now, we can't easily do async from a sync context
        // The actual sync will be done via a separate mechanism
        // This is a limitation we need to address later

        // Store the username
        *self.current_username.lock() = Some(username.to_string());

        // Return placeholder - the actual sync needs async runtime
        // TODO: Implement proper async sync with a background task
        tracing::info!(username = %username, "Sync requested (not yet implemented in blocking context)");

        Err(ProtocolError::Internal {
            message: "Sync requires async runtime - use the async API".to_string(),
        })
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

/// Run the IPC server loop
pub fn run_ipc_server(
    service: Arc<BandcampService>,
    address: &str,
    tcp_address: Option<&str>,
) -> Result<(), ServiceError> {
    let server = IpcServer::bind(address)?;

    // Also listen on TCP if specified
    if let Some(tcp) = tcp_address {
        server.listen_also(tcp)?;
    }

    tracing::info!("IPC server running, waiting for requests...");

    loop {
        match server.recv() {
            Ok(request) => {
                let response = service.handle_request(request);
                if let Err(e) = server.send(&response) {
                    tracing::error!(error = %e, "Failed to send response");
                    let fallback = BandcampResponse::Error(ProtocolError::Internal {
                        message: format!("Internal error: {}", e),
                    });
                    if let Err(e2) = server.send(&fallback) {
                        tracing::error!(error = %e2, "Failed to send fallback error response");
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to receive request");
            }
        }
    }
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

        // Get the client
        let has_client: bool = {
            let client_lock = service.client.lock();
            match client_lock.as_ref() {
                Some(_c) => {
                    // We need to clone or recreate the client for async use
                    // For now, skip if no client
                    drop(client_lock);
                    false // TODO: Fix this - need to share client properly
                }
                None => {
                    tracing::error!("No client available for download");
                    false
                }
            }
        };

        if !has_client {
            // Mark as failed
            let mut active = service.active_downloads.lock();
            if let Some(dl) = active.get_mut(&item_id) {
                dl.state = DownloadState::Failed;
                dl.error = Some("No authenticated client available".to_string());
            }
            service.downloads_failed.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        // TODO: Implement actual download logic
        // This requires proper async client sharing
        tracing::warn!(item_id = %item_id, "Download worker not fully implemented yet");

        // For now, just mark as failed with a message
        {
            let mut active = service.active_downloads.lock();
            if let Some(dl) = active.get_mut(&item_id) {
                dl.state = DownloadState::Failed;
                dl.error = Some("Download worker not fully implemented".to_string());
            }
        }
        service.downloads_failed.fetch_add(1, Ordering::Relaxed);

        // Small delay between downloads
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}
