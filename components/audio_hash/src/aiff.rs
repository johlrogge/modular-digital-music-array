//! AIFF / AIFF-C: ranges of the first `COMM` and `SSND` chunk payloads. Every
//! other chunk is ignored.

use std::io::{Read, Seek};
use std::ops::Range;

use crate::iff::{self, Endian};
use crate::tags::read_at;
use crate::{AudioHashError, Container};

const C: Container = Container::Aiff;

/// Byte ranges of an AIFF file's `COMM` and `SSND` payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiffLayout {
    /// `FORM` type is `AIFC` (COMM carries a compression type).
    pub is_aifc: bool,
    pub comm: Range<u64>,
    pub ssnd: Range<u64>,
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<AiffLayout, AudioHashError> {
    if file_len < 12 {
        return Err(C.malformed("shorter than FORM header"));
    }
    let head: [u8; 12] = read_at(r, 0)?;
    if &head[0..4] != b"FORM" || !matches!(&head[8..12], b"AIFF" | b"AIFC") {
        return Err(C.malformed("not a FORM/AIFF file"));
    }
    let [comm, ssnd] = iff::walk(r, file_len, C, Endian::Big, 12, [b"COMM", b"SSND"], None)?;
    Ok(AiffLayout {
        is_aifc: &head[8..12] == b"AIFC",
        comm: comm.ok_or_else(|| C.malformed("missing COMM chunk"))?,
        ssnd: ssnd.ok_or_else(|| C.malformed("missing SSND chunk"))?,
    })
}
