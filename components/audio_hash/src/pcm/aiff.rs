//! AIFF / AIFF-C: read COMM and SSND (located by the crate-level
//! [`crate::aiff`] walk) into a [`RawPcmLayout`].

use std::io::{Read, Seek};
use std::ops::Range;

use super::raw::{ContainerBytes, EightBit, RawPcmLayout};
use super::{ByteOrder, Channels, PcmHashError, PcmInfo, SampleEncoding, SampleRate, Unsupported};
use crate::iff::Endian;
use crate::tags::{be_u16, be_u32, read_at};
use crate::{aiff, Container};

const C: Container = Container::Aiff;

/// Exact positive integer value of an 80-bit IEEE extended number that fits
/// `u32`; anything else (fractions, negatives, zero, inf/NaN) is `None`.
fn extended_to_u32(b: &[u8; 10]) -> Option<u32> {
    let sign_exp = be_u16(b, 0);
    if sign_exp & 0x8000 != 0 {
        return None;
    }
    let mut m = [0u8; 8];
    m.copy_from_slice(&b[2..]);
    let mant = u64::from_be_bytes(m);
    if mant == 0 {
        return None;
    }
    // value = mant * 2^e
    let e = i32::from(sign_exp) - 16383 - 63;
    let value = if e >= 0 {
        if e >= 64 || mant.leading_zeros() < e as u32 {
            return None;
        }
        mant << e
    } else {
        let s = e.unsigned_abs();
        if s >= 64 || mant & ((1u64 << s) - 1) != 0 {
            return None;
        }
        mant >> s
    };
    u32::try_from(value).ok().filter(|v| *v != 0)
}

struct Comm {
    order: Endian,
    channels: Channels,
    frames: u64,
    container: ContainerBytes,
    rate: SampleRate,
    /// Bits per sample as declared (`sampleSize`), 1..=32.
    sample_size: u16,
}

fn parse_comm<R: Read + Seek>(
    r: &mut R,
    payload: &Range<u64>,
    is_aifc: bool,
) -> Result<Comm, PcmHashError> {
    let need = if is_aifc { 22 } else { 18 };
    if payload.end - payload.start < need {
        return Err(PcmHashError::malformed(C, "COMM chunk too short"));
    }
    let b: [u8; 22] = if is_aifc {
        read_at(r, payload.start)?
    } else {
        let short: [u8; 18] = read_at(r, payload.start)?;
        let mut b = [0u8; 22];
        b[..18].copy_from_slice(&short);
        b
    };

    let order = if is_aifc {
        match &b[18..22] {
            b"NONE" | b"twos" => Endian::Big,
            b"sowt" => Endian::Little,
            _ => return Err(PcmHashError::Unsupported(Unsupported::CompressedAiffC)),
        }
    } else {
        Endian::Big
    };

    let channels = Channels::from_field(u32::from(be_u16(&b, 0)), C)?;
    let sample_size = be_u16(&b, 6);
    if sample_size == 0 {
        return Err(PcmHashError::malformed(C, "zero sample size"));
    }
    // ceil(sampleSize / 8) bytes; more than 4 means sampleSize over 32 bits.
    let container = ContainerBytes::new(sample_size.div_ceil(8))
        .ok_or(PcmHashError::Unsupported(Unsupported::SampleSizeOver32))?;
    let mut ext = [0u8; 10];
    ext.copy_from_slice(&b[8..18]);
    let rate = extended_to_u32(&ext)
        .and_then(SampleRate::new)
        .ok_or(PcmHashError::Unsupported(Unsupported::NonIntegerRate))?;
    Ok(Comm {
        order,
        channels,
        frames: u64::from(be_u32(&b, 2)),
        container,
        rate,
        sample_size,
    })
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<RawPcmLayout, PcmHashError> {
    parse(r, file_len).map(|(layout, _)| layout)
}

/// The stream properties COMM declares, after the same validation as the
/// hash (SSND length against the frame count included).
pub(crate) fn info<R: Read + Seek>(r: &mut R, file_len: u64) -> Result<PcmInfo, PcmHashError> {
    let (layout, sample_size) = parse(r, file_len)?;
    let l = aiff::layout(r, file_len)?;
    let encoding = encoding(r, &l)?;
    Ok(PcmInfo {
        container: C,
        channels: layout.channels().count() as u8,
        sample_rate: layout.rate().get(),
        frames: layout.frames(),
        // At most 32: the container is at most 4 bytes.
        bits_per_sample: sample_size as u8,
        encoding,
    })
}

/// What `COMM` declares, under any compression type (float, `ulaw`, ... included):
/// no support check and no look at the sample data. `sampleSize` is the width of
/// the decoded samples as the file states it.
pub(crate) fn stream_info<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<PcmInfo, PcmHashError> {
    let l = aiff::layout(r, file_len)?;
    let encoding = encoding(r, &l)?;
    if l.comm.end - l.comm.start < 18 {
        return Err(PcmHashError::malformed(C, "COMM chunk too short"));
    }
    let b: [u8; 18] = read_at(r, l.comm.start)?;
    let channels = u8::try_from(be_u16(&b, 0))
        .ok()
        .filter(|c| *c != 0)
        .ok_or_else(|| PcmHashError::malformed(C, "COMM channel count out of range"))?;
    let bits_per_sample = u8::try_from(be_u16(&b, 6))
        .ok()
        .filter(|s| *s != 0)
        .ok_or_else(|| PcmHashError::malformed(C, "COMM sample size out of range"))?;
    let mut ext = [0u8; 10];
    ext.copy_from_slice(&b[8..18]);
    let sample_rate =
        extended_to_u32(&ext).ok_or(PcmHashError::Unsupported(Unsupported::NonIntegerRate))?;
    Ok(PcmInfo {
        container: C,
        channels,
        sample_rate,
        frames: u64::from(be_u32(&b, 2)),
        bits_per_sample,
        encoding,
    })
}

/// A plain AIFF is big-endian PCM; an AIFF-C is named by its compression type.
fn encoding<R: Read + Seek>(
    r: &mut R,
    l: &aiff::AiffLayout,
) -> Result<SampleEncoding, PcmHashError> {
    if !l.is_aifc {
        return Ok(SampleEncoding::Pcm {
            order: ByteOrder::Big,
        });
    }
    if l.comm.end - l.comm.start < 22 {
        return Err(PcmHashError::malformed(C, "COMM chunk too short"));
    }
    let b: [u8; 22] = read_at(r, l.comm.start)?;
    Ok(SampleEncoding::Other([b[18], b[19], b[20], b[21]]))
}

/// The layout and COMM's `sampleSize` (bits per sample, as declared).
fn parse<R: Read + Seek>(r: &mut R, file_len: u64) -> Result<(RawPcmLayout, u16), PcmHashError> {
    let l = aiff::layout(r, file_len)?;
    let c = parse_comm(r, &l.comm, l.is_aifc)?;

    if l.ssnd.end - l.ssnd.start < 8 {
        return Err(PcmHashError::malformed(C, "SSND chunk too short"));
    }
    let ssnd_head: [u8; 8] = read_at(r, l.ssnd.start)?;
    let offset = u64::from(be_u32(&ssnd_head, 0));
    // blockSize (bytes 4..8) is a hint for block-aligned readers; unused.
    let samples_start = l.ssnd.start + 8 + offset;
    if samples_start > l.ssnd.end {
        return Err(PcmHashError::malformed(C, "SSND offset past end of chunk"));
    }
    // The sample bytes after `offset` must be exactly numSampleFrames frames.
    let layout = RawPcmLayout::new(
        c.order,
        EightBit::Signed,
        c.container,
        c.channels,
        c.rate,
        samples_start..l.ssnd.end,
    )
    .filter(|layout| layout.frames() == c.frames)
    .ok_or_else(|| PcmHashError::malformed(C, "SSND length does not match COMM frame count"))?;
    Ok((layout, c.sample_size))
}
