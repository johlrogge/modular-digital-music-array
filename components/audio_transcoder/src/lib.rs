mod ffmpeg;

use std::path::Path;

use thiserror::Error;

// ── Public types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    /// Uncompressed PCM, big-endian — CDJ/Rekordbox standard
    Aiff,
    /// Uncompressed PCM, little-endian — universal compatibility
    Wav,
    /// Lossless compressed — archival / smaller exports
    Flac,
}

impl ExportFormat {
    /// Return the file extension (without leading dot) for this format.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Aiff => "aiff",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }

    /// Return the file suffix (with leading dot) for this format.
    pub fn suffix(&self) -> &'static str {
        match self {
            Self::Aiff => ".aiff",
            Self::Wav => ".wav",
            Self::Flac => ".flac",
        }
    }
}

/// Metadata to inject into exported files.
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
}

// ── Error ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum TranscoderError {
    #[error("IO error: {0}")]
    Io(std::io::Error),

    #[error("ffmpeg not found on PATH — install ffmpeg to enable audio export")]
    FfmpegNotFound,

    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_format_extension_aiff() {
        assert_eq!(ExportFormat::Aiff.extension(), "aiff");
    }

    #[test]
    fn export_format_extension_wav() {
        assert_eq!(ExportFormat::Wav.extension(), "wav");
    }

    #[test]
    fn export_format_extension_flac() {
        assert_eq!(ExportFormat::Flac.extension(), "flac");
    }

    #[test]
    fn export_format_suffix_aiff() {
        assert_eq!(ExportFormat::Aiff.suffix(), ".aiff");
    }

    #[test]
    fn export_format_suffix_wav() {
        assert_eq!(ExportFormat::Wav.suffix(), ".wav");
    }

    #[test]
    fn export_format_suffix_flac() {
        assert_eq!(ExportFormat::Flac.suffix(), ".flac");
    }

    #[test]
    fn export_format_suffix_has_leading_dot() {
        for fmt in [ExportFormat::Aiff, ExportFormat::Wav, ExportFormat::Flac] {
            assert!(
                fmt.suffix().starts_with('.'),
                "suffix() must start with '.' for {:?}",
                fmt
            );
        }
    }

    #[test]
    fn export_format_extension_matches_suffix_without_dot() {
        for fmt in [ExportFormat::Aiff, ExportFormat::Wav, ExportFormat::Flac] {
            assert_eq!(
                fmt.extension(),
                &fmt.suffix()[1..],
                "extension() must equal suffix() without leading dot for {:?}",
                fmt
            );
        }
    }
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Transcode `source` audio to `output` in the given `format`, embedding `meta`
/// tags and passing through cover art from the source via ffmpeg.
///
/// ffmpeg must be available on `$PATH`. Cover art is preserved automatically
/// via `-map_metadata 0`.
pub fn transcode(
    source: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &TrackMetadata,
) -> Result<(), TranscoderError> {
    ffmpeg::run_ffmpeg(source, output, format, meta)
}
