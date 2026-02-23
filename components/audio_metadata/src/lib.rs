use lofty::{Accessor, AudioFile, ItemKey, LoftyError, Probe, TaggedFileExt};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MetadataError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Lofty error: {0}")]
    Lofty(#[from] LoftyError),

    #[error("Metadata not found: {0}")]
    NotFound(String),

    #[error("Parse error: {0}")]
    ParseError(String),
}

#[derive(Debug, Clone)]
pub struct TrackMetadata {
    // Basic metadata
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,

    // Time and format
    pub duration: Option<Duration>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bit_depth: Option<u8>,
    pub bitrate: Option<u32>,

    // DJ-specific metadata
    pub bpm: Option<f32>,
    pub key: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub comment: Option<String>,

    // File info
    pub file_path: std::path::PathBuf,
    pub file_size_bytes: Option<u64>,
    pub has_picture: bool,
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

    /// Check if essential metadata is missing
    pub fn is_incomplete(&self) -> bool {
        self.artist.is_none() || self.title.is_none()
    }

    /// Get a display name for the track
    pub fn display_name(&self) -> String {
        match (&self.artist, &self.title) {
            (Some(artist), Some(title)) => format!("{} - {}", artist, title),
            (Some(artist), None) => artist.clone(),
            (None, Some(title)) => title.clone(),
            (None, None) => self
                .file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string(),
        }
    }
}

pub fn extract_metadata(path: impl AsRef<Path>) -> Result<TrackMetadata, MetadataError> {
    let path = path.as_ref();

    // Get file size
    let file_size_bytes = std::fs::metadata(path).ok().map(|m| m.len());

    // Probe and read the audio file
    let tagged_file = Probe::open(path)?.read()?;

    // Get audio properties
    let properties = tagged_file.properties();

    // Try to get the primary tag, fall back to first available tag
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    let mut metadata = TrackMetadata {
        file_path: path.to_path_buf(),
        file_size_bytes,
        duration: Some(properties.duration()),
        sample_rate: properties.sample_rate(),
        channels: properties.channels().map(|c| c as u16),
        bit_depth: properties.bit_depth(),
        bitrate: properties.overall_bitrate(),
        has_picture: false,
        // Initialize all other fields as None
        artist: None,
        album: None,
        title: None,
        album_artist: None,
        track_number: None,
        disc_number: None,
        bpm: None,
        key: None,
        genre: None,
        year: None,
        comment: None,
    };

    // Extract tag information if available
    if let Some(tag) = tag {
        // Basic metadata
        metadata.artist = tag.artist().map(|s| s.to_string());
        metadata.album = tag.album().map(|s| s.to_string());
        metadata.title = tag.title().map(|s| s.to_string());
        metadata.album_artist = tag.get_string(&ItemKey::AlbumArtist).map(|s| s.to_string());
        metadata.genre = tag.genre().map(|s| s.to_string());
        metadata.year = tag.year();
        metadata.comment = tag.comment().map(|s| s.to_string());
        metadata.track_number = tag.track();
        metadata.disc_number = tag.disk();

        // Check for pictures/album art
        metadata.has_picture = tag.picture_count() > 0;

        // DJ-specific metadata - try multiple possible tag keys
        // BPM can be stored in different fields
        metadata.bpm = tag
            .get_string(&ItemKey::Unknown("BPM".to_string()))
            .or_else(|| tag.get_string(&ItemKey::Unknown("TBPM".to_string())))
            .and_then(|s| s.parse::<f32>().ok());

        // Key can be stored in different fields depending on the tagger
        metadata.key = tag
            .get_string(&ItemKey::Unknown("INITIALKEY".to_string()))
            .or_else(|| tag.get_string(&ItemKey::Unknown("KEY".to_string())))
            .or_else(|| tag.get_string(&ItemKey::Unknown("TKEY".to_string())))
            .map(|s| s.to_string());
    }

    Ok(metadata)
}

pub fn discover_all_fields(
    path: impl AsRef<Path>,
) -> Result<HashMap<String, String>, MetadataError> {
    let path = path.as_ref();
    let tagged_file = Probe::open(path)?.read()?;
    let mut all_fields = HashMap::new();

    // Add file system information
    if let Ok(file_meta) = std::fs::metadata(path) {
        all_fields.insert("file.size_bytes".to_string(), file_meta.len().to_string());
        all_fields.insert(
            "file.size_mb".to_string(),
            format!("{:.2}", file_meta.len() as f64 / 1_048_576.0),
        );
    }

    // Add audio properties
    let properties = tagged_file.properties();
    all_fields.insert(
        "audio.duration_seconds".to_string(),
        format!("{:.3}", properties.duration().as_secs_f64()),
    );

    if let Some(bitrate) = properties.overall_bitrate() {
        all_fields.insert("audio.bitrate".to_string(), bitrate.to_string());
    }

    if let Some(sample_rate) = properties.sample_rate() {
        all_fields.insert("audio.sample_rate".to_string(), sample_rate.to_string());
    }

    if let Some(bit_depth) = properties.bit_depth() {
        all_fields.insert("audio.bit_depth".to_string(), bit_depth.to_string());
    }

    if let Some(channels) = properties.channels() {
        all_fields.insert("audio.channels".to_string(), channels.to_string());
    }

    // Get ALL tags from all available tag types
    for tag in tagged_file.tags() {
        let tag_type = format!("{:?}", tag.tag_type());

        // Standard accessors
        if let Some(artist) = tag.artist() {
            all_fields.insert(format!("{}.artist", tag_type), artist.to_string());
        }
        if let Some(album) = tag.album() {
            all_fields.insert(format!("{}.album", tag_type), album.to_string());
        }
        if let Some(title) = tag.title() {
            all_fields.insert(format!("{}.title", tag_type), title.to_string());
        }

        // Iterate through all items in the tag
        for item in tag.items() {
            let key = format!("{}.{:?}", tag_type, item.key());
            let value = item.value().text().unwrap_or("(binary data)").to_string();
            all_fields.insert(key, value);
        }

        // Check for pictures
        if tag.picture_count() > 0 {
            all_fields.insert(
                format!("{}.picture_count", tag_type),
                tag.picture_count().to_string(),
            );

            for (i, picture) in tag.pictures().iter().enumerate() {
                all_fields.insert(
                    format!("{}.picture_{}", tag_type, i),
                    format!("{:?}, {} bytes", picture.pic_type(), picture.data().len()),
                );
            }
        }
    }

    Ok(all_fields)
}

/// Try to extract metadata from file path when tags are missing
pub fn infer_from_path(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let components: Vec<_> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    if components.len() < 3 {
        return (None, None, None);
    }

    // Common patterns:
    // .../Artist/Album/Track.flac
    // .../Artist/Album (Year)/Artist - Title.flac
    let artist = Some(components[components.len() - 3].to_string());

    // Clean up album - remove year in parentheses if present
    let album_raw = components[components.len() - 2];
    let album = Some(
        album_raw
            .split(" (")
            .next()
            .unwrap_or(album_raw)
            .to_string(),
    );

    // Extract title from filename
    let mut title = None;
    if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
        // Try to clean up common patterns like "Artist - Title"
        if let Some(ref artist_name) = artist {
            if filename.starts_with(artist_name) {
                title = Some(
                    filename
                        .strip_prefix(artist_name)
                        .unwrap_or(filename)
                        .trim_start_matches(" - ")
                        .to_string(),
                );
            } else if filename.contains(" - ") {
                // Take everything after the last " - "
                title = filename.rsplit(" - ").next().map(|s| s.to_string());
            } else {
                title = Some(filename.to_string());
            }
        } else {
            title = Some(filename.to_string());
        }
    }

    (artist, album, title)
}
