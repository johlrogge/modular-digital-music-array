use std::io::Write;
use std::path::Path;

use extended::Extended;

use crate::{pcm, BitDepth, TranscodeParams, TranscoderError};

/// Reorder AIFF chunks so that the `ID3 ` chunk appears before `SSND`.
///
/// DJ software (Rekordbox, CDJs, Serato) expects the ID3 chunk to precede
/// the SSND audio chunk.  Lofty appends ID3 after SSND, so we fix the order
/// here as a post-processing step.
///
/// If no ID3 chunk is found the file is left untouched (no-op).
pub(crate) fn reorder_aiff_chunks(path: &Path) -> Result<(), TranscoderError> {
    let data = std::fs::read(path)?;

    // Minimum AIFF: FORM(4) + size(4) + AIFF(4) = 12 bytes
    if data.len() < 12 {
        return Ok(());
    }

    // Validate FORM/AIFF header
    if &data[0..4] != b"FORM" || &data[8..12] != b"AIFF" {
        return Ok(());
    }

    // Parse chunks starting after the 12-byte FORM header
    let mut pos = 12usize;
    let mut id3_chunk: Option<Vec<u8>> = None;
    let mut ssnd_chunk: Option<Vec<u8>> = None;
    let mut other_chunks: Vec<Vec<u8>> = Vec::new();

    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        // IFF spec: each chunk is padded to an even boundary
        let padded_size = size + (size & 1);
        let end = pos + 8 + padded_size;
        if end > data.len() {
            // Truncated chunk — stop parsing
            break;
        }
        let chunk_bytes = data[pos..end].to_vec();

        if id == b"ID3 " {
            id3_chunk = Some(chunk_bytes);
        } else if id == b"SSND" {
            ssnd_chunk = Some(chunk_bytes);
        } else {
            other_chunks.push(chunk_bytes);
        }

        pos = end;
    }

    // No ID3 chunk found — nothing to reorder
    let id3_chunk = match id3_chunk {
        Some(c) => c,
        None => return Ok(()),
    };

    // Rebuild file: FORM header + other + ID3 + SSND
    let mut out: Vec<u8> = Vec::with_capacity(data.len());

    // Placeholder for FORM header (we'll fill in the size after)
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&[0u8; 4]); // size placeholder
    out.extend_from_slice(b"AIFF");

    for chunk in &other_chunks {
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&id3_chunk);
    if let Some(ssnd) = &ssnd_chunk {
        out.extend_from_slice(ssnd);
    }

    // Fix up the FORM size field: total bytes after the 8-byte FORM+size header
    let form_body_len = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&form_body_len.to_be_bytes());

    std::fs::write(path, &out)?;
    Ok(())
}

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

    /// Build a minimal but structurally valid AIFF with chunks in the order:
    /// COMM → SSND → ID3  (wrong order — ID3 should come before SSND).
    fn minimal_aiff_with_id3_after_ssnd() -> Vec<u8> {
        // Tiny COMM chunk: 18-byte body (channels=1, frames=1, bitdepth=16, sr=44100)
        let comm_body: &[u8] = &[
            0x00, 0x01, // channels: 1
            0x00, 0x00, 0x00, 0x01, // numSampleFrames: 1
            0x00, 0x10, // sampleSize: 16
            0x40, 0x0E, 0xAC, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 80-bit 44100
        ];
        // SSND chunk: offset(4) + block(4) + 2 bytes PCM = 10 bytes body
        let ssnd_body: &[u8] = &[
            0x00, 0x00, 0x00, 0x00, // offset
            0x00, 0x00, 0x00, 0x00, // block_size
            0x00, 0x00, // one silent 16-bit sample
        ];
        // Fake ID3 chunk body (minimal — just some distinguishable bytes)
        let id3_body: &[u8] = b"ID3FAKEDATA";

        let mut buf: Vec<u8> = Vec::new();

        // FORM + size placeholder + AIFF
        buf.extend_from_slice(b"FORM");
        buf.extend_from_slice(&[0u8; 4]); // size — fill later
        buf.extend_from_slice(b"AIFF");

        // COMM chunk
        buf.extend_from_slice(b"COMM");
        buf.extend_from_slice(&(comm_body.len() as u32).to_be_bytes());
        buf.extend_from_slice(comm_body);
        // comm_body.len() == 18 which is even, no padding needed

        // SSND chunk
        buf.extend_from_slice(b"SSND");
        buf.extend_from_slice(&(ssnd_body.len() as u32).to_be_bytes());
        buf.extend_from_slice(ssnd_body);
        // ssnd_body.len() == 10, even — no padding

        // ID3 chunk (WRONG position — after SSND)
        buf.extend_from_slice(b"ID3 ");
        buf.extend_from_slice(&(id3_body.len() as u32).to_be_bytes());
        buf.extend_from_slice(id3_body);
        // id3_body.len() == 11 (odd) — pad byte
        buf.push(0x00);

        // Fix up FORM size
        let form_body_len = (buf.len() - 8) as u32;
        buf[4..8].copy_from_slice(&form_body_len.to_be_bytes());

        buf
    }

    fn find_chunk_offset(data: &[u8], id: &[u8; 4]) -> Option<usize> {
        let mut pos = 12usize; // skip FORM header
        while pos + 8 <= data.len() {
            let chunk_id = &data[pos..pos + 4];
            let size = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let padded = size + (size & 1);
            if chunk_id == id {
                return Some(pos);
            }
            pos += 8 + padded;
        }
        None
    }

    #[test]
    fn reorder_puts_id3_before_ssnd() {
        let aiff_bytes = minimal_aiff_with_id3_after_ssnd();

        // Verify precondition: in the original, SSND comes before ID3
        let ssnd_pos_before = find_chunk_offset(&aiff_bytes, b"SSND").expect("SSND missing");
        let id3_pos_before = find_chunk_offset(&aiff_bytes, b"ID3 ").expect("ID3  missing");
        assert!(
            ssnd_pos_before < id3_pos_before,
            "precondition: SSND should be before ID3 in source"
        );

        // Write to a temp file, call reorder, read back
        let tmp = tempfile::Builder::new().suffix(".aiff").tempfile().unwrap();
        std::fs::write(tmp.path(), &aiff_bytes).unwrap();
        reorder_aiff_chunks(tmp.path()).unwrap();

        let result = std::fs::read(tmp.path()).unwrap();

        // After reorder: ID3 must come before SSND
        let id3_pos_after =
            find_chunk_offset(&result, b"ID3 ").expect("ID3  missing after reorder");
        let ssnd_pos_after =
            find_chunk_offset(&result, b"SSND").expect("SSND missing after reorder");
        assert!(
            id3_pos_after < ssnd_pos_after,
            "after reorder, ID3 ({id3_pos_after}) should be before SSND ({ssnd_pos_after})"
        );
    }

    #[test]
    fn reorder_is_noop_when_no_id3_chunk() {
        // A plain AIFF without ID3 should be unchanged (same bytes)
        let params = stereo_params_16();
        let samples: Vec<f32> = vec![0.0; 10];
        let mut buf = Vec::new();
        write_aiff(&mut buf, &params, &samples).unwrap();

        let tmp = tempfile::Builder::new().suffix(".aiff").tempfile().unwrap();
        std::fs::write(tmp.path(), &buf).unwrap();
        reorder_aiff_chunks(tmp.path()).unwrap();
        let result = std::fs::read(tmp.path()).unwrap();

        assert_eq!(buf, result, "file without ID3 should be unchanged");
    }

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
