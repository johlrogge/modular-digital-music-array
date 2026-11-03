//! FLAC: the audio range is everything after the last metadata block, minus
//! trailing tags.

use std::io::{Read, Seek};
use std::ops::Range;

use crate::tags::{be_u24, head_id3v2_len, read_at, strip_trailers};
use crate::{AudioHashError, Container};

const C: Container = Container::Flac;

// A single-element Vec of a Range is exactly what we mean here.
#[allow(clippy::single_range_in_vec_init)]
pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<Vec<Range<u64>>, AudioHashError> {
    let mut pos = head_id3v2_len(r, file_len)?;
    if file_len.saturating_sub(pos) < 4 || read_at::<_, 4>(r, pos)? != *b"fLaC" {
        return Err(C.malformed("missing fLaC marker"));
    }
    pos += 4;
    loop {
        if file_len.saturating_sub(pos) < 4 {
            return Err(C.malformed("metadata block header truncated"));
        }
        let h: [u8; 4] = read_at(r, pos)?;
        let last = h[0] & 0x80 != 0;
        if h[0] & 0x7F == 127 {
            return Err(C.malformed("invalid metadata block type"));
        }
        let len = u64::from(be_u24(&h, 1));
        pos = pos
            .checked_add(4 + len)
            .filter(|p| *p <= file_len)
            .ok_or_else(|| C.malformed("metadata block extends past end of file"))?;
        if last {
            break;
        }
    }
    let audio_start = pos;
    let end = strip_trailers(r, audio_start, file_len, C)?;
    if end <= audio_start {
        return Err(AudioHashError::NoAudio);
    }
    if end - audio_start < 2 {
        return Err(C.malformed("audio shorter than a frame header"));
    }
    let sync: [u8; 2] = read_at(r, audio_start)?;
    if sync[0] != 0xFF || sync[1] & 0xFE != 0xF8 {
        return Err(C.malformed("no frame sync after metadata"));
    }
    Ok(vec![audio_start..end])
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
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

    #[test]
    fn flac1_vorbis_comment_is_ignored() {
        let f = flac_frames(3);
        let a = hash_of(&flac(&[streaminfo(), vorbis(10)], &f));
        let b = hash_of(&flac(&[streaminfo(), vorbis(500)], &f));
        assert_eq!(a, b);
    }

    #[test]
    fn flac2_padding_is_ignored() {
        let f = flac_frames(3);
        let a = hash_of(&flac(&[streaminfo(), padding(0)], &f));
        let b = hash_of(&flac(&[streaminfo(), padding(8192)], &f));
        assert_eq!(a, b);
    }

    #[test]
    fn flac3_picture_is_ignored() {
        let f = flac_frames(3);
        let a = hash_of(&flac(&[streaminfo()], &f));
        let b = hash_of(&flac(&[streaminfo(), picture(3000)], &f));
        assert_eq!(a, b);
    }

    #[test]
    fn flac4_surrounding_id3_tags_are_ignored() {
        let f = flac_frames(3);
        let bare = flac(&[streaminfo()], &f);
        let mut with_head = id3v2(300, false);
        with_head.extend(&bare);
        let mut with_v1 = bare.clone();
        with_v1.extend(id3v1());
        assert_eq!(hash_of(&bare), hash_of(&with_head));
        assert_eq!(hash_of(&bare), hash_of(&with_v1));
    }

    #[test]
    fn flac5_flipped_frame_byte_changes_hash() {
        let mut f = flac_frames(3);
        let a = hash_of(&flac(&[streaminfo()], &f));
        f[20] ^= 1;
        let b = hash_of(&flac(&[streaminfo()], &f));
        assert_ne!(a, b);
    }

    #[test]
    fn flac6_malformed_inputs() {
        // block length past EOF
        let mut b = b"fLaC".to_vec();
        b.extend([0x04, 0xFF, 0xFF, 0xFF, 1, 2, 3]);
        assert!(malformed(&b));
        // truncated block header
        assert!(malformed(b"fLaC\x00\x00"));
        // no sync after metadata
        assert!(malformed(&flac(&[streaminfo()], &[0, 0, 0, 0])));
        // missing fLaC
        assert!(malformed(b"xxxxyyyyzzzz"));
        // reserved block type 127
        assert!(malformed(&flac(&[(127, vec![0; 4])], &flac_frames(1))));
    }

    #[test]
    fn one_byte_of_audio_is_malformed_not_io() {
        assert!(malformed(&flac(&[streaminfo()], &[0xFF])));
    }

    #[test]
    fn x4_layout_ranges_are_exact() {
        let frames = flac_frames(2);
        let mut b = id3v2(5, false);
        b.extend(flac(&[streaminfo(), vorbis(10)], &frames));
        b.extend(id3v1());
        let start = 15 + 4 + (4 + 34) + (4 + 10);
        let end = start + frames.len();
        assert_eq!(lay(&b).unwrap(), vec![start as u64..end as u64]);
    }
}
