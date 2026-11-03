//! Tag detection shared by the container modules: head ID3v2 chains and
//! trailing ID3v1 / APEv2 / Lyrics3v2 / appended ID3v2 blocks.

use std::io::{Read, Seek, SeekFrom};

use crate::{AudioHashError, Container};

pub(crate) fn read_at<R: Read + Seek, const N: usize>(
    r: &mut R,
    pos: u64,
) -> Result<[u8; N], AudioHashError> {
    r.seek(SeekFrom::Start(pos))?;
    let mut buf = [0u8; N];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

// Fixed-width field readers. Callers always pass buffers whose length is
// known at compile time, so the indexing here cannot go out of bounds.
pub(crate) fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn be_u16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

pub(crate) fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub(crate) fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub(crate) fn be_u24(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([0, b[at], b[at + 1], b[at + 2]])
}

pub(crate) fn be_u64(b: &[u8; 8]) -> u64 {
    u64::from_be_bytes(*b)
}

fn syncsafe(b: [u8; 4]) -> Option<u64> {
    if b.iter().any(|x| x & 0x80 != 0) {
        None
    } else {
        Some(b.iter().fold(0u64, |acc, x| (acc << 7) | u64::from(*x)))
    }
}

/// Total length of the chain of ID3v2 tags at the start of the file.
/// A tag that would extend past EOF is not consumed.
pub(crate) fn head_id3v2_len<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<u64, AudioHashError> {
    let mut pos = 0u64;
    loop {
        if file_len.saturating_sub(pos) < 10 {
            return Ok(pos);
        }
        let h: [u8; 10] = read_at(r, pos)?;
        if &h[0..3] != b"ID3" || h[3] == 0xFF {
            return Ok(pos);
        }
        let Some(size) = syncsafe([h[6], h[7], h[8], h[9]]) else {
            return Ok(pos);
        };
        let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
        match pos.checked_add(10 + size + footer) {
            Some(next) if next <= file_len => pos = next,
            _ => return Ok(pos),
        }
    }
}

/// Move `end` back over any trailing tag blocks, never below `start`.
pub(crate) fn strip_trailers<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    container: Container,
) -> Result<u64, AudioHashError> {
    let mut end = end.max(start);
    loop {
        let avail = end - start;

        // Order matters: the self-describing, length-prefixed tags are checked
        // first, ID3v1 (a bare 3-byte signature) last, so that "TAG" bytes
        // inside another tag can never be mistaken for ID3v1. Once a
        // signature matches we are committed to that tag type: any later
        // inconsistency is Malformed, never a fall-through.

        // APEv1/APEv2 footer
        if avail >= 32 {
            let f: [u8; 32] = read_at(r, end - 32)?;
            if &f[..8] == b"APETAGEX" && matches!(le_u32(&f, 8), 1000 | 2000) {
                let size = u64::from(le_u32(&f, 12));
                let has_header = le_u32(&f, 20) & 0x8000_0000 != 0;
                if size < 32 {
                    return Err(container.malformed("APE size smaller than footer"));
                }
                let total = size + if has_header { 32 } else { 0 };
                if total > avail {
                    return Err(container.malformed("APE tag larger than file"));
                }
                if has_header && read_at::<_, 8>(r, end - total)? != *b"APETAGEX" {
                    return Err(container.malformed("APE header missing"));
                }
                end -= total;
                continue;
            }
        }

        // Lyrics3v2
        if avail >= 15 {
            let l: [u8; 15] = read_at(r, end - 15)?;
            if &l[6..] == b"LYRICS200" && l[..6].iter().all(u8::is_ascii_digit) {
                let size = l[..6]
                    .iter()
                    .fold(0u64, |acc, d| acc * 10 + u64::from(d - b'0'));
                let total = size + 15;
                if total > avail {
                    return Err(container.malformed("Lyrics3v2 block larger than file"));
                }
                if read_at::<_, 11>(r, end - total)? != *b"LYRICSBEGIN" {
                    return Err(container.malformed("Lyrics3v2 LYRICSBEGIN missing"));
                }
                end -= total;
                continue;
            }
        }

        // Appended ID3v2.4 (footer present)
        if avail >= 10 {
            let f: [u8; 10] = read_at(r, end - 10)?;
            if &f[..3] == b"3DI" && f[3] == 4 && f[4] != 0xFF && f[5] & 0x10 != 0 {
                if let Some(size) = syncsafe([f[6], f[7], f[8], f[9]]) {
                    let total = size + 20;
                    if total > avail {
                        return Err(container.malformed("ID3v2 footer tag larger than file"));
                    }
                    let h: [u8; 4] = read_at(r, end - total)?;
                    if &h[..3] != b"ID3" || h[3] != 4 {
                        return Err(container.malformed("ID3v2 header missing"));
                    }
                    end -= total;
                    continue;
                }
            }
        }

        // ID3v1 (+ extended "TAG+" block in front of it)
        if avail >= 128 && read_at::<_, 3>(r, end - 128)? == *b"TAG" {
            end -= 128;
            if end - start >= 227 && read_at::<_, 4>(r, end - 227)? == *b"TAG+" {
                end -= 227;
            }
            continue;
        }

        return Ok(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use pretty_assertions::assert_eq;
    use std::io::Cursor;

    fn head(bytes: &[u8]) -> u64 {
        head_id3v2_len(&mut Cursor::new(bytes), bytes.len() as u64).unwrap()
    }

    fn strip(bytes: &[u8], start: u64) -> Result<u64, AudioHashError> {
        strip_trailers(
            &mut Cursor::new(bytes),
            start,
            bytes.len() as u64,
            Container::Mp3,
        )
    }

    #[test]
    fn head_chain_skips_back_to_back_tags() {
        let mut b = id3v2(20, false);
        b.extend(id3v2(7, true));
        let tags = b.len() as u64;
        b.extend(mp3_frames(1));
        assert_eq!(head(&b), tags);
    }

    #[test]
    fn head_ignores_non_tags_and_overshoot() {
        assert_eq!(head(&mp3_frames(2)), 0);
        let mut b = id3v2(100, false);
        b.truncate(50);
        assert_eq!(head(&b), 0);
        assert_eq!(head(b"ID3"), 0);
    }

    #[test]
    fn strip_never_goes_below_start() {
        let b = id3v1();
        assert_eq!(strip(&b, 128).unwrap(), 128);
        assert_eq!(strip(&b, 0).unwrap(), 0);
        assert_eq!(strip(&[], 0).unwrap(), 0);
    }

    #[test]
    fn strip_extended_id3v1() {
        let mut b = mp3_frames(2);
        let audio = b.len() as u64;
        b.extend(id3v1_extended());
        b.extend(id3v1());
        assert_eq!(strip(&b, 0).unwrap(), audio);
    }

    #[test]
    fn strip_each_kind() {
        let audio = mp3_frames(3);
        let n = audio.len() as u64;
        for t in [
            id3v1(),
            ape(40, true),
            ape(40, false),
            lyrics3v2(60),
            id3v2(30, true),
        ] {
            let mut b = audio.clone();
            b.extend(t);
            assert_eq!(strip(&b, 0).unwrap(), n);
        }
    }

    #[test]
    fn ape_header_tag_at_id3v1_offset_is_not_mistaken_for_id3v1() {
        // ape(67, true) is 131 bytes, so the "TAG" inside its "APETAGEX"
        // header sits exactly 128 bytes before EOF. The old ID3v1-first order
        // stripped only 128 bytes and so hashed part of the APE tag.
        let mp3 = mp3_frames(3);
        let mut tagged = mp3.clone();
        tagged.extend(ape(67, true));
        assert_eq!(tagged.len() - mp3.len(), 131);
        assert_eq!(hash_of(&tagged), hash_of(&mp3));

        let f = flac(&[streaminfo()], &flac_frames(3));
        let mut tagged = f.clone();
        tagged.extend(ape(67, true));
        assert_eq!(hash_of(&tagged), hash_of(&f));
    }

    #[test]
    fn matched_ape_with_header_flag_requires_header_magic() {
        let audio = mp3_frames(2);
        let mut b = audio.clone();
        b.extend(ape(100, true));
        let hdr = audio.len();
        b[hdr..hdr + 8].copy_from_slice(b"XXXXXXXX");
        assert!(matches!(
            strip(&b, 0),
            Err(AudioHashError::Malformed { .. })
        ));
    }

    #[test]
    fn matched_lyrics3v2_requires_lyricsbegin() {
        let audio = mp3_frames(2);
        let mut b = audio.clone();
        b.extend(lyrics3v2(50));
        let at = audio.len();
        b[at..at + 11].copy_from_slice(b"XXXXXXXXXXX");
        assert!(matches!(
            strip(&b, 0),
            Err(AudioHashError::Malformed { .. })
        ));
    }

    #[test]
    fn matched_id3v2_footer_requires_header() {
        let audio = mp3_frames(2);
        let mut b = audio.clone();
        b.extend(id3v2(30, true));
        let at = audio.len();
        b[at..at + 3].copy_from_slice(b"XXX");
        assert!(matches!(
            strip(&b, 0),
            Err(AudioHashError::Malformed { .. })
        ));
    }

    #[test]
    fn ape_footer_with_unknown_version_is_not_ape() {
        let audio = mp3_frames(2);
        let mut b = audio.clone();
        b.extend(ape(100, false));
        let n = b.len();
        b[n - 32 + 8..n - 32 + 12].copy_from_slice(&1234u32.to_le_bytes());
        assert_eq!(strip(&b, 0).unwrap(), n as u64);
    }

    #[test]
    fn oversized_blocks_are_malformed() {
        let mut b = mp3_frames(1);
        b.extend(lyrics3v2(60));
        let n = b.len();
        // Inflate the Lyrics3v2 size digits.
        b[n - 15..n - 9].copy_from_slice(b"999999");
        assert!(matches!(
            strip(&b, 0),
            Err(AudioHashError::Malformed { .. })
        ));
    }
}
