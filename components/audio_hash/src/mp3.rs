//! MP3: audio is everything after the head ID3v2 chain and zero padding,
//! minus trailing tags. The Xing/LAME frame stays in the range.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use crate::tags::{head_id3v2_len, read_at, strip_trailers};
use crate::{AudioHashError, Container};

const C: Container = Container::Mp3;

/// Upper bound on zero padding skipped after the tag chain.
pub(crate) const MAX_ZERO_PADDING: u64 = 64 * 1024;

/// MPEG audio frame sync: `0xFF` then a byte with the top three bits set.
pub(crate) fn is_frame_sync(b: [u8; 2]) -> bool {
    b[0] == 0xFF && b[1] & 0xE0 == 0xE0
}

/// Number of leading zero bytes at `start`, bounded by `MAX_ZERO_PADDING`.
pub(crate) fn zero_padding<R: Read + Seek>(
    r: &mut R,
    start: u64,
    file_len: u64,
) -> Result<u64, AudioHashError> {
    let n = file_len.saturating_sub(start).min(MAX_ZERO_PADDING) as usize;
    let mut buf = vec![0u8; n];
    r.seek(SeekFrom::Start(start))?;
    r.read_exact(&mut buf)?;
    Ok(buf.iter().take_while(|b| **b == 0).count() as u64)
}

// A single-element Vec of a Range is exactly what we mean here.
#[allow(clippy::single_range_in_vec_init)]
pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<Vec<Range<u64>>, AudioHashError> {
    let mut start = head_id3v2_len(r, file_len)?;
    start += zero_padding(r, start, file_len)?;

    let end = strip_trailers(r, start, file_len, C)?;
    if end <= start {
        return Err(AudioHashError::NoAudio);
    }
    if end - start < 2 {
        return Err(C.malformed("audio shorter than a frame header"));
    }
    if !is_frame_sync(read_at(r, start)?) {
        return Err(C.malformed("no frame sync"));
    }
    Ok(vec![start..end])
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    fn lay(b: &[u8]) -> Result<Vec<Range<u64>>, AudioHashError> {
        lay_with(layout, b)
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[rstest]
    fn mp31_head_id3v2_is_ignored(
        #[values(0, 100, 5000)] size: usize,
        #[values(false, true)] footer: bool,
    ) {
        let frames = mp3_frames(6);
        let b = cat(&[&id3v2(size, footer), &frames]);
        assert_eq!(hash_of(&b), hash_of(&frames));
    }

    #[test]
    fn mp31_zero_padding_after_tag_is_ignored() {
        let frames = mp3_frames(6);
        let b = cat(&[&id3v2(50, false), &[0u8; 700], &frames]);
        let start = 60 + 700;
        assert_eq!(
            lay(&b).unwrap(),
            vec![start as u64..(start + frames.len()) as u64]
        );
    }

    #[test]
    fn mp32_trailing_tags_are_ignored() {
        let frames = mp3_frames(6);
        let bare = hash_of(&frames);
        let (v1, ape_h, ape_f, lyr) = (id3v1(), ape(100, true), ape(100, false), lyrics3v2(50));
        let cases: Vec<Vec<u8>> = vec![
            cat(&[&frames, &v1]),
            cat(&[&frames, &id3v1_extended(), &v1]),
            cat(&[&frames, &ape_h]),
            cat(&[&frames, &ape_f]),
            cat(&[&frames, &lyr]),
            cat(&[&frames, &lyr, &ape_h, &v1]),
            cat(&[&frames, &v1, &lyr, &ape_f]),
            cat(&[&id3v2(80, true), &frames, &ape_f, &lyr, &v1]),
        ];
        for (i, b) in cases.iter().enumerate() {
            assert_eq!(hash_of(b), bare, "case {i}");
        }
    }

    #[test]
    fn mp33_different_frames_differ() {
        assert_ne!(hash_of(&mp3_frames(6)), hash_of(&mp3_frames(7)));
        let mut f = mp3_frames(6);
        f[50] ^= 1;
        assert_ne!(hash_of(&f), hash_of(&mp3_frames(6)));
    }

    #[test]
    fn mp34_oversized_ape_is_malformed() {
        let mut b = cat(&[&mp3_frames(2), &ape(100, false)]);
        let n = b.len();
        b[n - 32 + 12..n - 32 + 16].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
        assert!(is_malformed(lay(&b), C));
    }

    #[test]
    fn missing_sync_is_malformed() {
        let b = cat(&[&id3v2(10, false), b"not audio at all"]);
        assert!(is_malformed(lay(&b), C));
    }

    #[test]
    fn one_byte_of_audio_is_malformed_not_io() {
        assert!(is_malformed(lay(&[0xFF]), C));
    }

    #[test]
    fn x4_layout_ranges_are_exact() {
        let frames = mp3_frames(3);
        let b = cat(&[
            &id3v2(20, false),
            &[0u8; 4],
            &frames,
            &ape(10, true),
            &id3v1(),
        ]);
        let start = 30 + 4;
        assert_eq!(
            lay(&b).unwrap(),
            vec![start as u64..(start + frames.len()) as u64]
        );
    }
}
