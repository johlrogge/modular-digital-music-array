//! AudioHash v1 (ADR-010): a container-aware byte-range hash of the encoded
//! audio payload, independent of tags and of any decoder or demuxer.
//!
//! `sha256( b"mdma-audiohash\0v1\0" || container_tag[4]
//!          || for each range: u64_le(len) || bytes )`

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

use music_primitives::AudioHash;
use sha2::{Digest, Sha256};

mod flac;
mod mp3;
mod mp4;
mod sniff;
mod tags;
mod wav;

#[cfg(test)]
mod fixtures;

const DOMAIN: &[u8] = b"mdma-audiohash\0v1\0";
const BUF_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Flac,
    Mp3,
    Wav,
    Mp4,
    /// Recognised by sniffing only; AudioHash v1 does not support it.
    Aiff,
}

impl Container {
    fn tag(self) -> [u8; 4] {
        match self {
            Container::Aiff => *b"aif\0",
            Container::Flac => *b"flac",
            Container::Mp3 => *b"mp3\0",
            Container::Wav => *b"wav\0",
            Container::Mp4 => *b"mp4\0",
        }
    }

    fn malformed(self, reason: &'static str) -> AudioHashError {
        AudioHashError::Malformed {
            container: self,
            reason,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioHashError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unrecognised or unsupported container")]
    UnsupportedContainer,
    #[error("malformed {container:?}: {reason}")]
    Malformed {
        container: Container,
        reason: &'static str,
    },
    #[error("no audio payload")]
    NoAudio,
}

/// Hash the audio payload of the file at `path`.
pub fn audio_hash(path: &Path) -> Result<AudioHash, AudioHashError> {
    audio_hash_reader(File::open(path)?)
}

/// Hash the audio payload of a seekable byte source.
pub fn audio_hash_reader<R: Read + Seek>(mut r: R) -> Result<AudioHash, AudioHashError> {
    let file_len = r.seek(SeekFrom::End(0))?;
    let container = sniff::sniff(&mut r, file_len)?;
    let ranges = match container {
        Container::Flac => vec![flac::layout(&mut r, file_len)?.audio],
        Container::Mp3 => mp3::layout(&mut r, file_len)?,
        Container::Wav => {
            let l = wav::layout(&mut r, file_len)?;
            vec![l.fmt, l.data]
        }
        Container::Mp4 => mp4::layout(&mut r, file_len)?,
        Container::Aiff => return Err(AudioHashError::UnsupportedContainer),
    };
    hash_ranges(&mut r, container, &ranges)
}

fn hash_ranges<R: Read + Seek>(
    r: &mut R,
    container: Container,
    ranges: &[Range<u64>],
) -> Result<AudioHash, AudioHashError> {
    let total: u64 = ranges
        .iter()
        .map(|rg| rg.end.saturating_sub(rg.start))
        .fold(0, u64::saturating_add);
    if total == 0 {
        return Err(AudioHashError::NoAudio);
    }
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(container.tag());
    let mut buf = vec![0u8; BUF_SIZE];
    for rg in ranges {
        let len = rg.end.saturating_sub(rg.start);
        hasher.update(len.to_le_bytes());
        r.seek(SeekFrom::Start(rg.start))?;
        let mut remaining = len;
        while remaining > 0 {
            let n = remaining.min(BUF_SIZE as u64) as usize;
            r.read_exact(&mut buf[..n])?;
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
    }
    Ok(AudioHash::from_sha256_v1(hasher.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;
    use std::io::Cursor;

    // TODO(R-1): retag real files (e.g. with ffmpeg/mutagen) and assert the
    // AudioHash is unchanged. Needs checked-in real-file fixtures, which are
    // out of scope for this branch.

    fn payload() -> Vec<u8> {
        flac_frames(3)
    }

    #[test]
    fn x1_container_tag_separates_formats() {
        let as_flac = hash_of(&flac(&[streaminfo()], &payload()));
        let as_wav = hash_of(&riff(&[fmt_chunk(1), chunk(b"data", &payload())]));
        assert_ne!(as_flac, as_wav);
    }

    #[test]
    fn x2_zero_length_audio_is_no_audio() {
        let cases: Vec<Vec<u8>> = vec![
            flac(&[streaminfo()], &[]),
            riff(&[fmt_chunk(1), chunk(b"data", &[])]),
            mp4(&[ftyp(), mp4_box(b"mdat", &[])]),
            mp4(&[ftyp()]),
        ];
        for bytes in cases {
            assert!(
                matches!(
                    audio_hash_reader(Cursor::new(&bytes)),
                    Err(AudioHashError::NoAudio)
                ),
                "{bytes:?}"
            );
        }
        let id3_only = id3v2(20, false);
        assert!(matches!(
            mp3::layout(&mut Cursor::new(&id3_only), id3_only.len() as u64),
            Err(AudioHashError::NoAudio)
        ));
    }

    #[test]
    fn extension_is_irrelevant_and_path_api_works() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = flac(&[streaminfo(), vorbis(10)], &payload());
        let a = dir.path().join("a.mp3");
        let b = dir.path().join("b.wav");
        std::fs::write(&a, &bytes).unwrap();
        std::fs::write(&b, &bytes).unwrap();
        assert_eq!(audio_hash(&a).unwrap(), hash_of(&bytes));
        assert_eq!(audio_hash(&b).unwrap(), hash_of(&bytes));
        assert!(matches!(
            audio_hash(&dir.path().join("missing")),
            Err(AudioHashError::Io(_))
        ));
    }

    #[test]
    fn padded_mp3_is_recognised_through_public_api() {
        let frames = mp3_frames(4);
        let mut padded = id3v2(50, false);
        padded.extend(vec![0u8; 700]);
        padded.extend(&frames);
        assert_eq!(hash_of(&padded), hash_of(&frames));
        // Padding alone, or padding before non-MP3 bytes, is not audio.
        let mut junk = vec![0u8; 700];
        junk.extend(b"fLaC");
        assert!(matches!(
            audio_hash_reader(Cursor::new(junk)),
            Err(AudioHashError::UnsupportedContainer)
        ));
    }

    #[test]
    fn aiff_is_unsupported_through_public_api() {
        let mut aiff = b"FORM\0\0\0\x04AIFF".to_vec();
        aiff.extend([0u8; 64]);
        assert!(matches!(
            audio_hash_reader(Cursor::new(aiff)),
            Err(AudioHashError::UnsupportedContainer)
        ));
    }

    #[test]
    fn large_ranges_stream_through_buffer() {
        let frames: Vec<u8> = [0xFF, 0xF8]
            .into_iter()
            .chain((0..200_000u32).map(|i| (i % 251) as u8))
            .collect();
        let a = hash_of(&flac(&[streaminfo()], &frames));
        let b = hash_of(&flac(&[streaminfo(), padding(100)], &frames));
        assert_eq!(a, b);
    }

    // G-1: golden hashes. They freeze algorithm v1 (ADR-010). Changing any of
    // them means changing the algorithm, which requires a v2 prefix and an ADR
    // update, not an edit of these strings.
    const GOLDEN_FLAC: &str =
        "audio-sha256-v1:5bea0244e801fe98b4f845b98ef25f5846eefaba1619da0c16f5260c8ec7d75c";
    const GOLDEN_MP3: &str =
        "audio-sha256-v1:45ed07547ac44ba67e233a0537b5ebfca7e54a134cf3a9ce51f2a908227e74d2";
    const GOLDEN_WAV: &str =
        "audio-sha256-v1:59168282d97863fe19a2798e1b34feb99f88f6e93b63edfcdb85311dc2b9a70f";
    const GOLDEN_MP4: &str =
        "audio-sha256-v1:74c2aa21653be59c02bd6f2b3a88e2d8e6a64d9af6af28b12d8c5bfbf7d5e20a";

    // Golden inputs are spelled out byte by byte on purpose: they must not
    // depend on `fixtures`, whose builders may change.
    fn golden_flac() -> Vec<u8> {
        let mut v = b"fLaC".to_vec();
        v.extend([0x00, 0, 0, 34]); // STREAMINFO, not last
        v.extend([0x11; 34]);
        v.extend([0x84, 0, 0, 12]); // VORBIS_COMMENT, last
        v.extend([b'v'; 12]);
        for fill in 1..=4u8 {
            v.extend([0xFF, 0xF8]);
            v.extend([fill; 30]);
        }
        v
    }

    fn golden_mp3() -> Vec<u8> {
        let mut v = Vec::new();
        for fill in [1u8, 1, 3, 3, 5] {
            v.extend([0xFF, 0xFB, 0x90, 0x04]);
            v.extend([fill; 96]);
        }
        v
    }

    fn golden_wav() -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend(292u32.to_le_bytes());
        v.extend(b"WAVE");
        v.extend(b"fmt ");
        v.extend(16u32.to_le_bytes());
        v.extend([1u8; 16]);
        v.extend(b"data");
        v.extend(256u32.to_le_bytes());
        v.extend(0..=255u8);
        v
    }

    fn golden_mp4() -> Vec<u8> {
        let mut v = 24u32.to_be_bytes().to_vec();
        v.extend(b"ftypM4A \0\0\0\0M4A isom");
        v.extend(24u32.to_be_bytes());
        v.extend(b"mdat");
        v.extend(b"mdat-payload-one");
        v.extend(11u32.to_be_bytes());
        v.extend(b"mdat");
        v.extend(b"two");
        v
    }

    #[test]
    fn g1_golden_hashes() {
        assert_eq!(hash_of(&golden_flac()).as_str(), GOLDEN_FLAC);
        assert_eq!(hash_of(&golden_mp3()).as_str(), GOLDEN_MP3);
        assert_eq!(hash_of(&golden_wav()).as_str(), GOLDEN_WAV);
        assert_eq!(hash_of(&golden_mp4()).as_str(), GOLDEN_MP4);
    }

    // Hand-built ADR-010 preimage:
    //   sha256( "mdma-audiohash\0v1\0" || tag[4] || for each range: u64_le(len) || bytes )
    fn spec_hash(tag: &[u8; 4], ranges: &[&[u8]]) -> String {
        let mut h = Sha256::new();
        h.update(b"mdma-audiohash\0v1\0");
        h.update(tag);
        for r in ranges {
            h.update((r.len() as u64).to_le_bytes());
            h.update(r);
        }
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        format!("audio-sha256-v1:{hex}")
    }

    #[test]
    fn preimage_matches_adr_for_wav() {
        let data: Vec<u8> = (0..=255u8).collect();
        let want = spec_hash(b"wav\0", &[&[1u8; 16], &data]);
        assert_eq!(hash_of(&golden_wav()).as_str(), want);
        assert_eq!(want, GOLDEN_WAV);
    }

    #[test]
    fn preimage_matches_adr_for_mp4_with_two_mdat_ranges() {
        let want = spec_hash(b"mp4\0", &[b"mdat-payload-one", b"two"]);
        assert_eq!(hash_of(&golden_mp4()).as_str(), want);
        assert_eq!(want, GOLDEN_MP4);
    }

    #[test]
    fn preimage_matches_adr_for_flac_and_mp3() {
        let flac_b = golden_flac();
        // "fLaC" + STREAMINFO block (4 + 34) + last VORBIS block (4 + 12)
        let frames = &flac_b[4 + 38 + 16..];
        assert_eq!(frames.len(), 4 * 32);
        assert_eq!(hash_of(&flac_b).as_str(), spec_hash(b"flac", &[frames]));
        let mp3_b = golden_mp3();
        assert_eq!(hash_of(&mp3_b).as_str(), spec_hash(b"mp3\0", &[&mp3_b]));
    }

    #[test]
    fn id3v1_extended_is_stripped_through_public_api() {
        let frames = mp3_frames(4);
        let mut tagged = frames.clone();
        tagged.extend(id3v1_extended());
        tagged.extend(id3v1());
        assert_eq!(hash_of(&tagged), hash_of(&frames));
        let mut f = flac(&[streaminfo()], &flac_frames(3));
        let bare = hash_of(&f);
        f.extend(id3v1_extended());
        f.extend(id3v1());
        assert_eq!(hash_of(&f), bare);
    }

    // Base inputs for mutation fuzzing and invariance properties.
    fn valid_inputs() -> Vec<Vec<u8>> {
        let mut flac_tagged = id3v2(40, false);
        flac_tagged.extend(flac(
            &[streaminfo(), vorbis(30), picture(100)],
            &flac_frames(3),
        ));
        flac_tagged.extend(id3v1());
        let mut mp3_tagged = id3v2(40, true);
        mp3_tagged.extend(mp3_frames(4));
        mp3_tagged.extend(ape(120, true));
        mp3_tagged.extend(id3v1());
        vec![
            flac(&[streaminfo()], &flac_frames(2)),
            flac_tagged,
            mp3_frames(3),
            mp3_tagged,
            riff(&[fmt_chunk(1), chunk(b"data", &[1, 2, 3, 4, 5])]),
            riff(&[
                chunk(b"LIST", b"INFOabc"),
                fmt_chunk(1),
                chunk(b"data", &[9; 33]),
                chunk(b"id3 ", &[1; 11]),
            ]),
            mp4(&[
                ftyp(),
                mp4_box(b"mdat", b"abcdef"),
                mp4_box(b"moov", b"xyz"),
            ]),
            mp4(&[
                ftyp(),
                mp4_box_large(b"mdat", b"abc"),
                mp4_box_to_eof(b"mdat", b"de"),
            ]),
        ]
    }

    fn trailer(kind: usize, n: usize) -> Vec<u8> {
        match kind {
            0 => id3v1(),
            1 => ape(n, n.is_multiple_of(2)),
            2 => lyrics3v2(n),
            _ => id3v2(n, true),
        }
    }

    fn magic(i: usize) -> Vec<u8> {
        match i {
            0 => vec![],
            1 => b"fLaC".to_vec(),
            2 => b"RIFF\0\0\0\0WAVE".to_vec(),
            3 => b"\0\0\0\x18ftypM4A ".to_vec(),
            4 => vec![0xFF, 0xFB],
            _ => b"ID3\x04\0\0\0\0\0\x05".to_vec(),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // Mutate valid files (4- and 8-byte windows, truncation): never panic.
        #[test]
        fn mutated_valid_files_never_panic(
            base in 0usize..8,
            edits in proptest::collection::vec(
                (any::<usize>(), any::<[u8; 8]>(), any::<bool>()),
                0..4,
            ),
            trunc in proptest::option::of(any::<usize>()),
        ) {
            let mut bytes = valid_inputs().swap_remove(base);
            for (at, window, wide) in edits {
                let len = if wide { 8 } else { 4 };
                if bytes.len() >= len {
                    let at = at % (bytes.len() - len + 1);
                    bytes[at..at + len].copy_from_slice(&window[..len]);
                }
            }
            if let Some(t) = trunc {
                let t = t % (bytes.len() + 1);
                bytes.truncate(t);
            }
            let _ = audio_hash_reader(Cursor::new(bytes));
        }

        // Any sequence of trailing tags leaves the hash unchanged.
        #[test]
        fn trailers_never_change_the_hash(
            use_flac in any::<bool>(),
            frames in 1usize..6,
            trailers in proptest::collection::vec((0usize..4, 0usize..400), 0..4),
        ) {
            let audio = if use_flac {
                flac(&[streaminfo(), vorbis(9)], &flac_frames(frames))
            } else {
                mp3_frames(frames)
            };
            let mut tagged = audio.clone();
            for (kind, n) in trailers {
                tagged.extend(trailer(kind, n));
            }
            prop_assert_eq!(hash_of(&tagged), hash_of(&audio));
        }

        #[test]
        fn x3_never_panics(
            data in proptest::collection::vec(any::<u8>(), 0..65536),
            which in 0usize..6,
        ) {
            let mut bytes = magic(which);
            bytes.extend(data);
            let _ = audio_hash_reader(Cursor::new(bytes));
        }
    }
}
