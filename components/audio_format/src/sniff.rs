//! Format identification from content (never from the file extension). The one
//! sniffer: the audio hash and upload handling both call [`sniff`].

use std::io::{self, Read, Seek, SeekFrom};

use crate::AudioFormat;

/// What [`sniff`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    Format(AudioFormat),
    /// RF64, a WAV variant nobody supports.
    Rf64,
    Unknown,
}

/// Upper bound on zero padding skipped after the tag chain.
pub const MAX_ZERO_PADDING: u64 = 64 * 1024;

fn read_at<R: Read + Seek, const N: usize>(r: &mut R, pos: u64) -> io::Result<[u8; N]> {
    r.seek(SeekFrom::Start(pos))?;
    let mut buf = [0u8; N];
    r.read_exact(&mut buf)?;
    Ok(buf)
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
pub fn head_id3v2_len<R: Read + Seek>(r: &mut R, file_len: u64) -> io::Result<u64> {
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

/// MPEG audio frame sync: `0xFF` then a byte with the top three bits set.
pub fn is_frame_sync(b: [u8; 2]) -> bool {
    b[0] == 0xFF && b[1] & 0xE0 == 0xE0
}

/// Number of leading zero bytes at `start`, bounded by `MAX_ZERO_PADDING`.
pub fn zero_padding<R: Read + Seek>(r: &mut R, start: u64, file_len: u64) -> io::Result<u64> {
    let n = file_len.saturating_sub(start).min(MAX_ZERO_PADDING) as usize;
    let mut buf = vec![0u8; n];
    r.seek(SeekFrom::Start(start))?;
    r.read_exact(&mut buf)?;
    Ok(buf.iter().take_while(|b| **b == 0).count() as u64)
}

/// Identify the audio format of a file of `file_len` bytes by its content.
///
/// ID3v2 tags in front are skipped, then FLAC and MP3 are recognised. WAV (`RIFF`
/// with form type `WAVE`), AIFF (`FORM` with type `AIFF` or `AIFC`; an 8SVX or other
/// IFF is not audio we know), RF64 and MP4 (`ftyp`, taken as M4A) are only
/// recognised at offset 0, since their layouts read from the start of the file.
pub fn sniff<R: Read + Seek>(r: &mut R, file_len: u64) -> io::Result<Sniffed> {
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
        return Ok(Sniffed::Format(AudioFormat::Flac));
    }
    if id3 == 0 {
        if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
            return Ok(Sniffed::Format(AudioFormat::Wav));
        }
        if head.len() >= 12 && &head[0..4] == b"FORM" && matches!(&head[8..12], b"AIFF" | b"AIFC") {
            return Ok(Sniffed::Format(AudioFormat::Aiff));
        }
        if head.starts_with(b"RF64") {
            return Ok(Sniffed::Rf64);
        }
        if head.len() >= 8 && &head[4..8] == b"ftyp" {
            return Ok(Sniffed::Format(AudioFormat::M4a));
        }
    }
    // MP3 last: tolerate bounded zero padding after the tag chain. Every other
    // magic check has already run, so this cannot capture another format.
    let start = id3 + zero_padding(r, id3, file_len)?;
    if file_len.saturating_sub(start) >= 2 && is_frame_sync(read_at(r, start)?) {
        return Ok(Sniffed::Format(AudioFormat::Mp3));
    }
    Ok(Sniffed::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn s(b: &[u8]) -> Sniffed {
        sniff(&mut Cursor::new(b), b.len() as u64).unwrap()
    }

    fn id3v2(payload: usize) -> Vec<u8> {
        let mut v = b"ID3\x04\0\0".to_vec();
        v.extend([0, 0, (payload >> 7) as u8 & 0x7F, payload as u8 & 0x7F]);
        v.extend(vec![0u8; payload]);
        v
    }

    fn is(f: AudioFormat) -> Sniffed {
        Sniffed::Format(f)
    }

    #[test]
    fn identifies_each_format() {
        assert_eq!(s(b"fLaC\0\0\0\0"), is(AudioFormat::Flac));
        assert_eq!(s(&[0xFF, 0xFB, 0x90, 0x00]), is(AudioFormat::Mp3));
        assert_eq!(s(b"RIFF\x04\0\0\0WAVEfmt "), is(AudioFormat::Wav));
        assert_eq!(s(b"\0\0\0\x18ftypM4A \0\0\0\0"), is(AudioFormat::M4a));
        assert_eq!(s(b"FORM\0\0\0\x04AIFF"), is(AudioFormat::Aiff));
        assert_eq!(s(b"FORM\0\0\0\x04AIFCCOMM"), is(AudioFormat::Aiff));
    }

    #[test]
    fn looks_past_head_id3v2_for_flac_and_mp3() {
        let mut b = id3v2(40);
        b.extend(b"fLaC\0\0\0\0");
        assert_eq!(s(&b), is(AudioFormat::Flac));
        let mut b = id3v2(40);
        b.extend([0, 0, 0, 0xFF, 0xFB, 0x90, 0]);
        assert_eq!(s(&b), is(AudioFormat::Mp3));
    }

    #[test]
    fn wav_aiff_and_mp4_only_at_offset_zero() {
        for tail in [
            &b"FORM\0\0\0\x04AIFF"[..],
            b"RIFF\x04\0\0\0WAVEfmt ",
            b"\0\0\0\x18ftypM4A ",
        ] {
            let mut b = id3v2(10);
            b.extend(tail);
            assert_eq!(s(&b), Sniffed::Unknown);
        }
    }

    #[test]
    fn other_iff_and_riff_files_are_not_audio_we_know() {
        assert_eq!(s(b"FORM\0\0\0\x04AIFX"), Sniffed::Unknown);
        assert_eq!(s(b"FORM\0\0\0\x048SVXVHDR"), Sniffed::Unknown);
        assert_eq!(s(b"RIFF\x04\0\0\0AVI LIST"), Sniffed::Unknown);
        assert_eq!(s(b"RIFF\x04\0\0\0WEBPVP8 "), Sniffed::Unknown);
    }

    #[test]
    fn rf64_garbage_and_empty() {
        assert_eq!(s(b"RF64\0\0\0\0WAVEds64"), Sniffed::Rf64);
        assert_eq!(s(b""), Sniffed::Unknown);
        assert_eq!(s(b"hello world!"), Sniffed::Unknown);
        assert_eq!(s(&id3v2(10)), Sniffed::Unknown);
        assert_eq!(s(&[0xFF, 0x1F]), Sniffed::Unknown);
        // a bare "ID3" is no tag and no MP3
        assert_eq!(s(b"ID3\x03"), Sniffed::Unknown);
    }
}
