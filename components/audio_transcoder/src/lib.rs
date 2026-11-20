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

// ── Public API ─────────────────────────────────────────────────────────────────

/// What is known about a source file; the transcoder decides from it the PCM
/// bit depth, the CDJ downsample and the trimming of encoder padding.
///
/// Every field may be unknown: the file is then exported as-is (risk of
/// "illegal format" on CDJs for high-rate sources).
#[derive(Debug, Clone, Default)]
pub struct SourceAudio {
    /// The source's file extension without the dot (`m4a`, `flac`, ...); empty
    /// when unknown.
    pub ext: String,
    /// Sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Bit depth.
    pub bit_depth: Option<u8>,
    /// Duration as the container states it; zero counts as unknown.
    pub duration: Option<std::time::Duration>,
}

/// Transcode `input` audio to `output` in the given `format`, embedding `meta`
/// tags and passing through cover art from the source via ffmpeg.
///
/// `source` describes the input: AIFF/WAV exports are downsampled to ≤ 48 kHz
/// when required by CDJ hardware (logged at `info!` level with the source rate,
/// target rate and file path, so the user can audit which tracks were
/// resampled), are 16-bit for sources of 16 bits or less and 24-bit otherwise,
/// and AAC sources are cut at their container duration, dropping the trailing
/// encoder padding.
///
/// ffmpeg must be available on `$PATH`. Cover art is preserved automatically
/// via `-map_metadata 0`.
pub fn transcode(
    input: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
    source: &SourceAudio,
) -> Result<(), TranscoderError> {
    if let (Some(src_hz), Some(target_hz)) =
        (source.sample_rate, source.cdj_downsample_rate(format))
    {
        tracing::info!(
            source = %input.display(),
            src_hz,
            target_hz,
            "downsampling to CDJ-safe rate"
        );
    }
    ffmpeg::run_ffmpeg(input, output, format, meta, source)
}

/// The bit rate in kbps of uncompressed PCM, as rekordbox writes it to the
/// `BitRate` attribute: `sample_rate × bit_depth × channels / 1000`
/// (1411 for 16-bit/44.1 kHz stereo, 2116 for 24-bit).
pub fn pcm_bitrate_kbps(sample_rate: u32, bit_depth: u8, channels: u16) -> u32 {
    (u64::from(sample_rate) * u64::from(bit_depth) * u64::from(channels) / 1000) as u32
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

    #[rstest]
    #[case(44_100, 16, 2, 1411)]
    #[case(48_000, 16, 2, 1536)]
    #[case(44_100, 24, 2, 2116)]
    #[case(48_000, 24, 2, 2304)]
    #[case(44_100, 16, 1, 705)]
    fn pcm_bitrate(#[case] rate: u32, #[case] depth: u8, #[case] ch: u16, #[case] kbps: u32) {
        assert_eq!(pcm_bitrate_kbps(rate, depth, ch), kbps);
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
