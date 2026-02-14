//! Typestate pipeline for music ingestion
//!
//! Each stage takes ownership of the previous, preventing dangling path pointers.
//! Files are processed path-based (not loaded into memory).
//!
//! ```text
//! InboxFile → ValidatedAudio → ExtractedTrack → IndexedTrack
//! ```

use music_facts::{ContentHash, FactSource, MusicValue};
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during ingestion
#[derive(Debug, Error)]
pub enum IngestError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Unsupported audio format: {0}")]
    UnsupportedFormat(String),

    #[error("Failed to compute hash: {0}")]
    HashError(String),

    #[error("Failed to extract metadata: {0}")]
    MetadataError(String),

    #[error("Duplicate track (already indexed): {}", .0.0)]
    Duplicate(ContentHash),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Source of the upload (for tracking provenance)
#[derive(Debug, Clone)]
pub enum UploadSource {
    /// Dropped into inbox via SMB/NFS
    NetworkShare,
    /// Uploaded via HTTP
    HttpUpload,
    /// Downloaded from Bandcamp
    BandcampDownload { artist_url: Option<String> },
    /// Extracted from Beatport zip
    BeatportZip { order_id: Option<String> },
}

/// Supported audio formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Flac,
    Mp3,
    Aiff,
    Wav,
}

impl AudioFormat {
    /// Detect format from file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "flac" => Some(Self::Flac),
            "mp3" => Some(Self::Mp3),
            "aiff" | "aif" => Some(Self::Aiff),
            "wav" => Some(Self::Wav),
            _ => None,
        }
    }

    /// Get canonical file extension
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
            Self::Aiff => "aiff",
            Self::Wav => "wav",
        }
    }
}

// =============================================================================
// Stage 1: InboxFile - A file sitting in the inbox
// =============================================================================

/// A file in the inbox, not yet validated
pub struct InboxFile {
    pub path: PathBuf,
    pub source: UploadSource,
}

impl InboxFile {
    /// Create from a path in the inbox
    pub fn new(path: PathBuf, source: UploadSource) -> Self {
        Self { path, source }
    }

    /// Validate the file and compute its hash
    /// Consumes self, transferring ownership to ValidatedAudio
    pub fn validate(self) -> Result<ValidatedAudio, IngestError> {
        // Check file exists
        if !self.path.exists() {
            return Err(IngestError::FileNotFound(self.path));
        }

        // Detect format from extension
        let ext = self.path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let format = AudioFormat::from_extension(ext)
            .ok_or_else(|| IngestError::UnsupportedFormat(ext.to_string()))?;

        // Compute content hash (streams file, doesn't load into memory)
        let content_hash = compute_hash(&self.path)?;

        Ok(ValidatedAudio {
            path: self.path,
            format,
            content_hash,
            source: self.source,
        })
    }
}

// =============================================================================
// Stage 2: ValidatedAudio - Format verified, hash computed
// =============================================================================

/// A validated audio file with known format and content hash
pub struct ValidatedAudio {
    pub path: PathBuf,
    pub format: AudioFormat,
    pub content_hash: ContentHash,
    pub source: UploadSource,
}

impl ValidatedAudio {
    /// Extract metadata from the audio file
    /// Consumes self, transferring ownership to ExtractedTrack
    pub fn extract_metadata(self) -> Result<ExtractedTrack, IngestError> {
        // Only read metadata, not audio data
        let facts = extract_facts(&self.path, &self.content_hash, self.format)?;

        Ok(ExtractedTrack {
            path: self.path,
            format: self.format,
            content_hash: self.content_hash,
            facts,
            source: self.source,
        })
    }
}

// =============================================================================
// Stage 3: ExtractedTrack - Metadata extracted as facts
// =============================================================================

/// A track with extracted metadata facts
pub struct ExtractedTrack {
    pub path: PathBuf,
    pub format: AudioFormat,
    pub content_hash: ContentHash,
    pub facts: Vec<(MusicValue, FactSource)>,
    pub source: UploadSource,
}

impl ExtractedTrack {
    /// Import the track into the library
    /// Moves file to blob storage and creates symlinks
    /// Consumes self, transferring ownership to IndexedTrack
    pub fn import(self, music_dir: &std::path::Path) -> Result<IndexedTrack, IngestError> {
        let hash_str = self
            .content_hash
            .0
            .strip_prefix("sha256:")
            .unwrap_or(&self.content_hash.0);

        // Blob path: /music/blobs/a1/b2c3d4...sha256.flac
        let blob_dir = music_dir.join("blobs").join(&hash_str[..2]);
        let blob_path = blob_dir.join(format!("{}.{}", hash_str, self.format.extension()));

        // Create blob directory
        std::fs::create_dir_all(&blob_dir)?;

        // Move file to blob storage (atomic rename, no data copy)
        std::fs::rename(&self.path, &blob_path)?;

        // Create human-readable symlink for debugging
        let symlink_path = create_symlink(&self.facts, &blob_path, music_dir)?;

        Ok(IndexedTrack {
            blob_path,
            symlink_path,
            content_hash: self.content_hash,
        })
    }
}

// =============================================================================
// Stage 4: IndexedTrack - File imported, ready for querying
// =============================================================================

/// A track that has been imported into the library
pub struct IndexedTrack {
    pub blob_path: PathBuf,
    pub symlink_path: Option<PathBuf>,
    pub content_hash: ContentHash,
}

// =============================================================================
// Helper functions (to be implemented)
// =============================================================================

/// Compute SHA256 hash of file contents (streaming, not loading into memory)
fn compute_hash(path: &std::path::Path) -> Result<ContentHash, IngestError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    let hash_string = hex::encode(result);
    Ok(ContentHash(format!("sha256:{}", hash_string)))
}

/// Extract metadata facts from audio file
fn extract_facts(
    path: &std::path::Path,
    content_hash: &ContentHash,
    format: AudioFormat,
) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
    crate::fact_generator::generate_facts(path, content_hash, format)
}

/// Create human-readable symlink in by-artist directory
fn create_symlink(
    facts: &[(MusicValue, FactSource)],
    blob_path: &std::path::Path,
    music_dir: &std::path::Path,
) -> Result<Option<PathBuf>, IngestError> {
    // Extract artist/album/title from facts
    let mut artist = None;
    let mut album = None;
    let mut title = None;
    let mut track_num = None;

    for (value, _source) in facts {
        match value {
            MusicValue::Artist(a) => artist = Some(a.0.clone()),
            MusicValue::Album(a) => album = Some(a.0.clone()),
            MusicValue::Title(t) => title = Some(t.0.clone()),
            MusicValue::TrackNumber(n) => track_num = Some(n.0),
            _ => {}
        }
    }

    // Need at least artist and title for symlink
    let (artist, title) = match (artist, title) {
        (Some(a), Some(t)) => (a, t),
        _ => return Ok(None), // Can't create symlink without artist/title
    };

    // Build symlink path: /music/by-artist/Artist/Album/01 - Title.flac
    let album_str = album.unwrap_or_else(|| "Unknown Album".to_string());
    let symlink_dir = music_dir.join("by-artist").join(&artist).join(&album_str);

    // Filename: "01 - Title.flac" or just "Title.flac"
    let filename = match track_num {
        Some(n) => format!(
            "{:02} - {}.{}",
            n,
            title,
            blob_path.extension().unwrap_or_default().to_string_lossy()
        ),
        None => format!(
            "{}.{}",
            title,
            blob_path.extension().unwrap_or_default().to_string_lossy()
        ),
    };

    let symlink_path = symlink_dir.join(&filename);

    // Create directory
    std::fs::create_dir_all(&symlink_dir)?;

    // Compute relative path from symlink to blob
    // e.g., from /music/by-artist/Artist/Album/ to /music/blobs/a1/hash.flac
    // = ../../../blobs/a1/hash.flac
    let relative_blob =
        pathdiff::diff_paths(blob_path, &symlink_dir).unwrap_or_else(|| blob_path.to_path_buf());

    // Create symlink (ignore if exists - could be re-import)
    match std::os::unix::fs::symlink(&relative_blob, &symlink_path) {
        Ok(()) => Ok(Some(symlink_path)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(Some(symlink_path)),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_format_from_extension() {
        assert_eq!(AudioFormat::from_extension("flac"), Some(AudioFormat::Flac));
        assert_eq!(AudioFormat::from_extension("FLAC"), Some(AudioFormat::Flac));
        assert_eq!(AudioFormat::from_extension("mp3"), Some(AudioFormat::Mp3));
        assert_eq!(AudioFormat::from_extension("aiff"), Some(AudioFormat::Aiff));
        assert_eq!(AudioFormat::from_extension("aif"), Some(AudioFormat::Aiff));
        assert_eq!(AudioFormat::from_extension("wav"), Some(AudioFormat::Wav));
        assert_eq!(AudioFormat::from_extension("ogg"), None);
    }
}
