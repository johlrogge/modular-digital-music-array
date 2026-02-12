//! Library service implementation
//!
//! Handles IPC requests and manages library state

use crate::ipc::{IpcServer, LibraryRequest, LibraryResponse, ServiceStatus, TrackInfo};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("IPC error: {0}")]
    Ipc(#[from] crate::ipc::IpcError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Library service state
pub struct LibraryService {
    music_dir: PathBuf,
    metadata_dir: PathBuf,
    start_time: Instant,
    tracks_indexed: AtomicUsize,
    facts_count: AtomicUsize,
}

impl LibraryService {
    /// Create a new library service
    pub fn new(music_dir: PathBuf, metadata_dir: PathBuf) -> Self {
        Self {
            music_dir,
            metadata_dir,
            start_time: Instant::now(),
            tracks_indexed: AtomicUsize::new(0),
            facts_count: AtomicUsize::new(0),
        }
    }

    /// Handle a single request
    pub fn handle_request(&self, request: LibraryRequest) -> LibraryResponse {
        tracing::debug!(?request, "Handling request");

        match request {
            LibraryRequest::Ping => LibraryResponse::Pong,

            LibraryRequest::GetStatus => {
                let status = ServiceStatus {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    tracks_indexed: self.tracks_indexed.load(Ordering::Relaxed),
                    facts_count: self.facts_count.load(Ordering::Relaxed),
                    inbox_queue_size: self.get_inbox_queue().len(),
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                };
                LibraryResponse::Status(status)
            }

            LibraryRequest::ListTracks { limit } => {
                // TODO: Read from fact stream and aggregate
                let tracks = self.list_tracks(limit);
                LibraryResponse::Tracks(tracks)
            }

            LibraryRequest::GetTrack { hash } => {
                // TODO: Look up track by hash
                let track = self.get_track(&hash);
                LibraryResponse::Track(track)
            }

            LibraryRequest::Search { query } => {
                // TODO: Implement search
                let results = self.search_tracks(&query);
                LibraryResponse::SearchResults(results)
            }

            LibraryRequest::GetInboxQueue => {
                let queue = self.get_inbox_queue();
                LibraryResponse::InboxQueue(queue)
            }

            LibraryRequest::IngestFile { path } => {
                // TODO: Trigger ingestion pipeline
                match self.ingest_file(&path) {
                    Ok(hash) => LibraryResponse::IngestResult {
                        hash: Some(hash),
                        success: true,
                        message: "File ingested successfully".to_string(),
                    },
                    Err(e) => LibraryResponse::IngestResult {
                        hash: None,
                        success: false,
                        message: e.to_string(),
                    },
                }
            }
        }
    }

    /// Get files in inbox directory
    fn get_inbox_queue(&self) -> Vec<PathBuf> {
        let inbox_dir = self.music_dir.join("inbox");
        if !inbox_dir.exists() {
            return vec![];
        }

        std::fs::read_dir(&inbox_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List tracks (stub - TODO: read from facts)
    fn list_tracks(&self, limit: Option<usize>) -> Vec<TrackInfo> {
        // For now, scan blob directory
        let blobs_dir = self.music_dir.join("blobs");
        if !blobs_dir.exists() {
            return vec![];
        }

        let mut tracks = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&blobs_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                if entry.path().is_dir() {
                    if let Ok(subentries) = std::fs::read_dir(entry.path()) {
                        for subentry in subentries.filter_map(|e| e.ok()) {
                            let path = subentry.path();
                            if path.is_file() {
                                if let Some(hash) = path.file_stem().and_then(|s| s.to_str()) {
                                    tracks.push(TrackInfo {
                                        content_hash: format!("sha256:{}", hash),
                                        title: None, // TODO: Read from facts
                                        artist: None,
                                        album: None,
                                        duration_seconds: None,
                                        bpm: None,
                                        key: None,
                                        blob_path: Some(path),
                                    });
                                }
                            }
                        }
                    }
                }

                if let Some(limit) = limit {
                    if tracks.len() >= limit {
                        break;
                    }
                }
            }
        }

        tracks
    }

    /// Get track by hash (stub)
    fn get_track(&self, hash: &str) -> Option<TrackInfo> {
        let hash_clean = hash.strip_prefix("sha256:").unwrap_or(hash);
        let blob_dir = self.music_dir.join("blobs").join(&hash_clean[..2]);

        // Try common extensions
        for ext in &["flac", "mp3", "aiff", "wav"] {
            let blob_path = blob_dir.join(format!("{}.{}", hash_clean, ext));
            if blob_path.exists() {
                return Some(TrackInfo {
                    content_hash: format!("sha256:{}", hash_clean),
                    title: None,
                    artist: None,
                    album: None,
                    duration_seconds: None,
                    bpm: None,
                    key: None,
                    blob_path: Some(blob_path),
                });
            }
        }

        None
    }

    /// Search tracks (stub)
    fn search_tracks(&self, _query: &str) -> Vec<TrackInfo> {
        // TODO: Implement actual search over facts
        vec![]
    }

    /// Ingest a file (stub)
    fn ingest_file(&self, path: &PathBuf) -> Result<String, ServiceError> {
        if !path.exists() {
            return Err(ServiceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File not found: {}", path.display()),
            )));
        }

        // TODO: Run through pipeline
        // For now, just return a placeholder hash
        Ok("sha256:not_implemented".to_string())
    }
}

/// Run the IPC server loop
pub fn run_ipc_server(service: Arc<LibraryService>, address: &str) -> Result<(), ServiceError> {
    let server = IpcServer::bind(address)?;

    tracing::info!("IPC server running, waiting for requests...");

    loop {
        match server.recv() {
            Ok(request) => {
                let response = service.handle_request(request);
                if let Err(e) = server.send(&response) {
                    tracing::error!(error = %e, "Failed to send response");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to receive request");
            }
        }
    }
}
