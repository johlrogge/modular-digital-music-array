// components/media_downloader/src/lib.rs
mod types;
mod utils;
mod ytdlp;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use url::Url;
use tokio::fs;

pub use crate::types::{DownloadError, TrackMetadata};
use crate::utils::generate_filename;
use crate::ytdlp::{Downloader, YtDlp};

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

        let download_path = dunce::canonicalize(download_path.as_ref())
            .map_err(|e| DownloadError::IoError(e))?;
        let temp_path = download_path.join("temp");

        // Create directories if they don't exist
        println!("Creating directories:");
        println!("Download path (absolute): {}", download_path.display());
        println!("Temp path (absolute): {}", temp_path.display());
        
        fs::create_dir_all(&download_path).await?;
        fs::create_dir_all(&temp_path).await?;

        Ok(Self {
            download_path,
            temp_path,
            downloader,
        })
    }

    pub async fn download(&self, url: &str) -> Result<(PathBuf, TrackMetadata), DownloadError> {
        let url = Url::parse(url)
            .map_err(|e| DownloadError::InvalidUrl(e.to_string()))?;

        println!("Created temporary directory in (absolute): {}", self.temp_path.display());
        let temp_dir = TempDir::new_in(&self.temp_path)?;
        println!("Temp dir absolute path: {}", temp_dir.path().canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "Failed to get absolute path".to_string()));

        // Get metadata first
        let metadata = self.downloader.fetch_metadata(&url, &temp_dir.path()).await?;
        println!("Got metadata: {:#?}", metadata);

        // Generate final filename and path
        let final_name = generate_filename(&metadata.title, url.as_str());
        let final_path = self.download_path.join(&final_name);
        println!("Final absolute path will be: {}", final_path.display());

        // Download directly to final location
        println!("Starting download...");
        self.downloader.download_audio(&url, &final_path, temp_dir.path()).await?;
        
        // Verify file exists
        match fs::metadata(&final_path).await {
            Ok(metadata) => println!("File exists at final absolute path: {}, size: {} bytes", 
                final_path.display(), metadata.len()),
            Err(e) => println!("Error checking final file at {}: {}", final_path.display(), e),
        }

        Ok((final_path, metadata))
    }
}
