//! What an export of a stored blob needs to know about it: the one description both
//! the console's export and the tests that prove its results use.

use std::path::Path;

use crate::SourceAudio;

/// The canonical file extension of a format: `aif` is `aiff`.
pub fn canonical_ext(ext: &str) -> &str {
    if ext == "aif" {
        "aiff"
    } else {
        ext
    }
}

/// What the export of a blob to a converted format needs to know about it. Built
/// only for converted formats: `original` reads nothing from the file.
#[derive(Debug, Default)]
pub struct ExportSource {
    /// Extension (lowercase), rate and depth as the transcoder sees them.
    pub audio: SourceAudio,
    /// The samples are plain PCM (a plain AIFF, a WAV), not AIFF-C or a codec.
    pub plain_pcm: bool,
}

/// Describe the blob at `path` (blocking file reads). Unreadable properties are
/// unknown, which the transcoder and the export decision treat conservatively.
///
/// An AIFF blob is read once with `audio_hash::stream_info` (rate, depth and
/// encoding from COMM, validated; its tags may be unreadable); everything else
/// from its tags.
pub fn describe_source(path: &Path, ext: &str) -> ExportSource {
    if canonical_ext(ext) == "aiff" {
        if let Ok(info) = audio_hash::stream_info(path) {
            return ExportSource {
                audio: SourceAudio {
                    ext: ext.to_string(),
                    sample_rate: Some(info.sample_rate),
                    bit_depth: Some(info.bits_per_sample),
                    duration: Some(info.duration()),
                },
                plain_pcm: matches!(info.encoding, audio_hash::SampleEncoding::Pcm { .. }),
            };
        }
    }
    let meta = audio_metadata::extract_metadata(path).ok();
    ExportSource {
        audio: SourceAudio {
            ext: ext.to_string(),
            sample_rate: meta.as_ref().and_then(|m| m.sample_rate),
            bit_depth: meta.as_ref().and_then(|m| m.bit_depth),
            duration: meta.as_ref().and_then(|m| m.duration),
        },
        // A WAV is PCM unless it is float, which lofty reports as 32 bits and the
        // export-ready rule then refuses. An AIFF whose COMM could not be read is
        // not known to be plain.
        plain_pcm: canonical_ext(ext) == "wav",
    }
}
