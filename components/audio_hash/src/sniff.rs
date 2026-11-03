//! Container identification from content (never from the file extension).

use std::io::{Read, Seek, SeekFrom};

use crate::mp3::{is_frame_sync, zero_padding};
use crate::tags::{head_id3v2_len, read_at};
use crate::{AudioHashError, Container};

pub(crate) fn sniff<R: Read + Seek>(r: &mut R, file_len: u64) -> Result<Container, AudioHashError> {
    let id3 = head_id3v2_len(r, file_len)?;
    let avail = file_len - id3;
    let mut head = [0u8; 12];
    let n = avail.min(12) as usize;
    if n > 0 {
        r.seek(SeekFrom::Start(id3))?;
        r.read_exact(&mut head[..n])?;
    }
    let head = &head[..n];

    if head.starts_with(b"fLaC") {
        return Ok(Container::Flac);
    }
    // WAV, RF64 and MP4 are only recognised at offset 0: their layouts read
    // from the start of the file, not after an ID3 chain.
    if id3 == 0 {
        if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
            return Ok(Container::Wav);
        }
        if head.starts_with(b"RF64") {
            return Err(AudioHashError::UnsupportedContainer);
        }
        if head.len() >= 8 && &head[4..8] == b"ftyp" {
            return Ok(Container::Mp4);
        }
    }
    // MP3 last: tolerate bounded zero padding after the tag chain. Every other
    // magic check has already run, so this cannot capture another format.
    let start = id3 + zero_padding(r, id3, file_len)?;
    if file_len.saturating_sub(start) >= 2 && is_frame_sync(read_at(r, start)?) {
        return Ok(Container::Mp3);
    }
    Err(AudioHashError::UnsupportedContainer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use std::io::Cursor;

    fn s(b: &[u8]) -> Result<Container, AudioHashError> {
        sniff(&mut Cursor::new(b), b.len() as u64)
    }

    #[test]
    fn identifies_each_container() {
        assert_eq!(
            s(&flac(&[streaminfo()], &flac_frames(1))).unwrap(),
            Container::Flac
        );
        assert_eq!(s(&mp3_frames(2)).unwrap(), Container::Mp3);
        assert_eq!(
            s(&riff(&[fmt_chunk(1), chunk(b"data", b"abcd")])).unwrap(),
            Container::Wav
        );
        assert_eq!(s(&mp4(&[ftyp()])).unwrap(), Container::Mp4);
    }

    #[test]
    fn looks_past_head_id3v2() {
        let mut b = id3v2(40, false);
        b.extend(flac(&[streaminfo()], &flac_frames(1)));
        assert_eq!(s(&b).unwrap(), Container::Flac);
        let mut b = id3v2(40, true);
        b.extend(mp3_frames(1));
        assert_eq!(s(&b).unwrap(), Container::Mp3);
    }

    #[test]
    fn mp4_only_without_id3() {
        let mut b = id3v2(10, false);
        b.extend(mp4(&[ftyp()]));
        assert!(matches!(s(&b), Err(AudioHashError::UnsupportedContainer)));
    }

    #[test]
    fn riff_after_head_id3v2_is_unsupported() {
        let mut b = id3v2(10, false);
        b.extend(riff(&[fmt_chunk(1), chunk(b"data", b"abcd")]));
        assert!(matches!(s(&b), Err(AudioHashError::UnsupportedContainer)));
        let mut b = id3v2(10, false);
        b.extend(b"RF64\0\0\0\0WAVEds64");
        assert!(matches!(s(&b), Err(AudioHashError::UnsupportedContainer)));
    }

    #[test]
    fn rejects_rf64_garbage_and_empty() {
        let unsupported = |b: &[u8]| matches!(s(b), Err(AudioHashError::UnsupportedContainer));
        assert!(unsupported(b"RF64\0\0\0\0WAVEds64"));
        assert!(unsupported(b""));
        assert!(unsupported(b"hello world!"));
        assert!(unsupported(&id3v2(10, false)));
        assert!(unsupported(&[0xFF, 0x1F]));
    }
}
