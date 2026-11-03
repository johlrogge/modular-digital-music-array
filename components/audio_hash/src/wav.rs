//! WAV: ranges are the `fmt ` payload then the `data` payload; every other
//! chunk is ignored.

use std::io::{Read, Seek};
use std::ops::Range;

use crate::iff::{self, Endian, Unbounded};
use crate::tags::read_at;
use crate::{AudioHashError, Container};

const C: Container = Container::Wav;

/// Byte ranges of a WAV file's `fmt ` and `data` payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WavLayout {
    pub fmt: Range<u64>,
    pub data: Range<u64>,
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<WavLayout, AudioHashError> {
    if file_len < 12 {
        return Err(C.malformed("shorter than RIFF header"));
    }
    let head: [u8; 12] = read_at(r, 0)?;
    if &head[0..4] == b"RF64" {
        return Err(AudioHashError::UnsupportedContainer);
    }
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err(C.malformed("not a RIFF/WAVE file"));
    }

    // A `data` size of 0xFFFFFFFF means "runs to end of file" (streamed WAV).
    // A size of 0 is a literally empty payload.
    let data_chunk = Unbounded {
        id: *b"data",
        truncated: "data chunk truncated",
    };
    let [fmt, data] = iff::walk(
        r,
        file_len,
        C,
        Endian::Little,
        12,
        [b"fmt ", b"data"],
        Some(&data_chunk),
    )?;

    let fmt = fmt.ok_or_else(|| C.malformed("missing fmt chunk"))?;
    let data = data.ok_or_else(|| C.malformed("missing data chunk"))?;
    if data.is_empty() {
        return Err(AudioHashError::NoAudio);
    }
    Ok(WavLayout { fmt, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use pretty_assertions::assert_eq;

    fn lay(b: &[u8]) -> Result<WavLayout, AudioHashError> {
        layout(&mut std::io::Cursor::new(b.to_vec()), b.len() as u64)
    }

    fn pcm() -> Vec<u8> {
        (0..64u8).collect()
    }

    fn base() -> Vec<u8> {
        riff(&[fmt_chunk(1), chunk(b"data", &pcm())])
    }

    #[test]
    fn wav1_other_chunks_are_ignored() {
        let want = hash_of(&base());
        let list = chunk(b"LIST", b"INFOISFT\x05\0\0\0Lavf\0");
        let id3 = chunk(b"id3 ", &id3v2(30, false));
        let bext = chunk(b"bext", &[7u8; 602]);
        let data = chunk(b"data", &pcm());
        let cases = [
            riff(&[fmt_chunk(1), list.clone(), data.clone()]),
            riff(&[fmt_chunk(1), data.clone(), list]),
            riff(&[fmt_chunk(1), id3.clone(), data.clone()]),
            riff(&[fmt_chunk(1), data.clone(), id3]),
            riff(&[bext, fmt_chunk(1), data]),
        ];
        for (i, c) in cases.iter().enumerate() {
            assert_eq!(hash_of(c), want, "case {i}");
        }
    }

    #[test]
    fn wav2_fmt_contents_matter() {
        let a = riff(&[fmt_chunk(1), chunk(b"data", &pcm())]);
        let b = riff(&[fmt_chunk(2), chunk(b"data", &pcm())]);
        assert_ne!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn wav3_odd_sizes_and_unbounded_data() {
        // odd-sized chunks before the data are padded
        let odd = chunk(b"LIST", &[1, 2, 3, 4, 5]);
        assert_eq!(odd.len() % 2, 0);
        let with_odd = riff(&[fmt_chunk(1), odd, chunk(b"data", &pcm())]);
        assert_eq!(hash_of(&with_odd), hash_of(&base()));

        // odd-sized data keeps its exact length (pad excluded)
        let odd_data = riff(&[fmt_chunk(1), chunk(b"data", &[9, 9, 9])]);
        let r = lay(&odd_data).unwrap();
        assert_eq!(r.data.end - r.data.start, 3);

        // 0xFFFFFFFF means "to EOF"
        let unbounded = riff(&[fmt_chunk(1), chunk_sized(b"data", 0xFFFF_FFFF, &pcm())]);
        assert_eq!(hash_of(&base()), hash_of(&unbounded));
    }

    #[test]
    fn data_size_zero_is_empty_not_to_eof() {
        // A following LIST chunk must never be hashed as audio.
        let b = riff(&[
            fmt_chunk(1),
            chunk_sized(b"data", 0, &[]),
            chunk(b"LIST", b"INFOabcd"),
        ]);
        assert!(matches!(lay(&b), Err(AudioHashError::NoAudio)));
    }

    #[test]
    fn wav4_bad_inputs() {
        let no_data = riff(&[fmt_chunk(1), chunk(b"LIST", b"abcd")]);
        assert!(is_malformed(lay(&no_data), C));
        let no_fmt = riff(&[chunk(b"data", &pcm())]);
        assert!(is_malformed(lay(&no_fmt), C));
        let truncated = riff(&[fmt_chunk(1), chunk_sized(b"data", 1000, &pcm())]);
        assert!(is_malformed(lay(&truncated), C));
        let rf64 = b"RF64\xff\xff\xff\xffWAVEds64\x1c\0\0\0".to_vec();
        assert!(matches!(
            lay(&rf64),
            Err(AudioHashError::UnsupportedContainer)
        ));
        assert!(matches!(
            crate::audio_hash_reader(std::io::Cursor::new(&rf64)),
            Err(AudioHashError::UnsupportedContainer)
        ));
    }

    #[test]
    fn x4_layout_ranges_are_exact() {
        let b = riff(&[
            chunk(b"LIST", b"abcd"),
            fmt_chunk(1),
            chunk(b"data", &pcm()),
        ]);
        // 12 riff header, LIST 8+4, fmt 8+16, data 8+64
        let fmt_start = 12 + 12 + 8;
        let data_start = fmt_start + 16 + 8;
        assert_eq!(
            lay(&b).unwrap(),
            WavLayout {
                fmt: fmt_start as u64..(fmt_start + 16) as u64,
                data: data_start as u64..(data_start + 64) as u64,
            }
        );
    }
}
