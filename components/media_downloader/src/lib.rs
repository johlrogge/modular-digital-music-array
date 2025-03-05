// components/media_downloader/src/lib.rs
mod organization;
mod types;
mod ytdlp;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::fs;
use url::Url;

pub use crate::organization::TrackLocation;
pub use crate::types::{DownloadError, Downloader, TrackMetadata};
use crate::ytdlp::YtDlp;

pub struct MediaDownloader {
    download_path: PathBuf,
    temp_path: PathBuf,
    downloader: Arc<dyn Downloader + Send + Sync>,
}

impl MediaDownloader {
    pub async fn new(download_path: impl AsRef<Path>) -> Result<Self, DownloadError> {
        Self::new_with_downloader(download_path, Arc::new(YtDlp)).await
    }

    pub async fn new_with_downloader(
        download_path: impl AsRef<Path>,
        downloader: Arc<dyn Downloader + Send + Sync>,
    ) -> Result<Self, DownloadError> {
        downloader.check_available().await?;

        let download_path =
            dunce::canonicalize(download_path.as_ref()).map_err(DownloadError::IoError)?;
        let temp_path = download_path.join("temp");

        // Create directories if they don't exist
        fs::create_dir_all(&download_path).await?;
        fs::create_dir_all(&temp_path).await?;

        Ok(Self {
            download_path,
            temp_path,
            downloader,
        })
    }

    pub async fn download(&self, url: &str) -> Result<(PathBuf, TrackMetadata), DownloadError> {
        let url = Url::parse(url).map_err(|e| DownloadError::InvalidUrl(e.to_string()))?;

        let temp_dir = TempDir::new_in(&self.temp_path)?;

        // Get metadata first
        let metadata = self
            .downloader
            .fetch_metadata(&url, temp_dir.path())
            .await?;
        let final_path = metadata.location.to_path(&self.download_path);

        // Create parent directories if they don't exist
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Download directly to final location
        self.downloader
            .download_audio(&url, &final_path, temp_dir.path())
            .await?;

        Ok((final_path, metadata))
    }
    
    pub async fn download_playlist(&self, url: &str) -> Result<Vec<(PathBuf, TrackMetadata)>, DownloadError> {
        let url = Url::parse(url).map_err(|e| DownloadError::InvalidUrl(e.to_string()))?;
        
        // Get list of tracks in the playlist
        let track_urls = self.downloader.fetch_playlist_urls(&url).await?;
        
        let mut results = Vec::new();
        
        // Download each track
        for track_url in track_urls {
            match self.download(&track_url).await {
                Ok((path, metadata)) => {
                    results.push((path, metadata));
                }
                Err(e) => {
                    eprintln!("Error downloading track {}: {}", track_url, e);
                    // Continue with other tracks even if one fails
                }
            }
        }
        
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    struct TestDownloader;

    #[async_trait::async_trait]
    impl Downloader for TestDownloader {
        async fn check_available(&self) -> Result<(), DownloadError> {
            Ok(())
        }

        async fn fetch_metadata(
            &self,
            _url: &Url,
            _temp_dir: &Path,
        ) -> Result<TrackMetadata, DownloadError> {
            Ok(TrackMetadata {
                location: TrackLocation::new("Test Artist", "Test Song"),
                duration: 180.0,
                source_url: "https://example.com".to_string(),
                download_time: chrono::Utc::now(),
            })
        }

        async fn download_audio(
            &self,
            _url: &Url,
            output: &Path,
            _temp_dir: &Path,
        ) -> Result<(), DownloadError> {
            // Simulate file creation
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).await?;
            }
            fs::write(output, b"test data").await?;
            Ok(())
        }
        
        async fn fetch_playlist_urls(&self, _url: &Url) -> Result<Vec<String>, DownloadError> {
            Ok(vec![
                "https://example.com/track1".to_string(),
                "https://example.com/track2".to_string(),
            ])
        }
    }

    #[tokio::test]
    async fn test_download_creates_directories() -> Result<(), DownloadError> {
        let temp = tempdir()?;
        let downloader =
            MediaDownloader::new_with_downloader(temp.path(), Arc::new(TestDownloader)).await?;

        let (path, _) = downloader.download("https://example.com/test").await?;

        assert!(path.exists());
        assert!(path.parent().unwrap().exists());

        Ok(())
    }
}
