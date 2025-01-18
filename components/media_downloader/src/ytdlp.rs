// components/media_downloader/src/ytdlp.rs
use std::path::{Path, PathBuf};
use tokio::process::Command;
use url::Url;
use crate::types::{DownloadError, TrackMetadata};
use async_trait::async_trait;
use serde::Deserialize;

#[async_trait]
pub trait Downloader {
    async fn check_available(&self) -> Result<(), DownloadError>;
    async fn fetch_metadata(&self, url: &Url, temp_dir: &Path) -> Result<TrackMetadata, DownloadError>;
    async fn download_audio(&self, url: &Url, output: &Path, temp_dir: &Path) -> Result<(), DownloadError>;
}

pub struct YtDlp;

#[async_trait]
impl Downloader for YtDlp {
    async fn check_available(&self) -> Result<(), DownloadError> {
        which::which("yt-dlp")
            .map(|_| ())
            .map_err(|_| DownloadError::YtDlpNotFound)
    }

    async fn fetch_metadata(&self, url: &Url, temp_dir: &Path) -> Result<TrackMetadata, DownloadError> {
        let abs_temp_dir = temp_dir.canonicalize()
            .map_err(|e| DownloadError::IoError(e))?;
        println!("Fetching metadata using temp dir (absolute): {}", abs_temp_dir.display());
        
        let output = Command::new("yt-dlp")
            .arg("--dump-json")
            .arg("--no-download")
            .arg(url.as_str())
            .current_dir(&abs_temp_dir)
            .output()
            .await?;

        if !output.status.success() {
            return Err(DownloadError::DownloadFailed(
                String::from_utf8_lossy(&output.stderr).into_owned()
            ));
        }

        let yt_meta: YtDlpMetadata = serde_json::from_slice(&output.stdout)
            .map_err(|e| DownloadError::DownloadFailed(e.to_string()))?;

        println!("{:#?}", yt_meta);

        Ok(TrackMetadata {
            title: yt_meta.title,
            artist: yt_meta.uploader,
            duration: yt_meta.duration,
            source_url: yt_meta.webpage_url,
            download_time: chrono::Utc::now(),
        })
    }

    async fn download_audio(&self, url: &Url, output: &Path, temp_dir: &Path) -> Result<(), DownloadError> {
        let abs_output = output.canonicalize().unwrap_or_else(|_| output.to_path_buf());
        let abs_temp_dir = temp_dir.canonicalize()
            .map_err(|e| DownloadError::IoError(e))?;
            
        println!("Downloading to absolute path: {}", abs_output.display());
        println!("Using absolute temp dir: {}", abs_temp_dir.display());

        let output_str = output.to_str().ok_or_else(|| 
            DownloadError::DownloadFailed("Invalid output path".to_string())
        )?;

        let status = Command::new("yt-dlp")
            .arg("-x")
            .arg("--audio-format").arg("flac")
            .arg("--audio-quality").arg("0")
            .arg("--format").arg("bestaudio")
            .arg("-o").arg(output_str)
            .arg(url.as_str())
            .current_dir(&abs_temp_dir)
            .status()
            .await?;

        if !status.success() {
            return Err(DownloadError::DownloadFailed(
                format!("yt-dlp exited with status: {}", status)
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct YtDlpMetadata {
    title: String,
    uploader: Option<String>,
    duration: f64,
    webpage_url: String,
}

#[cfg(test)]
pub mod stub {
    use super::*;

    pub struct DownloaderStub;

    #[async_trait]
    impl Downloader for DownloaderStub {
        async fn check_available(&self) -> Result<(), DownloadError> {
            Ok(())
        }

        async fn fetch_metadata(&self, _url: &Url, _temp_dir: &Path) -> Result<TrackMetadata, DownloadError> {
            Ok(TrackMetadata {
                title: "Test Song".to_string(),
                artist: Some("Test Artist".to_string()),
                duration: 180.0,
                source_url: "https://example.com/test".to_string(),
                download_time: chrono::Utc::now(),
            })
        }

        async fn download_audio(&self, _url: &Url, _output: &Path, _temp_dir: &Path) -> Result<(), DownloadError> {
            Ok(())
        }
    }
}
