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

/// Where an AIFF / AIFF-C integer PCM stream's samples are and how they are
/// stored (plain AIFF, or AIFF-C `NONE`/`twos`/`sowt`; mono or stereo), as
/// PcmHash reads it: SSND bounded exactly by COMM's frame count. The lenient
/// reader (for playback) bounds it by what is really in the file instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiffPcmLayout {
    raw: RawPcmLayout,
    significant_bits: u8,
    declared_frames: u64,
}

impl AiffPcmLayout {
    pub fn byte_order(&self) -> ByteOrder {
        match self.raw.order() {
            Endian::Big => ByteOrder::Big,
            Endian::Little => ByteOrder::Little,
        }
    }

    /// Bytes per stored sample, 1 to 4 (`ceil(significant_bits / 8)`).
    pub fn container_bytes(&self) -> usize {
        self.raw.container_bytes()
    }

    /// Bits per sample as COMM declares them, 1 to 32. Samples narrower than
    /// the container are left-justified in it.
    pub fn significant_bits(&self) -> u8 {
        self.significant_bits
    }

    pub fn channels(&self) -> u8 {
        self.raw.channels().count() as u8
    }

    pub fn sample_rate(&self) -> u32 {
        self.raw.rate().get()
    }

    /// Number of sample frames in [`data`](Self::data).
    pub fn frames(&self) -> u64 {
        self.raw.frames()
    }

    /// `numSampleFrames` as COMM declares it. Differs from [`frames`](Self::frames)
    /// only for a lenient read of a truncated or inconsistent file.
    pub fn declared_frames(&self) -> u64 {
        self.declared_frames
    }

    /// Whether the file holds all the declared frames (always true for a strict
    /// read).
    pub fn is_exact(&self) -> bool {
        self.raw.frames() == self.declared_frames
    }

    /// Exact byte range of the sample data in the file.
    pub fn data(&self) -> Range<u64> {
        self.raw.data().clone()
    }

    /// Append the sign-extended values of whole raw samples, at the full width
    /// of the container.
    pub fn decode_into(&self, raw: &[u8], out: &mut Vec<i32>) {
        self.raw.decode_into(raw, out);
    }
}

pub(crate) fn pcm_layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
    strictness: Strictness,
) -> Result<AiffPcmLayout, PcmHashError> {
    let p = parse(r, file_len, strictness)?;
    Ok(AiffPcmLayout {
        raw: p.layout,
        // At most 32: the container is at most 4 bytes.
        significant_bits: p.sample_size as u8,
        declared_frames: p.declared_frames,
    })
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<RawPcmLayout, PcmHashError> {
    parse(r, file_len, Strictness::Strict).map(|p| p.layout)
}

/// The stream properties COMM declares, after the same validation as the
/// hash (SSND length against the frame count included).
pub(crate) fn info<R: Read + Seek>(r: &mut R, file_len: u64) -> Result<PcmInfo, PcmHashError> {
    let p = parse(r, file_len, Strictness::Strict)?;
    let l = aiff::layout(r, file_len)?;
    let encoding = encoding(r, &l)?;
    Ok(PcmInfo {
        container: C,
        channels: p.layout.channels().count() as u8,
        sample_rate: p.layout.rate().get(),
        frames: p.layout.frames(),
        // At most 32: the container is at most 4 bytes.
        bits_per_sample: p.sample_size as u8,
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

/// How exactly the sample bytes must match COMM's frame count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strictness {
    /// Identity (PcmHash, AudioHash): SSND holds exactly `numSampleFrames` frames.
    Strict,
    /// Playback: a truncated file or an SSND longer or shorter than COMM says
    /// gives the whole frames that are there, at most `numSampleFrames`.
    Lenient,
}

struct Parsed {
    layout: RawPcmLayout,
    /// COMM's `sampleSize` (bits per sample, as declared).
    sample_size: u16,
    /// COMM's `numSampleFrames`.
    declared_frames: u64,
}

fn parse<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
    strictness: Strictness,
) -> Result<Parsed, PcmHashError> {
    let l = aiff::layout_with(r, file_len, strictness == Strictness::Lenient)?;
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
    let new = |data: Range<u64>| {
        RawPcmLayout::new(
            c.order,
            EightBit::Signed,
            c.container,
            c.channels,
            c.rate,
            data,
        )
    };
    let layout = match strictness {
        // The sample bytes after `offset` must be exactly numSampleFrames frames.
        Strictness::Strict => new(samples_start..l.ssnd.end)
            .filter(|layout| layout.frames() == c.frames)
            .ok_or_else(|| {
                PcmHashError::malformed(C, "SSND length does not match COMM frame count")
            })?,
        Strictness::Lenient => {
            let frame_bytes = (c.container.get() * c.channels.count()) as u64;
            let available = (l.ssnd.end - samples_start) / frame_bytes;
            let frames = available.min(c.frames);
            new(samples_start..samples_start + frames * frame_bytes)
                .expect("a whole number of frames")
        }
    };
    Ok(Parsed {
        layout,
        sample_size: c.sample_size,
        declared_frames: c.frames,
    })
}
