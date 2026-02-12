//! IPC interface for mdma-library using nng
//!
//! Uses request/reply pattern over IPC socket

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// Requests that can be sent to the library service
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LibraryRequest {
    /// Get service status
    GetStatus,

    /// List all tracks (optionally filtered)
    ListTracks { limit: Option<usize> },

    /// Get a specific track by content hash
    GetTrack { hash: String },

    /// Search tracks by query string
    Search { query: String },

    /// Get files currently in inbox queue
    GetInboxQueue,

    /// Manually trigger ingestion of a file
    IngestFile { path: PathBuf },

    /// Ping to check if service is alive
    Ping,
}

/// Track information for display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub content_hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_seconds: Option<u32>,
    pub bpm: Option<f32>,
    pub key: Option<String>,
    pub blob_path: Option<PathBuf>,
}

/// Service status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub version: String,
    pub tracks_indexed: usize,
    pub facts_count: usize,
    pub inbox_queue_size: usize,
    pub uptime_seconds: u64,
}

/// Responses from the library service
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LibraryResponse {
    /// Service status
    Status(ServiceStatus),

    /// List of tracks
    Tracks(Vec<TrackInfo>),

    /// Single track (or None if not found)
    Track(Option<TrackInfo>),

    /// Search results
    SearchResults(Vec<TrackInfo>),

    /// Inbox queue contents
    InboxQueue(Vec<PathBuf>),

    /// Result of ingestion request
    IngestResult {
        hash: Option<String>,
        success: bool,
        message: String,
    },

    /// Pong response
    Pong,

    /// Error response
    Error { message: String },
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Connection error: {0}")]
    Connection(String),
}

/// IPC Server for the library service
pub struct IpcServer {
    socket: nng::Socket,
}

impl IpcServer {
    /// Create and bind an IPC server
    pub fn bind(address: &str) -> Result<Self, IpcError> {
        let socket = nng::Socket::new(nng::Protocol::Rep0)?;
        socket.listen(address)?;
        tracing::info!(address = %address, "IPC server listening");
        Ok(Self { socket })
    }

    /// Receive a request (blocking)
    pub fn recv(&self) -> Result<LibraryRequest, IpcError> {
        let msg = self.socket.recv()?;
        let request: LibraryRequest = serde_json::from_slice(&msg)?;
        Ok(request)
    }

    /// Send a response
    pub fn send(&self, response: &LibraryResponse) -> Result<(), IpcError> {
        let data = serde_json::to_vec(response)?;
        let msg = nng::Message::from(&data[..]);
        self.socket.send(msg).map_err(|(_, e)| IpcError::Nng(e))?;
        Ok(())
    }
}

/// IPC Client for connecting to the library service
pub struct IpcClient {
    socket: nng::Socket,
}

impl IpcClient {
    /// Connect to the library service
    pub fn connect(address: &str) -> Result<Self, IpcError> {
        let socket = nng::Socket::new(nng::Protocol::Req0)?;
        socket.dial(address)?;
        Ok(Self { socket })
    }

    /// Send a request and receive response
    pub fn request(&self, request: &LibraryRequest) -> Result<LibraryResponse, IpcError> {
        let data = serde_json::to_vec(request)?;
        let msg = nng::Message::from(&data[..]);
        self.socket.send(msg).map_err(|(_, e)| IpcError::Nng(e))?;

        let response_msg = self.socket.recv()?;
        let response: LibraryResponse = serde_json::from_slice(&response_msg)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_request() {
        let req = LibraryRequest::GetStatus;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("GetStatus"));
    }

    #[test]
    fn serialize_response() {
        let resp = LibraryResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Pong"));
    }

    #[test]
    fn roundtrip_track_info() {
        let track = TrackInfo {
            content_hash: "sha256:abc123".to_string(),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: None,
            duration_seconds: Some(180),
            bpm: Some(128.0),
            key: Some("Am".to_string()),
            blob_path: Some(PathBuf::from("/music/blobs/ab/abc123.flac")),
        };

        let json = serde_json::to_string(&track).unwrap();
        let parsed: TrackInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, track.title);
    }
}
