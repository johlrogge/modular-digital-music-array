mod aiff;
mod flac;
mod metadata;
pub(crate) mod pcm;
mod wav;

use std::io::Write;
use std::path::Path;

use thiserror::Error;

pub use aiff::write_aiff;
pub use flac::write_flac;
pub use wav::write_wav;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    /// Uncompressed PCM, big-endian — CDJ/Rekordbox standard
    Aiff,
    /// Uncompressed PCM, little-endian — universal compatibility
    Wav,
    /// Lossless compressed — archival / smaller exports
    Flac,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitDepth {
    Sixteen,
    TwentyFour,
}

#[derive(Debug, Clone)]
pub struct TranscodeParams {
    pub format: ExportFormat,
    pub channels: u16,
    pub sample_rate: u32,
    pub bit_depth: BitDepth,
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

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum TranscoderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Lofty error: {0}")]
    Lofty(#[from] lofty::LoftyError),

    #[error("FLAC encoding not yet supported — use blob copy for FLAC sources")]
    FlacEncodingNotSupported,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Write encoded audio to `writer`.
///
/// `samples` must be interleaved, normalised to `[-1.0, 1.0]`.
pub fn transcode(
    writer: &mut impl Write,
    params: &TranscodeParams,
    samples: &[f32],
) -> Result<(), TranscoderError> {
    match params.format {
        ExportFormat::Aiff => write_aiff(writer, params, samples),
        ExportFormat::Wav => write_wav(writer, params, samples),
        ExportFormat::Flac => write_flac(writer, params, samples),
    }
}

/// Write encoded audio to `path` and then inject `metadata` tags.
///
/// The file is first written with `transcode`, then lofty opens it to
/// attach the tag fields.  FLAC returns an error immediately (use blob copy).
pub fn transcode_with_metadata(
    path: &Path,
    params: &TranscodeParams,
    samples: &[f32],
    meta: &TrackMetadata,
) -> Result<(), TranscoderError> {
    // Write the raw audio data, buffered to avoid per-sample syscall overhead
    let raw_file = std::fs::File::create(path)?;
    let mut file = std::io::BufWriter::new(raw_file);
    transcode(&mut file, params, samples)?;
    // Flush before dropping so any BufWriter error is visible
    file.flush()?;
    drop(file);

    // Inject metadata tags
    metadata::inject_metadata(path, &params.format, meta)?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // ── helper ────────────────────────────────────────────────────────────────

    /// Generate a sine-wave test signal at 440 Hz.
    fn sine_samples(sample_rate: u32, channels: u16, frames: usize) -> Vec<f32> {
        let freq = 440.0_f32;
        let sr = sample_rate as f32;
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin() * 0.5;
                std::iter::repeat(v).take(channels as usize)
            })
            .collect()
    }

    // ── byte-level marker tests ───────────────────────────────────────────────

    #[test]
    fn aiff_output_starts_with_form_and_contains_aiff() {
        let params = TranscodeParams {
            format: ExportFormat::Aiff,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        };
        let samples = sine_samples(44100, 2, 512);
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &samples).unwrap();
        assert_eq!(&buf[0..4], b"FORM", "AIFF must start with FORM");
        assert_eq!(&buf[8..12], b"AIFF", "AIFF must contain AIFF marker");
    }

    #[test]
    fn wav_output_starts_with_riff_and_contains_wave() {
        let params = TranscodeParams {
            format: ExportFormat::Wav,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        };
        let samples = sine_samples(44100, 2, 512);
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &samples).unwrap();
        assert_eq!(&buf[0..4], b"RIFF", "WAV must start with RIFF");
        assert_eq!(&buf[8..12], b"WAVE", "WAV must contain WAVE marker");
    }

    #[test]
    fn flac_returns_not_supported_error() {
        let params = TranscodeParams {
            format: ExportFormat::Flac,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        };
        let samples = sine_samples(44100, 2, 512);
        let mut buf = Vec::new();
        let result = transcode(&mut buf, &params, &samples);
        assert!(
            matches!(result, Err(TranscoderError::FlacEncodingNotSupported)),
            "FLAC transcode must return FlacEncodingNotSupported"
        );
    }

    // ── round-trip via standard library + direct byte inspection ─────────────
    // These tests decode the PCM samples by parsing the raw container bytes
    // ourselves, verifying quantisation fidelity without an external decoder.

    fn decode_wav_samples_16bit(buf: &[u8]) -> Vec<f32> {
        // find "data" chunk
        let data_pos = buf
            .windows(4)
            .position(|w| w == b"data")
            .expect("no data chunk");
        let data_size = u32::from_le_bytes(buf[data_pos + 4..data_pos + 8].try_into().unwrap());
        let pcm = &buf[data_pos + 8..data_pos + 8 + data_size as usize];
        pcm.chunks_exact(2)
            .map(|b| i16::from_le_bytes(b.try_into().unwrap()) as f32 / 32767.0)
            .collect()
    }

    fn decode_aiff_samples_16bit(buf: &[u8]) -> Vec<f32> {
        // find "SSND" chunk
        let ssnd_pos = buf
            .windows(4)
            .position(|w| w == b"SSND")
            .expect("no SSND chunk");
        let chunk_size =
            u32::from_be_bytes(buf[ssnd_pos + 4..ssnd_pos + 8].try_into().unwrap()) as usize;
        // skip offset(4) + block_size(4) = 8 bytes header inside SSND
        let pcm_start = ssnd_pos + 8 + 8;
        let pcm_end = ssnd_pos + 8 + chunk_size;
        let pcm = &buf[pcm_start..pcm_end];
        pcm.chunks_exact(2)
            .map(|b| i16::from_be_bytes(b.try_into().unwrap()) as f32 / 32767.0)
            .collect()
    }

    #[test]
    fn wav_16bit_self_roundtrip() {
        let sample_rate = 44100u32;
        let channels = 2u16;
        let original = sine_samples(sample_rate, channels, 1024);
        let params = TranscodeParams {
            format: ExportFormat::Wav,
            channels,
            sample_rate,
            bit_depth: BitDepth::Sixteen,
        };
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &original).unwrap();
        let decoded = decode_wav_samples_16bit(&buf);
        assert_eq!(decoded.len(), original.len());
        let tolerance = 1.0_f32 / 32767.0 * 2.0; // ±1 LSB
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!(
                (a - b).abs() <= tolerance,
                "WAV sample drift too large: orig={a:.6}, decoded={b:.6}"
            );
        }
    }

    #[test]
    fn aiff_16bit_self_roundtrip() {
        let sample_rate = 44100u32;
        let channels = 2u16;
        let original = sine_samples(sample_rate, channels, 1024);
        let params = TranscodeParams {
            format: ExportFormat::Aiff,
            channels,
            sample_rate,
            bit_depth: BitDepth::Sixteen,
        };
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &original).unwrap();
        let decoded = decode_aiff_samples_16bit(&buf);
        assert_eq!(decoded.len(), original.len());
        let tolerance = 1.0_f32 / 32767.0 * 2.0;
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!(
                (a - b).abs() <= tolerance,
                "AIFF sample drift too large: orig={a:.6}, decoded={b:.6}"
            );
        }
    }

    // ── edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn mono_audio_wav() {
        let params = TranscodeParams {
            format: ExportFormat::Wav,
            channels: 1,
            sample_rate: 48000,
            bit_depth: BitDepth::Sixteen,
        };
        let samples = sine_samples(48000, 1, 512);
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &samples).unwrap();
        assert_eq!(&buf[0..4], b"RIFF");
        // channels field is at offset 22 (little-endian u16)
        let ch = u16::from_le_bytes(buf[22..24].try_into().unwrap());
        assert_eq!(ch, 1);
    }

    #[test]
    fn mono_audio_aiff() {
        let params = TranscodeParams {
            format: ExportFormat::Aiff,
            channels: 1,
            sample_rate: 48000,
            bit_depth: BitDepth::Sixteen,
        };
        let samples = sine_samples(48000, 1, 512);
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &samples).unwrap();
        assert_eq!(&buf[0..4], b"FORM");
        // channels in COMM chunk: offset 12 (COMM header) + 8 (COMM chunk header) = 20, i16 BE
        let ch = i16::from_be_bytes(buf[20..22].try_into().unwrap());
        assert_eq!(ch, 1);
    }

    #[test]
    fn various_sample_rates_wav() {
        for &sr in &[44100u32, 48000, 96000] {
            let params = TranscodeParams {
                format: ExportFormat::Wav,
                channels: 2,
                sample_rate: sr,
                bit_depth: BitDepth::Sixteen,
            };
            let samples = sine_samples(sr, 2, 256);
            let mut buf = Vec::new();
            transcode(&mut buf, &params, &samples).unwrap();
            // sample rate at bytes 24..28 in WAV (little-endian u32)
            let decoded_sr = u32::from_le_bytes(buf[24..28].try_into().unwrap());
            assert_eq!(decoded_sr, sr, "sample rate mismatch for {sr}");
        }
    }

    #[test]
    fn various_sample_rates_aiff() {
        for &sr in &[44100u32, 48000, 96000] {
            let params = TranscodeParams {
                format: ExportFormat::Aiff,
                channels: 2,
                sample_rate: sr,
                bit_depth: BitDepth::Sixteen,
            };
            let samples = sine_samples(sr, 2, 256);
            let mut buf = Vec::new();
            transcode(&mut buf, &params, &samples).unwrap();
            assert_eq!(&buf[0..4], b"FORM");
        }
    }

    #[test]
    fn wav_24bit_produces_correct_pcm_size() {
        let frames = 100usize;
        let channels = 2u16;
        let params = TranscodeParams {
            format: ExportFormat::Wav,
            channels,
            sample_rate: 44100,
            bit_depth: BitDepth::TwentyFour,
        };
        let samples = vec![0.5f32; frames * channels as usize];
        let mut buf = Vec::new();
        transcode(&mut buf, &params, &samples).unwrap();
        // data size field at bytes 40..44
        let data_size = u32::from_le_bytes(buf[40..44].try_into().unwrap());
        assert_eq!(data_size as usize, frames * channels as usize * 3);
    }

    #[test]
    fn transcode_with_metadata_creates_file() {
        let tmp = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        let params = TranscodeParams {
            format: ExportFormat::Wav,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        };
        let samples = sine_samples(44100, 2, 512);
        let meta = TrackMetadata {
            title: Some("Test".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            bpm: Some(128.0),
            key: Some("Cm".to_string()),
        };
        transcode_with_metadata(tmp.path(), &params, &samples, &meta).unwrap();
        let metadata = std::fs::metadata(tmp.path()).unwrap();
        assert!(metadata.len() > 0, "output file must not be empty");
    }
}
