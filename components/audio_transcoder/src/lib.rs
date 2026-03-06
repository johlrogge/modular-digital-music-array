mod ffmpeg;

use std::path::Path;

use thiserror::Error;

// ── Public types ───────────────────────────────────────────────────────────────

/// Whether an audio format is lossless or lossy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatCategory {
    Lossless,
    Lossy,
}

impl FormatCategory {
    /// Classify a file extension (without leading dot) into a format category.
    /// Returns `None` for unrecognised extensions.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "flac" | "wav" | "aiff" | "aif" => Some(Self::Lossless),
            // m4a can contain ALAC (lossless) or AAC (lossy), but we cannot
            // distinguish from the extension alone; default to lossy.
            "mp3" | "ogg" | "opus" | "aac" | "m4a" | "wma" => Some(Self::Lossy),
            _ => None,
        }
    }
}

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
pub struct ExportMetadata {
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
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case(ExportFormat::Aiff, "aiff")]
    #[case(ExportFormat::Wav, "wav")]
    #[case(ExportFormat::Flac, "flac")]
    fn export_format_extension(#[case] fmt: ExportFormat, #[case] expected: &str) {
        assert_eq!(fmt.extension(), expected);
    }

    #[rstest]
    #[case(ExportFormat::Aiff, ".aiff")]
    #[case(ExportFormat::Wav, ".wav")]
    #[case(ExportFormat::Flac, ".flac")]
    fn export_format_suffix(#[case] fmt: ExportFormat, #[case] expected: &str) {
        assert_eq!(fmt.suffix(), expected);
    }

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn export_format_suffix_has_leading_dot(#[case] fmt: ExportFormat) {
        assert!(
            fmt.suffix().starts_with('.'),
            "suffix() must start with '.' for {:?}",
            fmt
        );
    }

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn export_format_extension_matches_suffix_without_dot(#[case] fmt: ExportFormat) {
        assert_eq!(
            fmt.extension(),
            &fmt.suffix()[1..],
            "extension() must equal suffix() without leading dot for {:?}",
            fmt
        );
    }

    // ── FormatCategory ────────────────────────────────────────────────────────

    #[rstest]
    #[case("flac", Some(FormatCategory::Lossless))]
    #[case("wav", Some(FormatCategory::Lossless))]
    #[case("aiff", Some(FormatCategory::Lossless))]
    #[case("mp3", Some(FormatCategory::Lossy))]
    #[case("ogg", Some(FormatCategory::Lossy))]
    #[case("opus", Some(FormatCategory::Lossy))]
    #[case("txt", None)]
    #[case("FLAC", Some(FormatCategory::Lossless))]
    #[case("MP3", Some(FormatCategory::Lossy))]
    fn format_category_from_extension(#[case] ext: &str, #[case] expected: Option<FormatCategory>) {
        assert_eq!(FormatCategory::from_extension(ext), expected);
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
    meta: &ExportMetadata,
) -> Result<(), TranscoderError> {
    ffmpeg::run_ffmpeg(source, output, format, meta)
}
