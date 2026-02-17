//! IPC Protocol types for mdma-bandcamp
//!
//! Pure types with no network dependencies. Shared between:
//! - mdma-bandcamp (server)
//! - CLI and console (clients)

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// Security Newtypes
// ============================================================================

/// Validated Bandcamp username.
///
/// Bandcamp usernames follow specific rules:
/// - 3-30 characters
/// - Alphanumeric, hyphens, and underscores only
/// - Cannot start or end with hyphen/underscore
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BandcampUsername(String);

/// Errors when constructing a BandcampUsername
#[derive(Debug, Clone, Error)]
pub enum UsernameError {
    #[error("username cannot be empty")]
    Empty,
    #[error("username too short (minimum 3 characters)")]
    TooShort,
    #[error("username too long (maximum 30 characters)")]
    TooLong,
    #[error(
        "username contains invalid characters (only alphanumeric, hyphens, underscores allowed)"
    )]
    InvalidChars,
    #[error("username cannot start or end with hyphen or underscore")]
    InvalidStartEnd,
}

impl BandcampUsername {
    /// Create a new BandcampUsername after validation.
    pub fn new(name: &str) -> Result<Self, UsernameError> {
        if name.is_empty() {
            return Err(UsernameError::Empty);
        }
        if name.len() < 3 {
            return Err(UsernameError::TooShort);
        }
        if name.len() > 30 {
            return Err(UsernameError::TooLong);
        }

        // Check for valid characters
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(UsernameError::InvalidChars);
        }

        // Check start/end
        let first = name.chars().next().unwrap();
        let last = name.chars().last().unwrap();
        if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
            return Err(UsernameError::InvalidStartEnd);
        }

        Ok(Self(name.to_string()))
    }

    /// Get the username as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BandcampUsername {
    type Error = UsernameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<BandcampUsername> for String {
    fn from(u: BandcampUsername) -> String {
        u.0
    }
}

impl std::fmt::Display for BandcampUsername {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Item ID for Bandcamp purchases.
///
/// Format: "p123456" for albums or "t789" for tracks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl ItemId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// Protocol Errors
// ============================================================================

/// Typed errors returned by the bandcamp service.
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum ProtocolError {
    #[error("not authenticated: {message}")]
    NotAuthenticated { message: String },

    #[error("invalid cookies: {message}")]
    InvalidCookies { message: String },

    #[error("collection fetch failed: {message}")]
    CollectionFetchFailed { message: String },

    #[error("download failed: {item_id} - {message}")]
    DownloadFailed { item_id: String, message: String },

    #[error("rate limited, retry after {retry_after_secs} seconds")]
    RateLimited { retry_after_secs: u64 },

    #[error("item not found: {item_id}")]
    ItemNotFound { item_id: String },

    #[error("internal error: {message}")]
    Internal { message: String },
}

// ============================================================================
// Request Types
// ============================================================================

/// Requests that can be sent to the bandcamp service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BandcampRequest {
    /// Ping to check if service is alive.
    Ping,

    /// Get service status.
    GetStatus,

    /// Reload cookies from disk.
    ReloadCookies,

    /// Start syncing a user's collection.
    Sync { username: BandcampUsername },

    /// List current downloads (active + queued).
    ListDownloads,

    /// Cancel a specific download.
    CancelDownload { id: ItemId },

    /// Pause all downloads.
    PauseAll,

    /// Resume downloads.
    ResumeAll,
}

// ============================================================================
// Response Types
// ============================================================================

/// Responses from the bandcamp service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum BandcampResponse {
    /// Pong response to Ping.
    Pong,

    /// Service status.
    Status(ServiceStatus),

    /// Cookies reloaded result.
    CookiesReloaded { valid: bool, message: String },

    /// Sync started.
    SyncStarted {
        username: String,
        total_items: usize,
        new_items: usize,
    },

    /// List of current downloads.
    Downloads(Vec<DownloadStatus>),

    /// Download cancelled.
    Cancelled { id: ItemId },

    /// Downloads paused.
    Paused,

    /// Downloads resumed.
    Resumed,

    /// Error response.
    Error(ProtocolError),
}

// ============================================================================
// Data Types
// ============================================================================

/// Service status information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub version: String,
    pub cookies_loaded: bool,
    pub current_username: Option<String>,
    pub downloads_active: usize,
    pub downloads_queued: usize,
    pub downloads_completed: usize,
    pub downloads_failed: usize,
    pub uptime_seconds: u64,
    pub paused: bool,
}

/// Download status for a single item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadStatus {
    pub id: ItemId,
    pub artist: String,
    pub title: String,
    pub state: DownloadState,
    /// Bytes downloaded so far.
    pub downloaded_bytes: u64,
    /// Total bytes (if known).
    pub total_bytes: Option<u64>,
    /// Error message if failed.
    pub error: Option<String>,
}

/// State of a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    /// Waiting to be downloaded.
    Queued,
    /// Currently downloading.
    Downloading,
    /// Download complete, extracting ZIP.
    Extracting,
    /// Moving files to inbox.
    Moving,
    /// Successfully completed.
    Completed,
    /// Failed with error.
    Failed,
    /// Cancelled by user.
    Cancelled,
}

impl std::fmt::Display for DownloadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadState::Queued => write!(f, "queued"),
            DownloadState::Downloading => write!(f, "downloading"),
            DownloadState::Extracting => write!(f, "extracting"),
            DownloadState::Moving => write!(f, "moving"),
            DownloadState::Completed => write!(f, "completed"),
            DownloadState::Failed => write!(f, "failed"),
            DownloadState::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_valid() {
        assert!(BandcampUsername::new("validuser").is_ok());
        assert!(BandcampUsername::new("user123").is_ok());
        assert!(BandcampUsername::new("user-name").is_ok());
        assert!(BandcampUsername::new("user_name").is_ok());
        assert!(BandcampUsername::new("a1b").is_ok());
    }

    #[test]
    fn username_rejects_empty() {
        assert!(matches!(
            BandcampUsername::new(""),
            Err(UsernameError::Empty)
        ));
    }

    #[test]
    fn username_rejects_too_short() {
        assert!(matches!(
            BandcampUsername::new("ab"),
            Err(UsernameError::TooShort)
        ));
    }

    #[test]
    fn username_rejects_too_long() {
        let long_name = "a".repeat(31);
        assert!(matches!(
            BandcampUsername::new(&long_name),
            Err(UsernameError::TooLong)
        ));
    }

    #[test]
    fn username_rejects_invalid_chars() {
        assert!(matches!(
            BandcampUsername::new("user@name"),
            Err(UsernameError::InvalidChars)
        ));
        assert!(matches!(
            BandcampUsername::new("user name"),
            Err(UsernameError::InvalidChars)
        ));
        assert!(matches!(
            BandcampUsername::new("user.name"),
            Err(UsernameError::InvalidChars)
        ));
    }

    #[test]
    fn username_rejects_invalid_start_end() {
        assert!(matches!(
            BandcampUsername::new("-username"),
            Err(UsernameError::InvalidStartEnd)
        ));
        assert!(matches!(
            BandcampUsername::new("username-"),
            Err(UsernameError::InvalidStartEnd)
        ));
        assert!(matches!(
            BandcampUsername::new("_username"),
            Err(UsernameError::InvalidStartEnd)
        ));
    }

    #[test]
    fn username_serde_roundtrip() {
        let username = BandcampUsername::new("testuser").unwrap();
        let json = serde_json::to_string(&username).unwrap();
        assert_eq!(json, "\"testuser\"");

        let parsed: BandcampUsername = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_str(), "testuser");
    }

    #[test]
    fn username_serde_rejects_invalid() {
        let result: Result<BandcampUsername, _> = serde_json::from_str("\"ab\"");
        assert!(result.is_err());
    }

    #[test]
    fn request_serialize() {
        let req = BandcampRequest::GetStatus;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("GetStatus"));
    }

    #[test]
    fn response_serialize() {
        let resp = BandcampResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Pong"));
    }

    #[test]
    fn download_status_roundtrip() {
        let status = DownloadStatus {
            id: ItemId::new("p123456"),
            artist: "Test Artist".to_string(),
            title: "Test Album".to_string(),
            state: DownloadState::Downloading,
            downloaded_bytes: 1024,
            total_bytes: Some(10240),
            error: None,
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: DownloadStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id.as_str(), "p123456");
        assert_eq!(parsed.state, DownloadState::Downloading);
    }

    #[test]
    fn service_status_roundtrip() {
        let status = ServiceStatus {
            version: "0.1.0".to_string(),
            cookies_loaded: true,
            current_username: Some("testuser".to_string()),
            downloads_active: 1,
            downloads_queued: 5,
            downloads_completed: 10,
            downloads_failed: 2,
            uptime_seconds: 3600,
            paused: false,
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: ServiceStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, "0.1.0");
        assert!(parsed.cookies_loaded);
    }
}
