//! Unified Source Protocol
//!
//! All music sources (Bandcamp, Beatport, etc.) implement this protocol.
//! Authentication and configuration are internal concerns — the source
//! service reads its own config files.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

// ============================================================================
// DownloadId newtype
// ============================================================================

/// Identifies a specific download task.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DownloadId(String);

impl DownloadId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DownloadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// ============================================================================
// Request Types
// ============================================================================

/// Requests that can be sent to any music source service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SourceRequest {
    /// Check if service is alive.
    Ping,

    /// Get service status.
    GetStatus,

    /// Start syncing the user's collection. Source handles auth internally.
    Sync,

    /// List current downloads (active + queued + recent).
    ListDownloads,

    /// Cancel a specific download.
    CancelDownload { id: DownloadId },

    /// Pause all downloads.
    PauseAll,

    /// Resume downloads.
    ResumeAll,
}

// ============================================================================
// Response Types
// ============================================================================

/// Responses from a music source service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SourceResponse {
    /// Pong response to Ping.
    Pong,

    /// Service status.
    Status(SourceStatus),

    /// Sync started.
    SyncStarted {
        total_items: usize,
        new_items: usize,
    },

    /// List of current downloads.
    Downloads(Vec<DownloadStatus>),

    /// Download cancelled.
    Cancelled { id: DownloadId },

    /// Downloads paused.
    Paused,

    /// Downloads resumed.
    Resumed,

    /// Error response.
    Error(SourceError),
}

// ============================================================================
// Data Types
// ============================================================================

/// Authentication state of a source service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AuthStatus {
    Authenticated,
    NotAuthenticated,
}

/// Queue state of a source service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum QueueState {
    Active,
    Paused,
}

/// Service status information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub name: String,
    pub version: String,
    pub auth: AuthStatus,
    pub downloads_active: usize,
    pub downloads_queued: usize,
    pub downloads_completed: usize,
    pub downloads_failed: usize,
    pub uptime_seconds: u64,
    pub queue: QueueState,
}

/// Download status for a single item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadStatus {
    pub id: DownloadId,
    pub artist: String,
    pub title: String,
    pub state: DownloadState,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

/// State of a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    /// Waiting to be downloaded.
    Queued,
    /// Currently downloading.
    Downloading,
    /// Processing (extracting, converting, etc.).
    Processing,
    /// Successfully completed.
    Completed,
    /// Failed with error.
    Failed { message: String },
    /// Cancelled by user.
    Cancelled,
}

impl std::fmt::Display for DownloadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadState::Queued => write!(f, "queued"),
            DownloadState::Downloading => write!(f, "downloading"),
            DownloadState::Processing => write!(f, "processing"),
            DownloadState::Completed => write!(f, "completed"),
            DownloadState::Failed { message } => write!(f, "failed: {}", message),
            DownloadState::Cancelled => write!(f, "cancelled"),
        }
    }
}

// ============================================================================
// Error Types
// ============================================================================

/// Typed errors returned by a source service.
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum SourceError {
    #[error("not authenticated: {message}")]
    NotAuthenticated { message: String },

    #[error("sync failed: {message}")]
    SyncFailed { message: String },

    #[error("download failed: {id} - {message}")]
    DownloadFailed { id: DownloadId, message: String },

    #[error("rate limited, retry after {retry_after_secs} seconds")]
    RateLimited { retry_after_secs: u64 },

    #[error("internal error: {message}")]
    Internal { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn request_ping_roundtrip() {
        let req = SourceRequest::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("Ping"));
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SourceRequest::Ping));
    }

    #[test]
    fn request_sync_roundtrip() {
        let req = SourceRequest::Sync;
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SourceRequest::Sync));
    }

    #[test]
    fn download_id_newtype_roundtrip() {
        let id = DownloadId::new("p123456");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"p123456\"");
        let parsed: DownloadId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.as_str(), "p123456");
        assert_eq!(parsed.to_string(), "p123456");
    }

    #[test]
    fn request_cancel_download_roundtrip() {
        let req = SourceRequest::CancelDownload {
            id: DownloadId::new("p123456"),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceRequest::CancelDownload { id } => assert_eq!(id, DownloadId::new("p123456")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_pong_roundtrip() {
        let resp = SourceResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SourceResponse::Pong));
    }

    #[test]
    fn response_sync_started_roundtrip() {
        let resp = SourceResponse::SyncStarted {
            total_items: 100,
            new_items: 5,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::SyncStarted {
                total_items,
                new_items,
            } => {
                assert_eq!(total_items, 100);
                assert_eq!(new_items, 5);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn source_status_roundtrip() {
        let status = SourceStatus {
            name: "bandcamp".to_string(),
            version: "0.1.0".to_string(),
            auth: AuthStatus::Authenticated,
            downloads_active: 1,
            downloads_queued: 5,
            downloads_completed: 10,
            downloads_failed: 2,
            uptime_seconds: 3600,
            queue: QueueState::Active,
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "bandcamp");
        assert_eq!(parsed.version, "0.1.0");
        assert_eq!(parsed.auth, AuthStatus::Authenticated);
    }

    #[test]
    fn download_status_roundtrip() {
        let status = DownloadStatus {
            id: DownloadId::new("p123456"),
            artist: "Test Artist".to_string(),
            title: "Test Album".to_string(),
            state: DownloadState::Downloading,
            downloaded_bytes: 1024,
            total_bytes: Some(10240),
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: DownloadStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, DownloadId::new("p123456"));
        assert_eq!(parsed.state, DownloadState::Downloading);
    }

    #[test]
    fn download_state_failed_roundtrip() {
        let state = DownloadState::Failed {
            message: "disk full".to_string(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: DownloadState = serde_json::from_str(&json).unwrap();
        match parsed {
            DownloadState::Failed { message } => assert_eq!(message, "disk full"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn source_error_roundtrip() {
        let err = SourceError::NotAuthenticated {
            message: "cookies expired".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: SourceError = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceError::NotAuthenticated { message } => assert_eq!(message, "cookies expired"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = SourceResponse::Error(SourceError::RateLimited {
            retry_after_secs: 60,
        });
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::Error(SourceError::RateLimited { retry_after_secs }) => {
                assert_eq!(retry_after_secs, 60);
            }
            _ => panic!("wrong variant"),
        }
    }
}
