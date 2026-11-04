//! AIFF / AIFF-C: read COMM and SSND (located by the crate-level
//! [`crate::aiff`] walk) into a [`RawPcmLayout`].

use std::io::{Read, Seek};
use std::ops::Range;

use super::raw::{ContainerBytes, EightBit, RawPcmLayout};
use super::{Channels, PcmHashError, SampleRate};
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
            _ => return Err(PcmHashError::Unsupported("aiff-c: compressed audio")),
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
        .ok_or(PcmHashError::Unsupported("aiff: sample size over 32 bits"))?;
    let mut ext = [0u8; 10];
    ext.copy_from_slice(&b[8..18]);
    let rate = extended_to_u32(&ext)
        .and_then(SampleRate::new)
        .ok_or(PcmHashError::Unsupported(
            "aiff: sample rate is not an integer",
        ))?;
    Ok(Comm {
        order,
        channels,
        frames: u64::from(be_u32(&b, 2)),
        container,
        rate,
    })
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<RawPcmLayout, PcmHashError> {
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
    RawPcmLayout::new(
        c.order,
        EightBit::Signed,
        c.container,
        c.channels,
        c.rate,
        samples_start..l.ssnd.end,
    )
    .filter(|layout| layout.frames() == c.frames)
    .ok_or_else(|| PcmHashError::malformed(C, "SSND length does not match COMM frame count"))
}
