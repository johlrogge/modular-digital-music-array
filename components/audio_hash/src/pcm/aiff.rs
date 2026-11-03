//! AIFF / AIFF-C: walk the big-endian chunks, read COMM and SSND into a
//! [`RawPcmLayout`].

use std::io::{Read, Seek};
use std::ops::Range;

use super::raw::{EightBit, Endian, RawPcmLayout};
use super::{Channels, PcmHashError, SampleRate};
use crate::tags::{be_u32, read_at};
use crate::Container;

const C: Container = Container::Aiff;

/// Exact positive integer value of an 80-bit IEEE extended number that fits
/// `u32`; anything else (fractions, negatives, zero, inf/NaN) is `None`.
fn extended_to_u32(b: &[u8; 10]) -> Option<u32> {
    let sign_exp = u16::from_be_bytes([b[0], b[1]]);
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
    container_bytes: u8,
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

    let channel_count = u16::from_be_bytes([b[0], b[1]]);
    if channel_count == 0 {
        return Err(PcmHashError::malformed(C, "zero channels"));
    }
    let channels = Channels::try_from(u32::from(channel_count))?;
    let sample_size = u16::from_be_bytes([b[6], b[7]]);
    if sample_size == 0 {
        return Err(PcmHashError::malformed(C, "zero sample size"));
    }
    if sample_size > 32 {
        return Err(PcmHashError::Unsupported("aiff: sample size over 32 bits"));
    }
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
        // ceil(sampleSize / 8); at most 4 after the range check above.
        container_bytes: sample_size.div_ceil(8) as u8,
        rate,
    })
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<RawPcmLayout, PcmHashError> {
    if file_len < 12 {
        return Err(PcmHashError::malformed(C, "shorter than FORM header"));
    }
    let head: [u8; 12] = read_at(r, 0)?;
    if &head[0..4] != b"FORM" || !matches!(&head[8..12], b"AIFF" | b"AIFC") {
        return Err(PcmHashError::malformed(C, "not a FORM/AIFF file"));
    }
    let is_aifc = &head[8..12] == b"AIFC";

    let mut comm: Option<Range<u64>> = None;
    let mut ssnd: Option<Range<u64>> = None;
    let mut pos = 12u64;
    while (comm.is_none() || ssnd.is_none()) && file_len.saturating_sub(pos) >= 8 {
        let h: [u8; 8] = read_at(r, pos)?;
        let size = u64::from(be_u32(&h, 4));
        let payload = pos + 8;
        let end = payload + size;
        if end > file_len {
            return Err(PcmHashError::malformed(C, "chunk extends past end of file"));
        }
        match &h[0..4] {
            b"COMM" if comm.is_none() => comm = Some(payload..end),
            b"SSND" if ssnd.is_none() => ssnd = Some(payload..end),
            _ => {}
        }
        pos = end + size % 2;
    }
    let comm = comm.ok_or_else(|| PcmHashError::malformed(C, "missing COMM chunk"))?;
    let ssnd = ssnd.ok_or_else(|| PcmHashError::malformed(C, "missing SSND chunk"))?;
    let c = parse_comm(r, &comm, is_aifc)?;

    if ssnd.end - ssnd.start < 8 {
        return Err(PcmHashError::malformed(C, "SSND chunk too short"));
    }
    let ssnd_head: [u8; 8] = read_at(r, ssnd.start)?;
    let offset = u64::from(be_u32(&ssnd_head, 0));
    // blockSize (bytes 4..8) is a hint for block-aligned readers; unused.
    let samples_start = ssnd.start + 8 + offset;
    if samples_start > ssnd.end {
        return Err(PcmHashError::malformed(C, "SSND offset past end of chunk"));
    }
    let frame_bytes = u64::from(c.container_bytes) * c.channels.count() as u64;
    if ssnd.end - samples_start != c.frames * frame_bytes {
        return Err(PcmHashError::malformed(
            C,
            "SSND length does not match COMM frame count",
        ));
    }
    Ok(RawPcmLayout {
        order: c.order,
        eight_bit: EightBit::Signed,
        container_bytes: c.container_bytes,
        channels: c.channels,
        rate: c.rate,
        data: samples_start..ssnd.end,
        frames: c.frames,
    })
}
