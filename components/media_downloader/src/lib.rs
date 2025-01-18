// components/media_downloader/src/lib.rs
mod types;
mod utils;
mod ytdlp;

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use url::Url;

pub use types::{DownloadError, TrackMetadata};
use utils::generate_filename;
use ytdlp::YtDlp;

pub struct MediaDownloader {
    download_path: PathBuf,
    temp_path: PathBuf,
}

impl MediaDownloader {
    /// Create a new MediaDownloader that will store files in the given directory
    pub async fn new(download_path: impl AsRef<Path>) -> Result<Self, DownloadError> {
        // Verify yt-dlp is installed
        YtDlp::check_available()?;

        let download_path = download_path.as_ref().to_owned();
        let temp_path = download_path.join("temp");

        // Create directories if they don't exist
        tokio::fs::create_dir_all(&download_path).await?;
        tokio::fs::create_dir_all(&temp_path).await?;

        Ok(Self {
            download_path,
            temp_path,
        })
    }

    /// Download a track from a URL, returning its path and metadata
    pub async fn download(&self, url: &str) -> Result<(PathBuf, TrackMetadata), DownloadError> {
        // Validate URL
        let url = Url::parse(url).map_err(|e| DownloadError::InvalidUrl(e.to_string()))?;

        // Create temporary directory for download
        let temp_dir = TempDir::new_in(&self.temp_path)?;

        // Get metadata first
        let metadata = YtDlp::fetch_metadata(&url, temp_dir.path()).await?;

        // Generate output filename
        let final_path = self
            .download_path
            .join(generate_filename(&metadata.title, url.as_str()));

        // Download the file
        YtDlp::download_audio(&url, &final_path, temp_dir.path()).await?;

        Ok((final_path, metadata))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    #[ignore]
    async fn test_downloader_creation() {
        let temp_dir = TempDir::new().unwrap();
        let downloader = MediaDownloader::new(temp_dir.path()).await;

        assert!(
            downloader.is_ok(),
            "Downloader creation failed with error: {:?}",
            downloader.err().unwrap()
        );

        let temp_path = temp_dir.path().join("temp");
        match fs::metadata(&temp_path) {
            Ok(_) => (),
            Err(e) => panic!(
                "Temp directory '{}' was not created: {}",
                temp_path.display(),
                e
            ),
        }
    }
}
