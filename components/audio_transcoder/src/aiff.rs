use std::io::Write;

use extended::Extended;

use crate::{pcm, BitDepth, TranscodeParams, TranscoderError};

/// Write AIFF data (FORM/AIFF + COMM + SSND) to a writer.
pub fn write_aiff(
    writer: &mut impl Write,
    params: &TranscodeParams,
    samples: &[f32],
) -> Result<(), TranscoderError> {
    let bytes_per_sample: u32 = match params.bit_depth {
        BitDepth::Sixteen => 2,
        BitDepth::TwentyFour => 3,
    };
    let num_frames = samples.len() as u32 / params.channels as u32;
    let pcm_data_len = num_frames * params.channels as u32 * bytes_per_sample;

    // COMM chunk body: 2 + 4 + 2 + 10 = 18 bytes
    let comm_body_len: u32 = 18;
    // SSND chunk body: 4 (offset) + 4 (block_size) + pcm data
    let ssnd_body_len: u32 = 8 + pcm_data_len;

    // FORM body: 4 (AIFF marker) + 8 (COMM hdr) + comm_body + 8 (SSND hdr) + ssnd_body
    let form_body_len: u32 = 4 + 8 + comm_body_len + 8 + ssnd_body_len;

    // ---- FORM/AIFF header ----
    writer.write_all(b"FORM")?;
    writer.write_all(&form_body_len.to_be_bytes())?;
    writer.write_all(b"AIFF")?;

    // ---- COMM chunk ----
    writer.write_all(b"COMM")?;
    writer.write_all(&comm_body_len.to_be_bytes())?;
    // channels (i16 big-endian)
    writer.write_all(&(params.channels as i16).to_be_bytes())?;
    // num_frames (u32 big-endian)
    writer.write_all(&num_frames.to_be_bytes())?;
    // bit depth (i16 big-endian)
    let bit_depth_val: i16 = match params.bit_depth {
        BitDepth::Sixteen => 16,
        BitDepth::TwentyFour => 24,
    };
    writer.write_all(&bit_depth_val.to_be_bytes())?;
    // sample rate as 80-bit extended (big-endian)
    let sr_extended = Extended::from(params.sample_rate as f64);
    writer.write_all(&sr_extended.to_be_bytes())?;

    // ---- SSND chunk ----
    writer.write_all(b"SSND")?;
    writer.write_all(&ssnd_body_len.to_be_bytes())?;
    // offset (u32 = 0) and block_size (u32 = 0)
    writer.write_all(&0u32.to_be_bytes())?;
    writer.write_all(&0u32.to_be_bytes())?;

    // PCM samples (big-endian)
    match params.bit_depth {
        BitDepth::Sixteen => {
            for &s in samples {
                writer.write_all(&pcm::f32_to_i16(s).to_be_bytes())?;
            }
        }
        BitDepth::TwentyFour => {
            for &s in samples {
                let bytes = pcm::f32_to_i24(s).to_be_bytes();
                writer.write_all(&bytes[1..])?;
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
            format: ExportFormat::Aiff,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        }
    }

    #[test]
    fn aiff_starts_with_form_aiff_marker() {
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        write_aiff(&mut buf, &params, &samples).unwrap();
        assert_eq!(&buf[0..4], b"FORM");
        // bytes 8..12 should be "AIFF"
        assert_eq!(&buf[8..12], b"AIFF");
    }

    #[test]
    fn aiff_contains_comm_chunk() {
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        write_aiff(&mut buf, &params, &samples).unwrap();
        // COMM chunk starts at offset 12
        assert_eq!(&buf[12..16], b"COMM");
    }

    #[test]
    fn aiff_contains_ssnd_chunk() {
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        write_aiff(&mut buf, &params, &samples).unwrap();
        // SSND starts after: FORM(8) + AIFF(4) + COMM hdr(8) + COMM body(18) = 38
        assert_eq!(&buf[38..42], b"SSND");
    }

    #[test]
    fn aiff_24bit_size_is_larger_than_16bit() {
        let params16 = stereo_params_16();
        let params24 = TranscodeParams {
            bit_depth: BitDepth::TwentyFour,
            ..stereo_params_16()
        };
        let samples: Vec<f32> = vec![0.5; 200];
        let mut buf16 = Vec::new();
        let mut buf24 = Vec::new();
        write_aiff(&mut buf16, &params16, &samples).unwrap();
        write_aiff(&mut buf24, &params24, &samples).unwrap();
        assert!(buf24.len() > buf16.len());
    }
}
