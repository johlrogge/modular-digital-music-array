// components/media_downloader/src/ytdlp.rs
use crate::organization::TrackLocation;
use crate::types::{DownloadError, Downloader, TrackMetadata};
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;
use url::Url;

#[derive(Debug, Deserialize)]
struct YtDlpMetadata {
    title: String,
    uploader: Option<String>,
    album: Option<String>,
    duration: f64,
    webpage_url: String,
    //usrformat_id: String,
    //ext: String,
    //release_date: Option<String>,
    //track: Option<String>,
    artist: Option<String>,
    creator: Option<String>,
}

pub struct YtDlp;

impl YtDlp {
    /// Verify that ffmpeg is available for format conversion
    async fn check_ffmpeg() -> Result<(), DownloadError> {
        which::which("ffmpeg")
            .map(|_| ())
            .map_err(|_| DownloadError::DependencyNotFound("ffmpeg"))
    }

    /// Select best audio format and ensure FLAC conversion
    fn get_format_args() -> Vec<String> {
        vec![
            // Extract audio only
            "-x".to_string(),
            // Select best audio quality source
            "--format".to_string(),
            "bestaudio".to_string(),
            // Force FLAC output regardless of input
            "--audio-format".to_string(),
            "flac".to_string(),
            // Best quality conversion
            "--audio-quality".to_string(),
            "0".to_string(),
            // Post-process with FFmpeg for reliable conversion
            "--postprocessor-args".to_string(),
            "-acodec flac -compression_level 12".to_string(),
        ]
    }
}

#[async_trait]
impl Downloader for YtDlp {
    async fn check_available(&self) -> Result<(), DownloadError> {
        // Check both yt-dlp and ffmpeg are available
        which::which("yt-dlp").map_err(|_| DownloadError::DependencyNotFound("yt-dlp"))?;
        Self::check_ffmpeg().await?;
        Ok(())
    }

    async fn fetch_metadata(
        &self,
        url: &Url,
        temp_dir: &Path,
    ) -> Result<TrackMetadata, DownloadError> {
        let output = Command::new("yt-dlp")
            .arg("--dump-json")
            .arg("--no-download")
            .arg(url.as_str())
            .current_dir(temp_dir)
            .output()
            .await?;

        if !output.status.success() {
            return Err(DownloadError::DownloadFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        let yt_meta: YtDlpMetadata = serde_json::from_slice(&output.stdout)
            .map_err(|e| DownloadError::DownloadFailed(e.to_string()))?;

        // Try to determine the artist from various metadata fields
        let artist = yt_meta
            .artist
            .or(yt_meta.creator)
            .or(yt_meta.uploader)
            .unwrap_or_else(|| "Unknown Artist".to_string());

        let location = if let Some(album) = yt_meta.album {
            TrackLocation::with_album(artist, album, yt_meta.title)
        } else {
            TrackLocation::new(artist, yt_meta.title)
        };

        Ok(TrackMetadata {
            location,
            duration: yt_meta.duration,
            source_url: yt_meta.webpage_url,
            download_time: Utc::now(),
        })
    }

    async fn download_audio(
        &self,
        url: &Url,
        output: &Path,
        temp_dir: &Path,
    ) -> Result<(), DownloadError> {
        let mut cmd = Command::new("yt-dlp");

        // Add all format-related arguments
        cmd.args(Self::get_format_args());

        // Add output and URL arguments
        cmd.arg("-o")
            .arg(
                output.to_str().ok_or_else(|| {
                    DownloadError::DownloadFailed("Invalid output path".to_string())
                })?,
            )
            .arg(url.as_str())
            .current_dir(temp_dir);

        let status = cmd.status().await?;

        if !status.success() {
            return Err(DownloadError::DownloadFailed(format!(
                "yt-dlp exited with status: {}",
                status
            )));
        }

        // Verify the output file exists
        if !output.exists() {
            return Err(DownloadError::DownloadFailed(
                "Output file not created".to_string(),
            ));
        }

        Ok(())
    }
}
