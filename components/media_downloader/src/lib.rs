// components/media_downloader/src/lib.rs
mod types;
mod utils;
mod ytdlp;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use url::Url;

pub use types::{DownloadError, TrackMetadata};
use utils::generate_filename;
use ytdlp::{Downloader, YtDlp};

pub struct MediaDownloader {
    download_path: PathBuf,
    temp_path: PathBuf,
    downloader: Arc<dyn Downloader + Send + Sync>,
}

impl MediaDownloader {
    /// Create a new MediaDownloader that will store files in the given directory
    pub async fn new(download_path: impl AsRef<Path>) -> Result<Self, DownloadError> {
        Self::new_with_downloader(download_path, Arc::new(YtDlp)).await
    }

    /// Create a new MediaDownloader with a specific downloader implementation
    pub async fn new_with_downloader(
        download_path: impl AsRef<Path>,
        downloader: Arc<dyn Downloader + Send + Sync>,
    ) -> Result<Self, DownloadError> {
        downloader.check_available().await?;

        let download_path = download_path.as_ref().to_owned();
        let temp_path = download_path.join("temp");

        // Create directories if they don't exist
        tokio::fs::create_dir_all(&download_path).await?;
        tokio::fs::create_dir_all(&temp_path).await?;

        Ok(Self {
            download_path,
            temp_path,
            downloader,
        })
    }

    /// Download a track from a URL, returning its path and metadata
    pub async fn download(&self, url: &str) -> Result<(PathBuf, TrackMetadata), DownloadError> {
        // Validate URL
        let url = Url::parse(url)
            .map_err(|e| DownloadError::InvalidUrl(e.to_string()))?;

        // Create temporary directory for download
        let temp_dir = TempDir::new_in(&self.temp_path)?;

        // Get metadata first
        let metadata = self.downloader.fetch_metadata(&url, temp_dir.path()).await?;
        
        // Generate output filename
        let final_path = self.download_path.join(
            generate_filename(&metadata.title, url.as_str())
        );

        // Download the file
        self.downloader.download_audio(&url, &final_path, temp_dir.path()).await?;

        Ok((final_path, metadata))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use ytdlp::stub::DownloaderStub;

    #[tokio::test]
    async fn test_downloader_creation() {
        let temp_dir = TempDir::new().unwrap();
        let downloader = MediaDownloader::new_with_downloader(
            temp_dir.path(),
            Arc::new(DownloaderStub)
        ).await;
        
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
            )
        }
    }

    #[tokio::test]
    async fn test_download() {
        let temp_dir = TempDir::new().unwrap();
        let downloader = MediaDownloader::new_with_downloader(
            temp_dir.path(),
            Arc::new(DownloaderStub)
        ).await.unwrap();

        let result = downloader.download("https://example.com/test").await;
        assert!(
            result.is_ok(),
            "Download failed with error: {:?}",
            result.err().unwrap()
        );

        let (path, metadata) = result.unwrap();
        assert_eq!(metadata.title, "Test Song");
        assert_eq!(metadata.artist.as_deref(), Some("Test Artist"));
    }
}
