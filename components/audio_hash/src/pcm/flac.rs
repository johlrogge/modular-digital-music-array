//! FLAC: decode with claxon over a synthesized stream (`fLaC`, the file's
//! STREAMINFO, then the audio frames) so the decoder never sees any other
//! metadata or tags.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::panic::{catch_unwind, AssertUnwindSafe};

use music_primitives::PcmHash;

use super::{AudioHashError, Channels, PcmHashError, PcmHasher, SampleRate, StoredBits};
use crate::tags::be_u32;
use crate::{flac, Container};

const C: Container = Container::Flac;
const STREAMINFO_LEN: usize = 34;

struct StreamInfo {
    rate: SampleRate,
    channels: Channels,
    bits: StoredBits,
    /// 0 = unknown.
    total_samples: u64,
    /// All zero = not set.
    md5: [u8; 16],
}

fn parse_streaminfo(b: &[u8; STREAMINFO_LEN]) -> Result<StreamInfo, PcmHashError> {
    // bytes 10..18: 20 bits rate, 3 bits channels-1, 5 bits bps-1, 36 bits total
    let rate = (u32::from(b[10]) << 12) | (u32::from(b[11]) << 4) | u32::from(b[12] >> 4);
    let channels = u32::from((b[12] >> 1) & 0x07) + 1;
    let bps = (((b[12] & 1) << 4) | (b[13] >> 4)) + 1;
    let total_samples = (u64::from(b[13] & 0x0F) << 32) | u64::from(be_u32(b, 14));
    let mut md5 = [0u8; 16];
    md5.copy_from_slice(&b[18..34]);
    Ok(StreamInfo {
        rate: SampleRate::new(rate)
            .ok_or_else(|| PcmHashError::malformed(C, "zero sample rate"))?,
        channels: Channels::try_from(channels)?,
        bits: StoredBits::new(bps).ok_or(PcmHashError::Decode("bad bits per sample"))?,
        total_samples,
        md5,
    })
}

fn map_claxon(e: claxon::Error) -> PcmHashError {
    match e {
        claxon::Error::IoError(e) => PcmHashError::Container(AudioHashError::Io(e)),
        claxon::Error::FormatError(s) => PcmHashError::Decode(s),
        claxon::Error::Unsupported(s) => PcmHashError::Unsupported(s),
    }
}

/// Decode every frame of `stream` into `hasher`, checking each block against
/// STREAMINFO.
fn decode<R: Read>(stream: R, si: &StreamInfo, hasher: &mut PcmHasher) -> Result<(), PcmHashError> {
    let mut reader = claxon::FlacReader::new(stream).map_err(map_claxon)?;
    let mut frames = reader.blocks();
    let bits = u32::from(si.bits.bits());
    let (min, max) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
    let mut buf: Vec<i32> = Vec::new();
    while let Some(block) = frames.read_next_or_eof(buf).map_err(map_claxon)? {
        if block.channels() as usize != si.channels.count() {
            return Err(PcmHashError::Decode(
                "frame channel count differs from STREAMINFO",
            ));
        }
        let planes: [&[i32]; 2] = [
            block.channel(0),
            block.channel(si.channels.count() as u32 - 1),
        ];
        // Mono: both entries are channel 0, only the first is written.
        if planes
            .iter()
            .take(si.channels.count())
            .flat_map(|p| p.iter())
            .any(|&s| !(min..=max).contains(&i64::from(s)))
        {
            return Err(PcmHashError::Decode(
                "sample exceeds STREAMINFO bits per sample",
            ));
        }
        hasher.write_planar(&planes[..si.channels.count()])?;
        buf = block.into_buffer();
    }
    Ok(())
}

pub(crate) fn hash<R: Read + Seek>(r: &mut R, file_len: u64) -> Result<PcmHash, PcmHashError> {
    let l = flac::layout(r, file_len)?;
    let range = l
        .streaminfo
        .ok_or_else(|| PcmHashError::malformed(C, "STREAMINFO is not the first metadata block"))?;
    let mut si_bytes = [0u8; STREAMINFO_LEN];
    r.seek(SeekFrom::Start(range.start))?;
    r.read_exact(&mut si_bytes)?;
    let si = parse_streaminfo(&si_bytes)?;

    let mut head = b"fLaC".to_vec();
    head.extend([0x80, 0, 0, STREAMINFO_LEN as u8]);
    head.extend(si_bytes);
    r.seek(SeekFrom::Start(l.audio.start))?;
    let stream = Cursor::new(head).chain(r.by_ref().take(l.audio.end - l.audio.start));

    let mut hasher = PcmHasher::new(si.channels, si.rate, si.bits);
    if si.md5 != [0u8; 16] {
        hasher = hasher.with_md5();
    }
    // Decode in a catch_unwind: a decoder bug must fail this one file, not
    // the caller.
    catch_unwind(AssertUnwindSafe(|| decode(stream, &si, &mut hasher)))
        .map_err(|_| PcmHashError::DecoderPanic)??;
    let digest = hasher.finish()?;

    if digest.frames == 0 {
        return Err(PcmHashError::no_audio());
    }
    if si.total_samples != 0 && si.total_samples != digest.frames {
        return Err(PcmHashError::SampleCountMismatch {
            declared: si.total_samples,
            decoded: digest.frames,
        });
    }
    if let Some(md5) = digest.md5 {
        if md5 != si.md5 {
            return Err(PcmHashError::Md5Mismatch);
        }
    }
    Ok(digest.hash)
}
