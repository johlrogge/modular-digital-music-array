//! Data types for Bandcamp API

use chrono::{DateTime, Utc};
use music_facts::{Artist, Title, Year};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Fan/user ID on Bandcamp
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct FanId(pub String);

impl fmt::Display for FanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Item ID (sale/purchase ID) on Bandcamp
/// Format: "p123456" for purchases or "t789" for tracks
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ItemId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Type of item (album or single track)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemType {
    Album,
    Track,
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemType::Album => write!(f, "album"),
            ItemType::Track => write!(f, "track"),
        }
    }
}

/// Item in user's Bandcamp collection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionItem {
    /// Unique ID for this purchase/item
    pub id: ItemId,
    /// Artist name
    pub artist: Artist,
    /// Album or track title
    pub title: Title,
    /// Whether this is an album or single track
    pub item_type: ItemType,
    /// When the item was purchased
    pub purchased: Option<DateTime<Utc>>,
    /// URL to download the item
    pub download_url: String,
}

/// Track information within an album
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    /// Track title
    pub title: Title,
    /// Track number (1-indexed)
    pub track_num: Option<u32>,
    /// Duration in seconds
    pub duration_secs: Option<u32>,
}

/// Detailed item information with download links
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalItem {
    /// Artist name
    pub artist: Artist,
    /// Album or track title
    pub title: Title,
    /// Whether this is an album or single track
    pub item_type: ItemType,
    /// Release year
    pub release_year: Option<Year>,
    /// Available download formats mapped to URLs
    pub formats: HashMap<AudioFormat, String>,
    /// Tracks in the item (for albums)
    pub tracks: Vec<TrackInfo>,
}

/// Audio format options for download
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioFormat {
    Flac,
    Wav,
    AacHi,
    Mp3_320,
    AiffLossless,
    Vorbis,
    Mp3V0,
    Alac,
}

impl AudioFormat {
    /// Bandcamp's internal format name
    pub fn bandcamp_name(&self) -> &'static str {
        match self {
            AudioFormat::Flac => "flac",
            AudioFormat::Wav => "wav",
            AudioFormat::AacHi => "aac-hi",
            AudioFormat::Mp3_320 => "mp3-320",
            AudioFormat::AiffLossless => "aiff-lossless",
            AudioFormat::Vorbis => "vorbis",
            AudioFormat::Mp3V0 => "mp3-v0",
            AudioFormat::Alac => "alac",
        }
    }

    /// Parse from Bandcamp's format name
    pub fn from_bandcamp_name(name: &str) -> Option<Self> {
        match name {
            "flac" => Some(AudioFormat::Flac),
            "wav" => Some(AudioFormat::Wav),
            "aac-hi" => Some(AudioFormat::AacHi),
            "mp3-320" => Some(AudioFormat::Mp3_320),
            "aiff-lossless" => Some(AudioFormat::AiffLossless),
            "vorbis" => Some(AudioFormat::Vorbis),
            "mp3-v0" => Some(AudioFormat::Mp3V0),
            "alac" => Some(AudioFormat::Alac),
            _ => None,
        }
    }

    /// File extension for this format
    pub fn extension(&self) -> &'static str {
        match self {
            AudioFormat::Flac => "flac",
            AudioFormat::Wav => "wav",
            AudioFormat::AacHi => "m4a",
            AudioFormat::Mp3_320 | AudioFormat::Mp3V0 => "mp3",
            AudioFormat::AiffLossless => "aiff",
            AudioFormat::Vorbis => "ogg",
            AudioFormat::Alac => "m4a",
        }
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.bandcamp_name())
    }
}

/// Download progress information
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    /// Bytes downloaded so far
    pub downloaded: u64,
    /// Total bytes (if known)
    pub total: Option<u64>,
}

impl DownloadProgress {
    pub fn percentage(&self) -> Option<f32> {
        self.total
            .map(|t| (self.downloaded as f32 / t as f32) * 100.0)
    }
}

/// Events emitted during download
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    /// Download started with optional total size
    Started { total: Option<u64> },
    /// Progress update
    Progress(DownloadProgress),
    /// Download completed successfully
    Completed { path: std::path::PathBuf },
    /// Download failed
    Failed { error: String },
}

// Internal types for parsing Bandcamp's JSON responses

/// Parsed data from user's Bandcamp collection page
#[derive(Debug, Deserialize)]
pub(crate) struct ParsedFanpageData {
    pub fan_data: FanData,
    pub collection_data: CollectionData,
    pub hidden_data: CollectionData,
    pub item_cache: ItemCache,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FanData {
    pub fan_id: serde_json::Value, // Can be string or number
    pub is_own_page: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CollectionData {
    pub batch_size: Option<u16>,
    pub item_count: Option<u16>,
    pub last_token: Option<String>,
    pub redownload_urls: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ItemCache {
    pub collection: HashMap<String, CachedItem>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CachedItem {
    pub sale_item_id: u64,
    pub sale_item_type: String,
    pub band_name: String,
    pub item_title: String,
    pub purchased: Option<String>,
}

/// Response from collection API pagination
#[derive(Debug, Deserialize)]
pub(crate) struct ParsedCollectionItems {
    pub more_available: bool,
    pub last_token: String,
    pub redownload_urls: HashMap<String, String>,
    pub items: Vec<CachedItem>,
}

/// Digital item details from download page
#[derive(Debug, Deserialize)]
pub(crate) struct ParsedDigitalItem {
    pub downloads: Option<HashMap<String, DigitalItemDownload>>,
    pub package_release_date: Option<String>,
    pub title: String,
    pub artist: String,
    pub download_type: Option<String>,
    pub download_type_str: String,
    pub item_type: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DigitalItemDownload {
    pub url: String,
}
