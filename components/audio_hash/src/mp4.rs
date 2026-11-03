//! MP4/M4A: ranges are all top-level `mdat` payloads in file order.

use std::io::{Read, Seek};
use std::ops::Range;

use crate::tags::{be_u32, be_u64, read_at};
use crate::{AudioHashError, Container};

const C: Container = Container::Mp4;

/// Upper bound on `mdat` boxes, so hostile input (back-to-back empty boxes)
/// cannot make the range list grow without limit.
const MAX_MDAT_BOXES: usize = 1 << 16;

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<Vec<Range<u64>>, AudioHashError> {
    let mut ranges = Vec::new();
    let mut pos = 0u64;
    let mut first = true;
    while pos < file_len {
        if file_len - pos < 8 {
            return Err(C.malformed("box header truncated"));
        }
        let h: [u8; 8] = read_at(r, pos)?;
        let size32 = be_u32(&h, 0);
        let typ = [h[4], h[5], h[6], h[7]];
        let (size, header) = match size32 {
            1 => {
                if file_len - pos < 16 {
                    return Err(C.malformed("largesize header truncated"));
                }
                (be_u64(&read_at(r, pos + 8)?), 16)
            }
            0 => (file_len - pos, 8),
            n => (u64::from(n), 8),
        };
        if size < header {
            return Err(C.malformed("box size smaller than its header"));
        }
        let end = pos
            .checked_add(size)
            .filter(|e| *e <= file_len)
            .ok_or_else(|| C.malformed("box extends past end of file"))?;
        if first && &typ != b"ftyp" {
            return Err(AudioHashError::UnsupportedContainer);
        }
        first = false;
        if &typ == b"mdat" {
            if ranges.len() >= MAX_MDAT_BOXES {
                return Err(C.malformed("too many mdat boxes"));
            }
            ranges.push(pos + header..end);
        }
        pos = end;
    }
    if first {
        return Err(AudioHashError::UnsupportedContainer);
    }
    if ranges.iter().all(Range::is_empty) {
        return Err(AudioHashError::NoAudio);
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use pretty_assertions::assert_eq;

    fn lay(b: &[u8]) -> Result<Vec<Range<u64>>, AudioHashError> {
        lay_with(layout, b)
    }

    fn malformed(b: &[u8]) -> bool {
        is_malformed(lay(b), C)
    }

    const AUDIO: &[u8] = b"the encoded audio samples";

    fn moov(tags: &[u8]) -> Vec<u8> {
        mp4_box(b"moov", &mp4_box(b"udta", &mp4_box(b"ilst", tags)))
    }

    #[test]
    fn mp41_metadata_and_box_order_are_ignored() {
        let mdat = mp4_box(b"mdat", AUDIO);
        let want = hash_of(&mp4(&[ftyp(), moov(b"one"), mdat.clone()]));
        let cases = [
            mp4(&[ftyp(), moov(b"a much longer tag value"), mdat.clone()]),
            mp4(&[
                ftyp(),
                mp4_box(b"free", &[0; 100]),
                moov(b"one"),
                mdat.clone(),
            ]),
            mp4(&[ftyp(), mdat.clone(), moov(b"one")]),
        ];
        for (i, c) in cases.iter().enumerate() {
            assert_eq!(hash_of(c), want, "case {i}");
        }
    }

    #[test]
    fn mp42_different_mdat_differs() {
        let a = hash_of(&mp4(&[ftyp(), mp4_box(b"mdat", AUDIO)]));
        let b = hash_of(&mp4(&[ftyp(), mp4_box(b"mdat", b"other samples!!!")]));
        assert_ne!(a, b);
    }

    #[test]
    fn mp43_largesize_and_to_eof_boxes_parse() {
        let want = hash_of(&mp4(&[ftyp(), mp4_box(b"mdat", AUDIO)]));
        let large = mp4(&[ftyp(), mp4_box_large(b"mdat", AUDIO)]);
        let to_eof = mp4(&[ftyp(), mp4_box_to_eof(b"mdat", AUDIO)]);
        assert_eq!(hash_of(&large), want);
        assert_eq!(hash_of(&to_eof), want);
    }

    #[test]
    fn mp44_bad_box_sizes_are_malformed() {
        let small = mp4(&[ftyp(), vec![0, 0, 0, 4, b'f', b'r', b'e', b'e']]);
        assert!(malformed(&small));
        let past = mp4(&[
            ftyp(),
            vec![0, 0, 0xFF, 0xFF, b'm', b'd', b'a', b't', 1, 2, 3],
        ]);
        assert!(malformed(&past));
        let trunc_header = mp4(&[ftyp(), vec![0, 0, 0]]);
        assert!(malformed(&trunc_header));
        let mut huge_large = ftyp();
        huge_large.extend([0, 0, 0, 1, b'm', b'd', b'a', b't']);
        huge_large.extend(u64::MAX.to_be_bytes());
        assert!(malformed(&huge_large));
        let small_large = mp4(&[ftyp(), {
            let mut v = vec![0, 0, 0, 1, b'm', b'd', b'a', b't'];
            v.extend(9u64.to_be_bytes());
            v
        }]);
        assert!(malformed(&small_large));
    }

    #[test]
    fn first_box_must_be_ftyp() {
        assert!(matches!(
            lay(&mp4_box(b"moov", b"x")),
            Err(AudioHashError::UnsupportedContainer)
        ));
        assert!(matches!(
            lay(&[]),
            Err(AudioHashError::UnsupportedContainer)
        ));
    }

    #[test]
    fn no_mdat_is_no_audio() {
        assert!(matches!(
            lay(&mp4(&[ftyp(), moov(b"x")])),
            Err(AudioHashError::NoAudio)
        ));
    }

    #[test]
    fn too_many_mdat_boxes_is_malformed() {
        let mut b = ftyp();
        for _ in 0..=MAX_MDAT_BOXES {
            b.extend(mp4_box(b"mdat", &[]));
        }
        assert!(malformed(&b));
    }

    #[test]
    fn x4_layout_ranges_are_exact() {
        let f = ftyp();
        let b = mp4(&[
            f.clone(),
            mp4_box(b"mdat", b"abc"),
            moov(b"t"),
            mp4_box_large(b"mdat", b"defg"),
        ]);
        let first = f.len() + 8;
        let moov_len = moov(b"t").len();
        let second = f.len() + 11 + moov_len + 16;
        assert_eq!(
            lay(&b).unwrap(),
            vec![
                first as u64..(first + 3) as u64,
                second as u64..(second + 4) as u64
            ]
        );
    }
}
