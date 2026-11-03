//! Chunk walking shared by the RIFF (little-endian) and IFF (big-endian)
//! containers: WAV and AIFF.

use std::io::{Read, Seek};
use std::ops::Range;

use crate::tags::{be_u32, le_u32, read_at};
use crate::{AudioHashError, Container};

/// Byte order of chunk sizes (and of sample data in the PCM readers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endian {
    Little,
    Big,
}

/// A chunk whose size field may be `0xFFFF_FFFF`, meaning "runs to end of
/// file" (streamed WAV `data`). A size of 0 is a literally empty payload.
pub(crate) struct Unbounded {
    pub id: [u8; 4],
    /// Reason reported when this chunk is truncated.
    pub truncated: &'static str,
}

const UNBOUNDED_SIZE: u64 = 0xFFFF_FFFF;

/// Walk the chunks of a file starting at `start` and return the payload
/// range of the first chunk with each wanted id, in the order given. Stops as
/// soon as all are found. Chunks are padded to even length. A chunk reaching
/// past the end of the file is Malformed.
pub(crate) fn walk<R: Read + Seek, const N: usize>(
    r: &mut R,
    file_len: u64,
    container: Container,
    order: Endian,
    start: u64,
    wanted: [&[u8; 4]; N],
    unbounded: Option<&Unbounded>,
) -> Result<[Option<Range<u64>>; N], AudioHashError> {
    let mut found: [Option<Range<u64>>; N] = std::array::from_fn(|_| None);
    let mut pos = start;
    while found.iter().any(Option::is_none) && file_len.saturating_sub(pos) >= 8 {
        let h: [u8; 8] = read_at(r, pos)?;
        let id: [u8; 4] = [h[0], h[1], h[2], h[3]];
        let size = u64::from(match order {
            Endian::Little => le_u32(&h, 4),
            Endian::Big => be_u32(&h, 4),
        });
        let payload = pos + 8;
        let ub = unbounded.filter(|u| u.id == id);
        let end = if ub.is_some() && size == UNBOUNDED_SIZE {
            file_len
        } else {
            payload + size
        };
        if end > file_len {
            return Err(container.malformed(match ub {
                Some(u) => u.truncated,
                None => "chunk extends past end of file",
            }));
        }
        if let Some(i) = wanted.iter().position(|w| **w == id) {
            if found[i].is_none() {
                found[i] = Some(payload..end);
            }
        }
        pos = end + (end - payload) % 2;
    }
    Ok(found)
}
