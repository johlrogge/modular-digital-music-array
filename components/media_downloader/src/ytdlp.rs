// components/media_downloader/src/ytdlp.rs
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use serde::Deserialize;
use url::Url;
use crate::types::{DownloadError, TrackMetadata};

#[derive(Deserialize)]
struct YtDlpMetadata {
    title: String,
    uploader: Option<String>,
    duration: f64,
    webpage_url: String,
}

pub struct YtDlp;

impl YtDlp {
    /// Verify yt-dlp is available
    pub fn check_available() -> Result<(), DownloadError> {
        which::which("yt-dlp")
            .map(|_| ())
            .map_err(|_| DownloadError::YtDlpNotFound)
    }

    /// Fetch metadata for a URL without downloading
    pub async fn fetch_metadata(url: &Url, temp_dir: &Path) -> Result<TrackMetadata, DownloadError> {
        let output = Command::new("yt-dlp")
            .arg("--dump-json")
            .arg("--no-download")
            .arg(url.as_str())
            .current_dir(temp_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            return Err(DownloadError::DownloadFailed(
                String::from_utf8_lossy(&output.stderr).into_owned()
            ));
        }

        let yt_meta: YtDlpMetadata = serde_json::from_slice(&output.stdout)
            .map_err(|e| DownloadError::DownloadFailed(e.to_string()))?;

        Ok(TrackMetadata {
            title: yt_meta.title,
            artist: yt_meta.uploader,
            duration: yt_meta.duration,
            source_url: yt_meta.webpage_url,
            download_time: chrono::Utc::now(),
        })
    }

    /// Download audio from a URL to the specified output path
    pub async fn download_audio(url: &Url, output: &Path, temp_dir: &Path) -> Result<(), DownloadError> {
        let status = Command::new("yt-dlp")
            .arg("-x")                          // Extract audio
            .arg("--audio-format").arg("flac")  // Use FLAC for lossless quality
            .arg("--audio-quality").arg("0")    // Best quality
            .arg("--format").arg("bestaudio")   // Get best audio source
            .arg("-o").arg(output)
            .arg(url.as_str())
            .current_dir(temp_dir)
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
