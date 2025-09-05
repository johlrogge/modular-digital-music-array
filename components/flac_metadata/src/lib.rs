use std::path::Path;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MetadataError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Audio format error: {0}")]
    Format(String),

    #[error("Metadata not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone)]
pub struct TrackMetadata {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub duration: Option<Duration>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub file_path: std::path::PathBuf,
}

impl TrackMetadata {
    pub fn format_duration(&self) -> String {
        match self.duration {
            Some(duration) => {
                let total_seconds = duration.as_secs();
                let minutes = total_seconds / 60;
                let seconds = total_seconds % 60;
                format!("{}:{:02}", minutes, seconds)
            }
            None => "Unknown".to_string(),
        }
    }
}

pub fn extract_metadata(path: impl AsRef<Path>) -> Result<TrackMetadata, MetadataError> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let path = path.as_ref();
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("flac");

    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| MetadataError::Format(e.to_string()))?;

    let track = probed
        .format
        .default_track()
        .ok_or_else(|| MetadataError::NotFound("No default track found".into()))?;

    // Extract basic audio properties
    let sample_rate = track.codec_params.sample_rate;
    let channels = track.codec_params.channels.map(|c| c.count() as u16);

    // Calculate duration
    let duration = if let (Some(sr), Some(frames)) = (sample_rate, track.codec_params.n_frames) {
        Some(Duration::from_secs_f64(frames as f64 / sr as f64))
    } else {
        None
    };

    // Extract metadata tags
    let metadata = probed.metadata.get();
    let mut artist = None;
    let mut album = None;
    let mut title = None;

    if let Some(metadata) = metadata {
        if let Some(current) = metadata.current() {
            for tag in current.tags() {
                match tag.key.as_str() {
                    "ARTIST" | "Artist" => artist = Some(tag.value.to_string()),
                    "ALBUM" | "Album" => album = Some(tag.value.to_string()),
                    "TITLE" | "Title" => title = Some(tag.value.to_string()),
                    _ => {}
                }
            }
        }
    }

    Ok(TrackMetadata {
        artist,
        album,
        title,
        duration,
        sample_rate,
        channels,
        file_path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_formatting() {
        let metadata = TrackMetadata {
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            title: Some("Test Title".to_string()),
            duration: Some(Duration::from_secs(291)), // 4:51
            sample_rate: Some(44100),
            channels: Some(2),
            file_path: "/test/path.flac".into(),
        };

        assert_eq!(metadata.format_duration(), "4:51");
    }

    #[test]
    fn test_unknown_duration_formatting() {
        let metadata = TrackMetadata {
            artist: None,
            album: None,
            title: None,
            duration: None,
            sample_rate: None,
            channels: None,
            file_path: "/test/path.flac".into(),
        };

        assert_eq!(metadata.format_duration(), "Unknown");
    }
}
