//! IPC Protocol types for mdma-library
//!
//! Pure types with no network dependencies. Shared between:
//! - mdma-library (server)
//! - library-ipc-client (used by CLI and console)

// Re-export types used in the protocol so clients don't need to depend on music-facts
pub use music_facts::{Bpm, ContentHash, DurationSeconds, Key};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// Security Newtypes
// ============================================================================

/// Validated inbox path - prevents path traversal attacks.
///
/// Only allows relative paths within the inbox directory.
/// Rejects: absolute paths, "..", null bytes, empty strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InboxPath(String);

/// Errors when constructing an InboxPath
#[derive(Debug, Clone, Error)]
pub enum InboxPathError {
    #[error("inbox path cannot be empty")]
    Empty,
    #[error("inbox path cannot be absolute")]
    AbsolutePath,
    #[error("inbox path cannot contain '..'")]
    PathTraversal,
    #[error("inbox path cannot contain null bytes")]
    NullByte,
}

impl InboxPath {
    /// Create a new InboxPath after validation.
    pub fn new(name: &str) -> Result<Self, InboxPathError> {
        if name.is_empty() {
            return Err(InboxPathError::Empty);
        }
        if name.starts_with('/') {
            return Err(InboxPathError::AbsolutePath);
        }
        if name.contains("..") {
            return Err(InboxPathError::PathTraversal);
        }
        if name.contains('\0') {
            return Err(InboxPathError::NullByte);
        }
        Ok(Self(name.to_string()))
    }

    /// Get the path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InboxPath {
    type Error = InboxPathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<InboxPath> for String {
    fn from(p: InboxPath) -> String {
        p.0
    }
}

impl std::fmt::Display for InboxPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// Protocol Errors
// ============================================================================

/// Typed errors returned by the library service.
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum ProtocolError {
    #[error("track not found: {hash}")]
    TrackNotFound { hash: String },

    #[error("inbox file not found: {path}")]
    InboxFileNotFound { path: String },

    #[error("ingestion failed: {message}")]
    IngestionFailed { message: String },

    #[error("internal error: {message}")]
    Internal { message: String },
}

// ============================================================================
// Request Types
// ============================================================================

/// Requests that can be sent to the library service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LibraryRequest {
    /// Ping to check if service is alive.
    Ping,

    /// Get service status.
    GetStatus,

    /// List all tracks (optionally limited).
    ListTracks { limit: Option<usize> },

    /// Get a specific track by content hash (supports partial hashes).
    GetTrack { hash: ContentHash },

    /// Get all facts for a track (supports partial hashes).
    GetFacts { hash: ContentHash },

    /// Search tracks by query string.
    Search { query: String },

    /// Get files currently in inbox queue.
    GetInboxQueue,

    /// Ingest a specific file from inbox.
    IngestFile { path: InboxPath },

    /// Delete a file from inbox without ingesting.
    DeleteInboxFile { path: InboxPath },

    /// Ingest all files in inbox.
    IngestAll,
}

// ============================================================================
// Response Types
// ============================================================================

/// Responses from the library service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum LibraryResponse {
    /// Pong response to Ping.
    Pong,

    /// Service status.
    Status(ServiceStatus),

    /// List of tracks.
    Tracks(Vec<TrackInfo>),

    /// Single track.
    Track(TrackInfo),

    /// All facts for a track (type, value pairs).
    Facts {
        hash: ContentHash,
        facts: Vec<(String, String)>,
    },

    /// Search results.
    SearchResults(Vec<TrackInfo>),

    /// Inbox queue contents.
    InboxQueue(Vec<InboxPath>),

    /// Result of single file ingestion.
    IngestResult(IngestResult),

    /// Results of ingest-all operation.
    IngestAllResult(Vec<IngestAllItem>),

    /// Error response.
    Error(ProtocolError),
}

// ============================================================================
// Data Types
// ============================================================================

/// Track information for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub content_hash: ContentHash,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<DurationSeconds>,
    pub bpm: Option<Bpm>,
    pub key: Option<Key>,
    /// Relative blob path (no absolute paths in protocol).
    pub blob_path: Option<String>,
}

/// Service status information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub version: String,
    pub tracks_indexed: usize,
    pub facts_count: usize,
    pub inbox_queue_size: usize,
    pub uptime_seconds: u64,
}

/// Result of a single file ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub hash: Option<ContentHash>,
    pub success: bool,
    pub message: String,
}

/// Result item for ingest-all operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestAllItem {
    pub path: InboxPath,
    pub result: IngestResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_path_valid() {
        assert!(InboxPath::new("track.flac").is_ok());
        assert!(InboxPath::new("subdir/track.flac").is_ok());
        assert!(InboxPath::new("a/b/c/track.flac").is_ok());
    }

    #[test]
    fn inbox_path_rejects_empty() {
        assert!(matches!(InboxPath::new(""), Err(InboxPathError::Empty)));
    }

    #[test]
    fn inbox_path_rejects_absolute() {
        assert!(matches!(
            InboxPath::new("/etc/passwd"),
            Err(InboxPathError::AbsolutePath)
        ));
    }

    #[test]
    fn inbox_path_rejects_traversal() {
        assert!(matches!(
            InboxPath::new("../../../etc/passwd"),
            Err(InboxPathError::PathTraversal)
        ));
        assert!(matches!(
            InboxPath::new("foo/../bar"),
            Err(InboxPathError::PathTraversal)
        ));
    }

    #[test]
    fn inbox_path_rejects_null() {
        assert!(matches!(
            InboxPath::new("foo\0bar"),
            Err(InboxPathError::NullByte)
        ));
    }

    #[test]
    fn inbox_path_serde_roundtrip() {
        let path = InboxPath::new("track.flac").unwrap();
        let json = serde_json::to_string(&path).unwrap();
        assert_eq!(json, "\"track.flac\"");

        let parsed: InboxPath = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_str(), "track.flac");
    }

    #[test]
    fn inbox_path_serde_rejects_invalid() {
        // Malicious JSON should fail deserialization
        let result: Result<InboxPath, _> = serde_json::from_str("\"../../../etc/passwd\"");
        assert!(result.is_err());
    }

    #[test]
    fn request_serialize() {
        let req = LibraryRequest::GetStatus;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("GetStatus"));
    }

    #[test]
    fn response_serialize() {
        let resp = LibraryResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Pong"));
    }

    #[test]
    fn track_info_roundtrip() {
        let track = TrackInfo {
            content_hash: ContentHash("sha256:abc123".to_string()),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: None,
            duration: Some(DurationSeconds(180)),
            bpm: Some(Bpm::from_u32(128).unwrap()),
            key: None,
            blob_path: Some("ab/abc123.flac".to_string()),
        };

        let json = serde_json::to_string(&track).unwrap();
        let parsed: TrackInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, track.title);
        assert_eq!(parsed.content_hash.0, track.content_hash.0);
    }
}
