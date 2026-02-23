use std::io::Write;

use crate::{pcm, BitDepth, TranscodeParams, TranscoderError};

/// Write WAV data (RIFF/WAVE + fmt chunk + data chunk) to a writer.
pub fn write_wav(
    writer: &mut impl Write,
    params: &TranscodeParams,
    samples: &[f32],
) -> Result<(), TranscoderError> {
    let bytes_per_sample: u16 = match params.bit_depth {
        BitDepth::Sixteen => 2,
        BitDepth::TwentyFour => 3,
    };
    let bits_per_sample: u16 = match params.bit_depth {
        BitDepth::Sixteen => 16,
        BitDepth::TwentyFour => 24,
    };

    let num_samples = samples.len() as u32;
    let pcm_data_len = num_samples * bytes_per_sample as u32;

    let block_align: u16 = params.channels * bytes_per_sample;
    let byte_rate: u32 = params.sample_rate * block_align as u32;

    // fmt chunk body: 16 bytes for PCM
    let fmt_body_len: u32 = 16;
    // data chunk body: pcm_data_len bytes
    // RIFF body: 4 (WAVE) + 8 (fmt hdr) + fmt_body + 8 (data hdr) + pcm_data
    let riff_body_len: u32 = 4 + 8 + fmt_body_len + 8 + pcm_data_len;

    // ---- RIFF/WAVE header ----
    writer.write_all(b"RIFF")?;
    writer.write_all(&riff_body_len.to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    // ---- fmt chunk ----
    writer.write_all(b"fmt ")?;
    writer.write_all(&fmt_body_len.to_le_bytes())?;
    // audio format: 1 = PCM (little-endian u16)
    writer.write_all(&1u16.to_le_bytes())?;
    // channels
    writer.write_all(&params.channels.to_le_bytes())?;
    // sample rate
    writer.write_all(&params.sample_rate.to_le_bytes())?;
    // byte rate
    writer.write_all(&byte_rate.to_le_bytes())?;
    // block align
    writer.write_all(&block_align.to_le_bytes())?;
    // bits per sample
    writer.write_all(&bits_per_sample.to_le_bytes())?;

    // ---- data chunk ----
    writer.write_all(b"data")?;
    writer.write_all(&pcm_data_len.to_le_bytes())?;

    // PCM samples (little-endian)
    match params.bit_depth {
        BitDepth::Sixteen => {
            for &s in samples {
                writer.write_all(&pcm::f32_to_i16(s).to_le_bytes())?;
            }
        }
        BitDepth::TwentyFour => {
            for &s in samples {
                let bytes = pcm::f32_to_i24(s).to_le_bytes();
                writer.write_all(&bytes[..3])?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BitDepth, ExportFormat, TranscodeParams};

    fn stereo_params_16() -> TranscodeParams {
        TranscodeParams {
            format: ExportFormat::Wav,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        }
    }

    #[test]
    fn wav_starts_with_riff_wave_marker() {
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        write_wav(&mut buf, &params, &samples).unwrap();
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(&buf[8..12], b"WAVE");
    }

    #[test]
    fn wav_contains_fmt_chunk() {
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        write_wav(&mut buf, &params, &samples).unwrap();
        // fmt chunk starts at offset 12
        assert_eq!(&buf[12..16], b"fmt ");
    }

    #[test]
    fn wav_contains_data_chunk() {
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        write_wav(&mut buf, &params, &samples).unwrap();
        // data chunk starts at: RIFF hdr(8) + WAVE(4) + fmt hdr(8) + fmt body(16) = 36
        assert_eq!(&buf[36..40], b"data");
    }

    #[test]
    fn wav_24bit_size_is_larger_than_16bit() {
        let params16 = stereo_params_16();
        let params24 = TranscodeParams {
            bit_depth: BitDepth::TwentyFour,
            ..stereo_params_16()
        };
        let samples: Vec<f32> = vec![0.5; 200];
        let mut buf16 = Vec::new();
        let mut buf24 = Vec::new();
        write_wav(&mut buf16, &params16, &samples).unwrap();
        write_wav(&mut buf24, &params24, &samples).unwrap();
        assert!(buf24.len() > buf16.len());
    }
}
