//! IPC Protocol types for mdma-library
//!
//! Pure types with no network dependencies. Shared between:
//! - mdma-library (server)
//! - library-ipc-client (used by CLI and console)

// Re-export types used in the protocol so clients don't need to depend on music-facts
pub use music_facts::{Bpm, ContentHash, DurationSeconds, Key, MusicValue};
// Re-export query types so clients can use them without depending on library-search directly
pub use library_search::{
    CamelotLetter, DurationQuery, DurationUnit, KeyQuery, NumericQuery, StringQuery, TrackQuery,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

// ============================================================================
// FactType Newtype
// ============================================================================

/// Typed fact name (e.g. "artist", "genre", "isrc").
///
/// Replaces bare `String` in request/response types to prevent accidental
/// confusion between a fact type name and a fact value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactType(String);

impl FactType {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

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
// Playlist Name Validation
// ============================================================================

/// Validated playlist name — only allows `[a-zA-Z0-9_-]`, rejects empty strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PlaylistName(String);

/// Errors when constructing a PlaylistName
#[derive(Debug, Clone, Error)]
pub enum PlaylistNameError {
    #[error("playlist name cannot be empty")]
    Empty,
    #[error(
        "playlist name contains invalid characters (only a-z, A-Z, 0-9, _, - allowed): {name}"
    )]
    InvalidCharacters { name: String },
}

impl PlaylistName {
    pub fn new(name: &str) -> Result<Self, PlaylistNameError> {
        if name.is_empty() {
            return Err(PlaylistNameError::Empty);
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(PlaylistNameError::InvalidCharacters {
                name: name.to_string(),
            });
        }
        Ok(Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PlaylistName {
    type Error = PlaylistNameError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<PlaylistName> for String {
    fn from(p: PlaylistName) -> String {
        p.0
    }
}

impl std::fmt::Display for PlaylistName {
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

    #[error("playlist not found: {name}")]
    PlaylistNotFound { name: String },

    #[error("playlist already exists: {name}")]
    PlaylistAlreadyExists { name: String },

    #[error("invalid playlist name: {name}")]
    InvalidPlaylistName { name: String },
}

// ============================================================================
// Ingest Source Metadata
// ============================================================================

/// Source metadata for provenance tracking during ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source_type")]
pub enum IngestSource {
    /// Downloaded from Bandcamp
    Bandcamp {
        item_id: String,
        artist_url: Option<String>,
    },
    /// Extracted from Beatport
    Beatport { order_id: Option<String> },
    /// Uploaded via HTTP or dropped into inbox
    Upload,
}

// ============================================================================
// Request Types
// ============================================================================

/// Requests that can be sent to the library service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
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

    /// Search tracks by structured query.
    Search { query: TrackQuery },

    /// Get all distinct values stored for a given fact type.
    /// Returns a sorted list usable for discovery (e.g. all genres, all labels).
    GetFactValues { fact_type: FactType },

    /// Get files currently in inbox queue.
    GetInboxQueue,

    /// Ingest a specific file from inbox.
    IngestFile {
        path: InboxPath,
        /// Optional source metadata for provenance tracking.
        source: Option<IngestSource>,
    },

    /// Delete a file from inbox without ingesting.
    DeleteInboxFile { path: InboxPath },

    /// Ingest all files in inbox.
    IngestAll,

    /// Check if any track has a fact matching the given type and value.
    HasFact { fact_type: FactType, value: String },

    /// Batch check: which of these values exist for a given fact type?
    HasFacts {
        fact_type: FactType,
        values: Vec<String>,
    },

    /// List all playlist names.
    PlaylistList,

    /// Get playlist content verbatim.
    PlaylistGet { name: PlaylistName },

    /// Create a new playlist (fails if it already exists).
    PlaylistNew { name: PlaylistName, content: String },

    /// Append content to an existing playlist.
    PlaylistAppend { name: PlaylistName, content: String },

    /// Replace (overwrite) a playlist's content.
    PlaylistReplace { name: PlaylistName, content: String },

    /// Remove a playlist.
    PlaylistRemove { name: PlaylistName },

    /// Rename a playlist.
    PlaylistRename {
        from: PlaylistName,
        to: PlaylistName,
    },

    /// Re-extract cover art for tracks that don't have a CoverArtPath fact yet.
    ReindexCovers,

    /// Write a bookmark fact for a track.
    WriteBookmark {
        hash: ContentHash,
        scope: Option<String>,
    },

    /// Write a single fact for a track. Used for importing metadata from external sources.
    WriteFact { hash: ContentHash, fact: MusicValue },

    /// Retract all facts whose FactSource.source_name matches `source_name` for every
    /// ContentHash that has an ItemId fact equal to `item_id`.
    RetractSourceFacts {
        item_id: String,
        source_name: String,
    },

    /// Look up the album title(s) stored for any track tagged with this ItemId.
    /// If multiple tracks have different album titles for the same ItemId, returns
    /// one value (the first encountered during iteration).
    GetAlbumTitleByItemId { item_id: String },

    /// Count the number of tracks in the library whose facts include `ItemId = item_id`.
    GetTrackCountForItemId { item_id: String },
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

    /// Sorted distinct values for a requested fact type.
    FactValues(Vec<String>),

    /// Whether a single fact exists.
    FactExists {
        fact_type: FactType,
        value: String,
        exists: bool,
    },

    /// Batch result: which values exist for a given fact type.
    FactsExist {
        fact_type: FactType,
        existing: Vec<String>,
    },

    /// List of playlist names.
    PlaylistNames(Vec<PlaylistName>),

    /// Verbatim playlist content.
    PlaylistContent(String),

    /// Bookmark written successfully.
    BookmarkWritten,

    /// Fact written successfully.
    FactWritten,

    /// Source facts retracted successfully.
    SourceFactsRetracted,

    /// Album title for a given ItemId (None if no tracks with that ItemId have an album title).
    AlbumTitleByItemId(Option<String>),

    /// Number of tracks in the library whose facts include a given ItemId.
    TrackCountForItemId(usize),

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
    /// Relative path to cover art image (e.g. "cover-art/<hash>.jpg"). No absolute paths.
    pub cover_art_path: Option<String>,
    /// Track number on album (from tags).
    pub track_number: Option<u32>,
    /// Disc number on a multi-disc release (from tags).
    pub disc_number: Option<u32>,
    /// ISO 8601 datetime when track was added to the library.
    pub added: Option<String>,
    /// ISO 8601 datetime when the track was last started (played). None if never played.
    #[serde(default)]
    pub started: Option<String>,
    /// ISO 8601 datetime when the track was last stopped. None if never played.
    #[serde(default)]
    pub stopped: Option<String>,
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
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IngestResult {
    Success {
        hash: Option<ContentHash>,
        message: String,
    },
    Failure {
        message: String,
    },
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
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case("track.flac")]
    #[case("subdir/track.flac")]
    #[case("a/b/c/track.flac")]
    fn inbox_path_valid(#[case] path: &str) {
        assert!(InboxPath::new(path).is_ok());
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

    #[rstest]
    #[case("../../../etc/passwd")]
    #[case("foo/../bar")]
    fn inbox_path_rejects_traversal(#[case] path: &str) {
        assert!(matches!(
            InboxPath::new(path),
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

    #[rstest]
    #[case("techno-set")]
    #[case("My_Playlist_2")]
    #[case("a")]
    fn playlist_name_valid(#[case] name: &str) {
        assert!(PlaylistName::new(name).is_ok());
    }

    #[test]
    fn playlist_name_rejects_empty() {
        assert!(matches!(
            PlaylistName::new(""),
            Err(PlaylistNameError::Empty)
        ));
    }

    #[rstest]
    #[case("foo/bar")]
    #[case("foo bar")]
    #[case("../etc")]
    #[case("name.plist")]
    fn playlist_name_rejects_invalid_chars(#[case] name: &str) {
        assert!(PlaylistName::new(name).is_err());
    }

    #[test]
    fn playlist_name_serde_roundtrip() {
        let name = PlaylistName::new("my-set").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"my-set\"");
        let parsed: PlaylistName = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_str(), "my-set");
    }

    #[test]
    fn playlist_name_serde_rejects_invalid() {
        let result: Result<PlaylistName, _> = serde_json::from_str("\"../../../etc\"");
        assert!(result.is_err());
    }

    #[test]
    fn write_fact_request_roundtrip() {
        use music_facts::{Bpm, MusicValue};
        let hash = ContentHash::new("sha256:abc");
        let fact = MusicValue::Bpm(Bpm::from_u32(128).unwrap());
        let req = LibraryRequest::WriteFact {
            hash: hash.clone(),
            fact: fact.clone(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: LibraryRequest = serde_json::from_str(&json).unwrap();
        if let LibraryRequest::WriteFact { hash: h, fact: f } = decoded {
            assert_eq!(h.as_str(), hash.as_str());
            assert_eq!(f, fact);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn fact_written_response_roundtrip() {
        let resp = LibraryResponse::FactWritten;
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: LibraryResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, LibraryResponse::FactWritten));
    }

    #[test]
    fn retract_source_facts_request_roundtrip() {
        let req = LibraryRequest::RetractSourceFacts {
            item_id: "p123456".to_string(),
            source_name: "bandcamp".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: LibraryRequest = serde_json::from_str(&json).unwrap();
        match decoded {
            LibraryRequest::RetractSourceFacts {
                item_id,
                source_name,
            } => {
                assert_eq!(item_id, "p123456");
                assert_eq!(source_name, "bandcamp");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn get_album_title_by_item_id_request_roundtrip() {
        let req = LibraryRequest::GetAlbumTitleByItemId {
            item_id: "p123456".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: LibraryRequest = serde_json::from_str(&json).unwrap();
        match decoded {
            LibraryRequest::GetAlbumTitleByItemId { item_id } => {
                assert_eq!(item_id, "p123456");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn source_facts_retracted_response_roundtrip() {
        let resp = LibraryResponse::SourceFactsRetracted;
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: LibraryResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, LibraryResponse::SourceFactsRetracted));
    }

    #[test]
    fn album_title_by_item_id_response_some_roundtrip() {
        let resp = LibraryResponse::AlbumTitleByItemId(Some("My Album".to_string()));
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: LibraryResponse = serde_json::from_str(&json).unwrap();
        match decoded {
            LibraryResponse::AlbumTitleByItemId(Some(title)) => {
                assert_eq!(title, "My Album");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn album_title_by_item_id_response_none_roundtrip() {
        let resp = LibraryResponse::AlbumTitleByItemId(None);
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: LibraryResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, LibraryResponse::AlbumTitleByItemId(None)));
    }

    #[test]
    fn get_track_count_for_item_id_request_roundtrip() {
        let req = LibraryRequest::GetTrackCountForItemId {
            item_id: "p123456".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: LibraryRequest = serde_json::from_str(&json).unwrap();
        match decoded {
            LibraryRequest::GetTrackCountForItemId { item_id } => {
                assert_eq!(item_id, "p123456");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn track_count_for_item_id_response_roundtrip() {
        let resp = LibraryResponse::TrackCountForItemId(3);
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: LibraryResponse = serde_json::from_str(&json).unwrap();
        match decoded {
            LibraryResponse::TrackCountForItemId(count) => assert_eq!(count, 3),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn track_count_for_item_id_zero_roundtrip() {
        let resp = LibraryResponse::TrackCountForItemId(0);
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: LibraryResponse = serde_json::from_str(&json).unwrap();
        match decoded {
            LibraryResponse::TrackCountForItemId(count) => assert_eq!(count, 0),
            _ => panic!("wrong variant"),
        }
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
    fn ingest_result_success_serialization() {
        let hash = ContentHash::new("sha256:abc");
        let result = IngestResult::Success {
            hash: Some(hash.clone()),
            message: "ingested".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: IngestResult = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, IngestResult::Success { .. }));
        if let IngestResult::Success {
            hash: h,
            message: m,
        } = decoded
        {
            assert_eq!(h, Some(hash));
            assert_eq!(m, "ingested");
        }
    }

    #[test]
    fn ingest_result_failure_serialization() {
        let result = IngestResult::Failure {
            message: "something failed".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: IngestResult = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, IngestResult::Failure { .. }));
        if let IngestResult::Failure { message } = decoded {
            assert_eq!(message, "something failed");
        }
    }

    #[test]
    fn track_info_roundtrip() {
        let track = TrackInfo {
            content_hash: ContentHash::new("sha256:abc123"),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: None,
            duration: Some(DurationSeconds::new(180)),
            bpm: Some(Bpm::from_u32(128).unwrap()),
            key: None,
            blob_path: Some("ab/abc123.flac".to_string()),
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        };

        let json = serde_json::to_string(&track).unwrap();
        let parsed: TrackInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, track.title);
        assert_eq!(parsed.content_hash.as_str(), track.content_hash.as_str());
    }
}
