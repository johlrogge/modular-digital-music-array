//! Integer-PCM reader shared by WAV and AIFF: the container parsers describe
//! where the samples are and how they are stored; this module hashes them.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use music_primitives::PcmHash;

use super::{Channels, PcmHashError, PcmHasher, SampleRate, StoredBits};
use crate::BUF_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endian {
    Little,
    Big,
}

/// Signedness of 1-byte samples (WAV 8-bit is unsigned, AIFF 8-bit signed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EightBit {
    Signed,
    Unsigned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawPcmLayout {
    pub order: Endian,
    pub eight_bit: EightBit,
    /// Bytes per stored sample, 1..=4. Stored bits are `container_bytes * 8`.
    pub container_bytes: u8,
    pub channels: Channels,
    pub rate: SampleRate,
    /// Byte range of the sample data; exactly `frames` whole frames.
    pub data: Range<u64>,
    pub frames: u64,
}

/// Sign-extended, right-justified value of one stored sample.
fn decode_sample(bytes: &[u8], order: Endian, eight_bit: EightBit) -> i32 {
    if let [b] = *bytes {
        return match eight_bit {
            EightBit::Unsigned => i32::from(b) - 128,
            EightBit::Signed => i32::from(b as i8),
        };
    }
    let fold = |acc: u32, &b: &u8| (acc << 8) | u32::from(b);
    let raw = match order {
        Endian::Big => bytes.iter().fold(0u32, fold),
        Endian::Little => bytes.iter().rev().fold(0u32, fold),
    };
    let shift = 32 - 8 * bytes.len() as u32;
    ((raw << shift) as i32) >> shift
}

pub(crate) fn hash<R: Read + Seek>(
    r: &mut R,
    layout: &RawPcmLayout,
) -> Result<PcmHash, PcmHashError> {
    if layout.frames == 0 {
        return Err(PcmHashError::no_audio());
    }
    let cb = usize::from(layout.container_bytes);
    let bits = u8::try_from(cb * 8)
        .ok()
        .and_then(StoredBits::new)
        .ok_or(PcmHashError::Decode("bad sample container size"))?;
    let frame_bytes = cb * layout.channels.count();
    let data_len = layout.data.end.saturating_sub(layout.data.start);
    if data_len != layout.frames.saturating_mul(frame_bytes as u64) {
        return Err(PcmHashError::Decode(
            "data length disagrees with frame count",
        ));
    }

    let mut hasher = PcmHasher::new(layout.channels, layout.rate, bits);
    r.seek(SeekFrom::Start(layout.data.start))?;
    let chunk_frames = (BUF_SIZE / frame_bytes).max(1) as u64;
    let mut raw = vec![0u8; chunk_frames as usize * frame_bytes];
    let mut samples: Vec<i32> = Vec::with_capacity(chunk_frames as usize * layout.channels.count());
    let mut remaining = layout.frames;
    while remaining > 0 {
        let n = remaining.min(chunk_frames);
        let len = n as usize * frame_bytes;
        r.read_exact(&mut raw[..len])?;
        samples.clear();
        samples.extend(
            raw[..len]
                .chunks_exact(cb)
                .map(|b| decode_sample(b, layout.order, layout.eight_bit)),
        );
        hasher.write_interleaved(&samples);
        remaining -= n;
    }

    let digest = hasher.finish()?;
    if digest.frames != layout.frames {
        return Err(PcmHashError::SampleCountMismatch {
            declared: layout.frames,
            decoded: digest.frames,
        });
    }
    Ok(digest.hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn decodes_every_width_and_order() {
        use EightBit::{Signed, Unsigned};
        use Endian::{Big, Little};
        assert_eq!(decode_sample(&[0x00], Little, Unsigned), -128);
        assert_eq!(decode_sample(&[0xFF], Little, Unsigned), 127);
        assert_eq!(decode_sample(&[0x80], Big, Signed), -128);
        assert_eq!(decode_sample(&[0x7F], Big, Signed), 127);
        assert_eq!(decode_sample(&[0xFE, 0xFF], Little, Signed), -2);
        assert_eq!(decode_sample(&[0xFF, 0xFE], Big, Signed), -2);
        assert_eq!(
            decode_sample(&[0x00, 0x00, 0x80], Little, Signed),
            -8_388_608
        );
        assert_eq!(decode_sample(&[0x7F, 0xFF, 0xFF], Big, Signed), 8_388_607);
        assert_eq!(decode_sample(&[0, 0, 0, 0x80], Little, Signed), i32::MIN);
        assert_eq!(
            decode_sample(&[0x7F, 0xFF, 0xFF, 0xFF], Big, Signed),
            i32::MAX
        );
    }
}
