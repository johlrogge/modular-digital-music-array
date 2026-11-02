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

    /// Force re-sync of a specific source item by identifier.
    ResyncItem { identifier: String },

    /// Check if a single item is stale by fetching current details and
    /// comparing track count against the library.
    CheckItem { identifier: String },

    /// List the user's playlists on this source.
    /// Answer with `SourceResponse::Playlists`.
    ListPlaylists,

    /// Fetch one playlist. `playlist` is a slug or a URL.
    /// Answer with `SourceResponse::Playlist`.
    GetPlaylist { playlist: String },
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

    /// Resync queued for a specific source item.
    ResyncQueued {
        identifier: String,
        tracks_queued: usize,
    },

    /// Result of a CheckItem request.
    ItemChecked {
        identifier: String,
        live_track_count: usize,
        stored_track_count: usize,
        stale: bool,
    },

    /// Result of a ListPlaylists request.
    Playlists(Vec<SourcePlaylistSummary>),

    /// Result of a GetPlaylist request.
    Playlist(SourcePlaylist),

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

/// Summary of a playlist on a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePlaylistSummary {
    pub id: String,
    pub title: String,
    pub url: String,
    pub slug: String,
    pub track_count: u32,
    pub is_private: bool,
}

/// A full playlist from a source, tracks in source order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePlaylist {
    pub id: String,
    pub title: String,
    pub url: String,
    pub total_count: u32,
    pub tracks: Vec<SourcePlaylistTrack>,
}

/// A single track within a source playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePlaylistTrack {
    pub position: u32,
    pub source_track_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: f64,
    pub owned: bool,
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

    #[error("item not found: {identifier}")]
    ItemNotFound { identifier: String },

    #[error("check failed for {identifier}: {reason}")]
    CheckFailed { identifier: String, reason: String },

    /// The source does not support the requested operation.
    #[error("unsupported request: {request}")]
    Unsupported { request: String },
}

impl SourceError {
    /// Build an `Unsupported` error naming the request.
    pub fn unsupported(request: impl Into<String>) -> Self {
        Self::Unsupported {
            request: request.into(),
        }
    }
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

    #[test]
    fn request_resync_item_roundtrip() {
        let req = SourceRequest::ResyncItem {
            identifier: "p123456".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceRequest::ResyncItem { identifier } => assert_eq!(identifier, "p123456"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_resync_queued_roundtrip() {
        let resp = SourceResponse::ResyncQueued {
            identifier: "p123456".to_string(),
            tracks_queued: 7,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::ResyncQueued {
                identifier,
                tracks_queued,
            } => {
                assert_eq!(identifier, "p123456");
                assert_eq!(tracks_queued, 7);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn source_error_item_not_found_roundtrip() {
        let err = SourceError::ItemNotFound {
            identifier: "p999".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: SourceError = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceError::ItemNotFound { identifier } => assert_eq!(identifier, "p999"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_check_item_roundtrip() {
        let req = SourceRequest::CheckItem {
            identifier: "p123456".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceRequest::CheckItem { identifier } => assert_eq!(identifier, "p123456"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_item_checked_stale_roundtrip() {
        let resp = SourceResponse::ItemChecked {
            identifier: "p123456".to_string(),
            live_track_count: 7,
            stored_track_count: 2,
            stale: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::ItemChecked {
                identifier,
                live_track_count,
                stored_track_count,
                stale,
            } => {
                assert_eq!(identifier, "p123456");
                assert_eq!(live_track_count, 7);
                assert_eq!(stored_track_count, 2);
                assert!(stale);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_item_checked_fresh_roundtrip() {
        let resp = SourceResponse::ItemChecked {
            identifier: "p999".to_string(),
            live_track_count: 5,
            stored_track_count: 5,
            stale: false,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::ItemChecked { stale, .. } => assert!(!stale),
            _ => panic!("wrong variant"),
        }
    }

    fn sample_playlist() -> SourcePlaylist {
        SourcePlaylist {
            id: "pl1".to_string(),
            title: "Friday Set".to_string(),
            url: "https://bandcamp.com/u/x/playlists/friday".to_string(),
            total_count: 2,
            tracks: vec![
                SourcePlaylistTrack {
                    position: 1,
                    source_track_id: "t1".to_string(),
                    title: "One".to_string(),
                    artist: "A".to_string(),
                    album: Some("Alb".to_string()),
                    duration_secs: 301.5,
                    owned: true,
                },
                SourcePlaylistTrack {
                    position: 2,
                    source_track_id: "t2".to_string(),
                    title: "Two".to_string(),
                    artist: "B".to_string(),
                    album: None,
                    duration_secs: 200.0,
                    owned: false,
                },
            ],
        }
    }

    #[test]
    fn request_list_playlists_roundtrip() {
        let json = serde_json::to_string(&SourceRequest::ListPlaylists).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SourceRequest::ListPlaylists));
    }

    #[test]
    fn request_get_playlist_roundtrip() {
        let req = SourceRequest::GetPlaylist {
            playlist: "friday".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceRequest::GetPlaylist { playlist } => assert_eq!(playlist, "friday"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_playlists_roundtrip() {
        let summary = SourcePlaylistSummary {
            id: "pl1".to_string(),
            title: "Friday Set".to_string(),
            url: "https://example/friday".to_string(),
            slug: "friday".to_string(),
            track_count: 12,
            is_private: true,
        };
        let resp = SourceResponse::Playlists(vec![summary.clone()]);
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::Playlists(v) => assert_eq!(v, vec![summary]),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_playlist_roundtrip() {
        let pl = sample_playlist();
        let json = serde_json::to_string(&SourceResponse::Playlist(pl.clone())).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::Playlist(p) => assert_eq!(p, pl),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn source_error_unsupported_roundtrip() {
        let resp = SourceResponse::Error(SourceError::unsupported("ListPlaylists"));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::Error(e @ SourceError::Unsupported { .. }) => {
                assert_eq!(e.to_string(), "unsupported request: ListPlaylists");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn source_error_check_failed_roundtrip() {
        let err = SourceError::CheckFailed {
            identifier: "p111".to_string(),
            reason: "HTTP 403 Forbidden".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: SourceError = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceError::CheckFailed { identifier, reason } => {
                assert_eq!(identifier, "p111");
                assert_eq!(reason, "HTTP 403 Forbidden");
            }
            _ => panic!("wrong variant"),
        }
    }
}
