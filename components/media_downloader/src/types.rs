// components/media_downloader/src/types.rs
use crate::organization::TrackLocation;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("Required dependency not found: {0}")]
    DependencyNotFound(&'static str),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Download failed: {0}")]
    DownloadFailed(String),

    #[error("Format conversion failed: {0}")]
    FormatError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("Playlist error: {0}")]
    PlaylistError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackMetadata {
    /// Location information (artist, album, title)
    pub location: TrackLocation,

    /// Duration in seconds
    pub duration: f64,

    /// Original URL the track was downloaded from
    pub source_url: String,

    /// When the track was downloaded
    pub download_time: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait Downloader {
    /// Check if the downloader is available and has all required dependencies
    async fn check_available(&self) -> Result<(), DownloadError>;

    /// Fetch metadata about a track without downloading it
    async fn fetch_metadata(
        &self,
        url: &url::Url,
        temp_dir: &std::path::Path,
    ) -> Result<TrackMetadata, DownloadError>;

    /// Download a track and convert it to FLAC format
    async fn download_audio(
        &self,
        url: &url::Url,
        output: &std::path::Path,
        temp_dir: &std::path::Path,
    ) -> Result<(), DownloadError>;
    
    /// Fetch all track URLs from a playlist
    async fn fetch_playlist_urls(&self, url: &url::Url) -> Result<Vec<String>, DownloadError>;
}
