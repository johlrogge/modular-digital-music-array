// components/media_downloader/src/types.rs
use serde::{Serialize, Deserialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("yt-dlp not found in PATH")]
    YtDlpNotFound,
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Download failed: {0}")]
    DownloadFailed(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: Option<String>,
    pub duration: f64,
    pub source_url: String,
    pub download_time: chrono::DateTime<chrono::Utc>,
}
