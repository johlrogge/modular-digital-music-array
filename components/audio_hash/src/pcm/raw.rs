//! Integer-PCM reader shared by WAV and AIFF: the container parsers describe
//! where the samples are and how they are stored; this module hashes them.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use music_primitives::PcmHash;

use super::{Channels, PcmHashError, PcmHasher, SampleRate, StoredBits};
use crate::iff::Endian;
use crate::BUF_SIZE;

/// Signedness of 1-byte samples (WAV 8-bit is unsigned, AIFF 8-bit signed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EightBit {
    Signed,
    Unsigned,
}

/// Bytes per stored sample: 1 to 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerBytes {
    One,
    Two,
    Three,
    Four,
}

impl ContainerBytes {
    pub fn new(bytes: u16) -> Option<Self> {
        match bytes {
            1 => Some(ContainerBytes::One),
            2 => Some(ContainerBytes::Two),
            3 => Some(ContainerBytes::Three),
            4 => Some(ContainerBytes::Four),
            _ => None,
        }
    }

    fn get(self) -> usize {
        match self {
            ContainerBytes::One => 1,
            ContainerBytes::Two => 2,
            ContainerBytes::Three => 3,
            ContainerBytes::Four => 4,
        }
    }

    fn stored_bits(self) -> StoredBits {
        StoredBits(self.get() as u8 * 8)
    }
}

/// Where integer PCM samples are and how they are stored. Always describes a
/// whole number of frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawPcmLayout {
    order: Endian,
    eight_bit: EightBit,
    container: ContainerBytes,
    channels: Channels,
    rate: SampleRate,
    data: Range<u64>,
    frames: u64,
}

impl RawPcmLayout {
    /// `None` when `data` is not a whole number of frames.
    pub fn new(
        order: Endian,
        eight_bit: EightBit,
        container: ContainerBytes,
        channels: Channels,
        rate: SampleRate,
        data: Range<u64>,
    ) -> Option<Self> {
        let frame_bytes = (container.get() * channels.count()) as u64;
        let len = data.end.checked_sub(data.start)?;
        (len % frame_bytes == 0).then_some(RawPcmLayout {
            order,
            eight_bit,
            container,
            channels,
            rate,
            frames: len / frame_bytes,
            data,
        })
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }
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
    let cb = layout.container.get();
    let frame_bytes = cb * layout.channels.count();
    let mut hasher = PcmHasher::new(layout.channels, layout.rate, layout.container.stored_bits());
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
    Ok(hasher.finish()?.hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn layout_derives_frames_and_rejects_partial_frames() {
        let rate = SampleRate::new(44100).unwrap();
        let mk = |len: u64| {
            RawPcmLayout::new(
                Endian::Little,
                EightBit::Signed,
                ContainerBytes::Three,
                Channels::Stereo,
                rate,
                10..10 + len,
            )
        };
        assert_eq!(mk(18).unwrap().frames(), 3);
        assert_eq!(mk(0).unwrap().frames(), 0);
        assert!(mk(17).is_none());
        assert_eq!(ContainerBytes::new(0), None);
        assert_eq!(ContainerBytes::new(5), None);
    }

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
