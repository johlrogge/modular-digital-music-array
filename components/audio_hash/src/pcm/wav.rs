//! WAV: parse `fmt ` into a [`RawPcmLayout`], reusing AudioHash v1's chunk walk.

use std::io::{Read, Seek, SeekFrom};

use super::raw::{ContainerBytes, EightBit, RawPcmLayout};
use super::{Channels, PcmHashError, SampleRate};
use crate::iff::Endian;
use crate::tags::{le_u16, le_u32};
use crate::{wav, Container};

const C: Container = Container::Wav;

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// Bytes 4..16 of the SubFormat GUID `xxxxxxxx-0000-0010-8000-00aa00389b71`.
const GUID_TAIL: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
/// WAVEFORMATEXTENSIBLE: 18-byte WAVEFORMATEX + 22 bytes.
const EXTENSIBLE_LEN: u64 = 40;

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<RawPcmLayout, PcmHashError> {
    let l = wav::layout(r, file_len)?;
    let fmt_len = l.fmt.end - l.fmt.start;
    if fmt_len < 16 {
        return Err(PcmHashError::malformed(
            C,
            "fmt chunk shorter than 16 bytes",
        ));
    }
    let mut fmt = vec![0u8; fmt_len.min(EXTENSIBLE_LEN) as usize];
    r.seek(SeekFrom::Start(l.fmt.start))?;
    r.read_exact(&mut fmt)?;

    let mut tag = le_u16(&fmt, 0);
    if tag == FORMAT_EXTENSIBLE {
        if fmt_len < EXTENSIBLE_LEN {
            return Err(PcmHashError::malformed(C, "extensible fmt chunk too short"));
        }
        let guid = &fmt[24..40];
        if guid[4..] != GUID_TAIL {
            return Err(PcmHashError::Unsupported("wav: unknown SubFormat"));
        }
        tag = match le_u32(guid, 0) {
            1 => FORMAT_PCM,
            3 => FORMAT_FLOAT,
            _ => return Err(PcmHashError::Unsupported("wav: non-PCM SubFormat")),
        };
    }
    match tag {
        FORMAT_PCM => {}
        FORMAT_FLOAT => return Err(PcmHashError::Unsupported("wav: float PCM")),
        _ => return Err(PcmHashError::Unsupported("wav: non-PCM format")),
    }

    let channel_count = le_u16(&fmt, 2);
    let channels = Channels::from_field(u32::from(channel_count), C)?;
    let rate = SampleRate::new(le_u32(&fmt, 4))
        .ok_or_else(|| PcmHashError::malformed(C, "zero sample rate"))?;
    let block_align = le_u16(&fmt, 12);
    if block_align == 0 || !block_align.is_multiple_of(channel_count) {
        return Err(PcmHashError::malformed(
            C,
            "block align is not a multiple of the channel count",
        ));
    }
    // ADR-012: stored bits come from the block align; wBitsPerSample,
    // wValidBitsPerSample and dwChannelMask are ignored.
    let container = ContainerBytes::new(block_align / channel_count).ok_or(
        PcmHashError::Unsupported("wav: sample container over 4 bytes"),
    )?;

    // `data` must be whole frames. With an unbounded (0xFFFFFFFF) data size
    // the payload runs to EOF, so a trailing chunk of whole frames is hashed
    // as audio (consistent with AudioHash v1); anything else is Malformed.
    RawPcmLayout::new(
        Endian::Little,
        EightBit::Unsigned,
        container,
        channels,
        rate,
        l.data,
    )
    .ok_or_else(|| PcmHashError::malformed(C, "data is not a whole number of frames"))
}
