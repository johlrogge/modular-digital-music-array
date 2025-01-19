// components/media_downloader/src/types.rs
use crate::organization::TrackLocation;
use thiserror::Error;
use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};

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
    async fn fetch_metadata(&self, url: &url::Url, temp_dir: &std::path::Path) 
        -> Result<TrackMetadata, DownloadError>;
    
    /// Download a track and convert it to FLAC format
    async fn download_audio(&self, url: &url::Url, output: &std::path::Path, temp_dir: &std::path::Path) 
        -> Result<(), DownloadError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_track_metadata_serialization() {
        let location = TrackLocation::new(
            "Test Artist",
            "Test Song",
        );
        
        let metadata = TrackMetadata {
            location,
            duration: 180.5,
            source_url: "https://example.com/song".to_string(),
            download_time: Utc::now(),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        let decoded: TrackMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.location.artist, "Test Artist");
        assert_eq!(decoded.location.title, "Test Song");
        assert_eq!(decoded.duration, 180.5);
        assert_eq!(decoded.source_url, "https://example.com/song");
    }

    #[test]
    fn test_download_errors() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let error = DownloadError::IoError(io_error);
        assert!(error.to_string().contains("file not found"));

        let error = DownloadError::DependencyNotFound("yt-dlp");
        assert!(error.to_string().contains("yt-dlp"));

        let error = DownloadError::InvalidUrl("bad://url".to_string());
        assert!(error.to_string().contains("bad://url"));
    }
}
