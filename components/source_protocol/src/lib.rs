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

    /// Start a background job importing a playlist into the library.
    /// `playlist` is a slug or a URL; `name` overrides the target playlist
    /// name. Answer with `SourceResponse::ImportStarted`, or
    /// `SourceError::ImportInProgress` if a job is running.
    ImportPlaylist {
        playlist: String,
        name: Option<String>,
        dry_run: bool,
    },

    /// Get the current or last import job.
    /// Answer with `SourceResponse::ImportStatus`.
    GetImportStatus,
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

    /// An import job was started.
    ImportStarted { job_id: u64 },

    /// Result of a GetImportStatus request: the current or last job, if any.
    ImportStatus(Option<ImportJobStatus>),

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

/// Phase of a server-side playlist import job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportPhase {
    /// Fetching the playlist from the source.
    Fetching,
    /// Matching playlist tracks against the library.
    Matching,
    /// Syncing the collection and waiting for queued downloads.
    Syncing,
    /// Matching again after downloads finished.
    Rematching,
    /// Writing the playlist.
    Writing,
    /// Finished successfully, with what the job did.
    Done(ImportResult),
    /// Aborted with an error.
    Failed { message: String },
}

/// How a finished import job ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportResult {
    /// The playlist was written.
    Written(ImportWritten),
    /// Dry run: this many tracks would have been written. Never zero; a dry
    /// run without matches is `NothingMatched`.
    DryRun { would_write: u32 },
    /// No track matched definitively, so nothing was written (an existing
    /// playlist is never replaced with an empty one).
    NothingMatched,
}

/// Progress of the downloads queued by the sync an import job triggered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSyncProgress {
    pub queued: u32,
    pub completed: u32,
    pub failed: u32,
    /// Why the wait for the downloads ended; `None` while still waiting.
    pub ended: Option<SyncWaitEnd>,
}

/// Why an import stopped waiting for its downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncWaitEnd {
    /// Every download completed, failed or was cancelled.
    AllFinished,
    /// Downloads were paused, so the rest were not fetched.
    Paused,
    /// The time limit passed first.
    TimedOut,
}

/// Result of matching one playlist track against the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImportTrackOutcome {
    /// Exactly one library track matched.
    Matched { hash: String },
    /// Several library tracks matched; never written.
    Ambiguous { candidates: Vec<String> },
    /// Artist and title match but the duration is outside tolerance
    /// (probably a different version); never written.
    DurationMismatch {
        hash: String,
        library_secs: u32,
        source_secs: u32,
    },
    /// No library match. `not_owned` is true when the user does not own
    /// the track on the source, so a sync cannot fetch it.
    Missing { not_owned: bool },
}

/// Match report line for one playlist track, in source order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportTrackReport {
    pub position: u32,
    pub artist: String,
    pub title: String,
    pub outcome: ImportTrackOutcome,
}

/// Outcome of writing the playlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWritten {
    /// False when an existing playlist was replaced.
    pub created: bool,
    pub tracks: u32,
}

/// Status of a playlist import job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportJobStatus {
    pub job_id: u64,
    /// Request metadata: whether the job was asked not to write. The outcome
    /// is in `phase` (`ImportPhase::Done`).
    pub dry_run: bool,
    pub playlist_title: Option<String>,
    pub target_name: Option<String>,
    pub total_count: Option<u32>,
    pub phase: ImportPhase,
    pub tracks: Vec<ImportTrackReport>,
    pub sync: Option<ImportSyncProgress>,
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

    /// The request is understood but its arguments are invalid
    /// (for example an unusable playlist name).
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// A playlist import job is already running.
    #[error("import already running: job {job_id}")]
    ImportInProgress { job_id: u64 },
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
    fn request_import_playlist_roundtrip() {
        let req = SourceRequest::ImportPlaylist {
            playlist: "friday".to_string(),
            name: Some("Friday Import".to_string()),
            dry_run: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceRequest::ImportPlaylist {
                playlist,
                name,
                dry_run,
            } => {
                assert_eq!(playlist, "friday");
                assert_eq!(name.as_deref(), Some("Friday Import"));
                assert!(dry_run);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn request_get_import_status_roundtrip() {
        let json = serde_json::to_string(&SourceRequest::GetImportStatus).unwrap();
        let parsed: SourceRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SourceRequest::GetImportStatus));
    }

    #[test]
    fn response_import_started_roundtrip() {
        let json = serde_json::to_string(&SourceResponse::ImportStarted { job_id: 7 }).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            SourceResponse::ImportStarted { job_id: 7 }
        ));
    }

    #[test]
    fn response_import_status_none_roundtrip() {
        let json = serde_json::to_string(&SourceResponse::ImportStatus(None)).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SourceResponse::ImportStatus(None)));
    }

    fn sample_import_status() -> ImportJobStatus {
        ImportJobStatus {
            job_id: 3,
            dry_run: false,
            playlist_title: Some("Friday Set".to_string()),
            target_name: Some("Friday Set".to_string()),
            total_count: Some(5),
            phase: ImportPhase::Syncing,
            tracks: vec![
                ImportTrackReport {
                    position: 1,
                    artist: "A".to_string(),
                    title: "One".to_string(),
                    outcome: ImportTrackOutcome::Matched {
                        hash: "1111111111111111".to_string(),
                    },
                },
                ImportTrackReport {
                    position: 2,
                    artist: "B".to_string(),
                    title: "Two".to_string(),
                    outcome: ImportTrackOutcome::Ambiguous {
                        candidates: vec!["22".to_string(), "33".to_string()],
                    },
                },
                ImportTrackReport {
                    position: 3,
                    artist: "C".to_string(),
                    title: "Three".to_string(),
                    outcome: ImportTrackOutcome::DurationMismatch {
                        hash: "44".to_string(),
                        library_secs: 210,
                        source_secs: 545,
                    },
                },
                ImportTrackReport {
                    position: 4,
                    artist: "D".to_string(),
                    title: "Four".to_string(),
                    outcome: ImportTrackOutcome::Missing { not_owned: true },
                },
            ],
            sync: Some(ImportSyncProgress {
                queued: 4,
                completed: 2,
                failed: 1,
                ended: Some(SyncWaitEnd::Paused),
            }),
        }
    }

    #[test]
    fn sync_wait_end_roundtrip() {
        for end in [
            SyncWaitEnd::AllFinished,
            SyncWaitEnd::Paused,
            SyncWaitEnd::TimedOut,
        ] {
            let progress = ImportSyncProgress {
                queued: 3,
                completed: 1,
                failed: 0,
                ended: Some(end),
            };
            let json = serde_json::to_string(&progress).unwrap();
            let parsed: ImportSyncProgress = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, progress);
        }
        let waiting = ImportSyncProgress {
            queued: 3,
            completed: 1,
            failed: 0,
            ended: None,
        };
        let parsed: ImportSyncProgress =
            serde_json::from_str(&serde_json::to_string(&waiting).unwrap()).unwrap();
        assert_eq!(parsed, waiting);
    }

    #[test]
    fn import_phase_done_variants_roundtrip() {
        for result in [
            ImportResult::Written(ImportWritten {
                created: false,
                tracks: 3,
            }),
            ImportResult::DryRun { would_write: 2 },
            ImportResult::NothingMatched,
        ] {
            let phase = ImportPhase::Done(result);
            let json = serde_json::to_string(&phase).unwrap();
            let parsed: ImportPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, phase);
        }
    }

    #[test]
    fn duration_mismatch_is_whole_seconds_on_the_wire() {
        let outcome = ImportTrackOutcome::DurationMismatch {
            hash: "44".to_string(),
            library_secs: 210,
            source_secs: 545,
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("\"library_secs\":210"), "got: {json}");
        assert!(!json.contains("210."), "got: {json}");
        let parsed: ImportTrackOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, outcome);
    }

    #[test]
    fn source_error_invalid_request_roundtrip() {
        let resp = SourceResponse::Error(SourceError::InvalidRequest {
            message: "bad name".to_string(),
        });
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::Error(e @ SourceError::InvalidRequest { .. }) => {
                assert_eq!(e.to_string(), "invalid request: bad name");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_import_status_roundtrip() {
        let status = sample_import_status();
        let resp = SourceResponse::ImportStatus(Some(status.clone()));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::ImportStatus(Some(s)) => assert_eq!(s, status),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn import_phase_failed_roundtrip() {
        let phase = ImportPhase::Failed {
            message: "boom".to_string(),
        };
        let json = serde_json::to_string(&phase).unwrap();
        let parsed: ImportPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, phase);
    }

    #[test]
    fn source_error_import_in_progress_roundtrip() {
        let resp = SourceResponse::Error(SourceError::ImportInProgress { job_id: 9 });
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: SourceResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            SourceResponse::Error(e @ SourceError::ImportInProgress { .. }) => {
                assert_eq!(e.to_string(), "import already running: job 9");
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
