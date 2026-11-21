//! Container identification from content (never from the file extension).

use std::io::{Read, Seek};

use audio_format::{AudioFormat, Sniffed};

use crate::{AudioHashError, Container};

/// The container of the file, by content. The sniffing itself is `audio_format::sniff`,
/// the one sniffer; this maps its answer to the hash's containers.
pub(crate) fn sniff<R: Read + Seek>(r: &mut R, file_len: u64) -> Result<Container, AudioHashError> {
    match audio_format::sniff(r, file_len)? {
        Sniffed::Format(AudioFormat::Flac) => Ok(Container::Flac),
        Sniffed::Format(AudioFormat::Mp3) => Ok(Container::Mp3),
        Sniffed::Format(AudioFormat::Wav) => Ok(Container::Wav),
        Sniffed::Format(AudioFormat::M4a) => Ok(Container::Mp4),
        Sniffed::Format(AudioFormat::Aiff) => Ok(Container::Aiff),
        Sniffed::Rf64 | Sniffed::Unknown => Err(AudioHashError::UnsupportedContainer),
    }
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
    fn aiff_and_aifc_are_recognised_at_offset_zero() {
        assert_eq!(s(b"FORM\0\0\0\x04AIFF").unwrap(), Container::Aiff);
        assert_eq!(s(b"FORM\0\0\0\x04AIFCCOMM").unwrap(), Container::Aiff);
        assert!(matches!(
            s(b"FORM\0\0\0\x04AIFX"),
            Err(AudioHashError::UnsupportedContainer)
        ));
    }

    #[test]
    fn aiff_after_head_id3v2_is_unsupported() {
        let mut b = id3v2(10, false);
        b.extend(b"FORM\0\0\0\x04AIFF");
        assert!(matches!(s(&b), Err(AudioHashError::UnsupportedContainer)));
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
