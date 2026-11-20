//! AIFF / AIFF-C: ranges of the first `COMM` and `SSND` chunk payloads. Every
//! other chunk is ignored.

use std::io::{Read, Seek};
use std::ops::Range;

use crate::iff::{self, Endian};
use crate::tags::{be_u32, read_at};
use crate::{AudioHashError, Container};

const C: Container = Container::Aiff;

/// Byte ranges of an AIFF file's `COMM` and `SSND` payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiffLayout {
    /// `FORM` type is `AIFC` (COMM carries a compression type).
    pub is_aifc: bool,
    pub comm: Range<u64>,
    pub ssnd: Range<u64>,
}

pub(crate) fn layout<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<AiffLayout, AudioHashError> {
    layout_with(r, file_len, false)
}

/// [`layout`]; with `clamp_truncated`, a chunk reaching past the end of the
/// file ends at the end of the file (playback of a truncated download).
pub(crate) fn layout_with<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
    clamp_truncated: bool,
) -> Result<AiffLayout, AudioHashError> {
    if file_len < 12 {
        return Err(C.malformed("shorter than FORM header"));
    }
    let head: [u8; 12] = read_at(r, 0)?;
    if &head[0..4] != b"FORM" || !matches!(&head[8..12], b"AIFF" | b"AIFC") {
        return Err(C.malformed("not a FORM/AIFF file"));
    }
    let [comm, ssnd] = iff::walk_with(
        r,
        file_len,
        C,
        Endian::Big,
        12,
        [b"COMM", b"SSND"],
        None,
        clamp_truncated,
    )?;
    Ok(AiffLayout {
        is_aifc: &head[8..12] == b"AIFC",
        comm: comm.ok_or_else(|| C.malformed("missing COMM chunk"))?,
        ssnd: ssnd.ok_or_else(|| C.malformed("missing SSND chunk"))?,
    })
}

/// AudioHash v1 ranges (ADR-010, 2026-11-20 note): the whole `COMM` payload
/// (it plays the role of WAV's `fmt `, and for AIFF-C carries the compression
/// type) then the SSND *sample bytes*. SSND's 8-byte `offset`/`blockSize`
/// header and the `offset` padding bytes are layout, not audio, and are left
/// out; so is every other chunk (`ID3 `, COMT, MARK, INST, APPL, ...).
pub(crate) fn hash_ranges<R: Read + Seek>(
    r: &mut R,
    file_len: u64,
) -> Result<Vec<Range<u64>>, AudioHashError> {
    let l = layout(r, file_len)?;
    if l.ssnd.end - l.ssnd.start < 8 {
        return Err(C.malformed("SSND chunk too short"));
    }
    let head: [u8; 8] = read_at(r, l.ssnd.start)?;
    let samples_start = l.ssnd.start + 8 + u64::from(be_u32(&head, 0));
    if samples_start > l.ssnd.end {
        return Err(C.malformed("SSND offset past end of chunk"));
    }
    if samples_start == l.ssnd.end {
        return Err(AudioHashError::NoAudio);
    }
    Ok(vec![l.comm, samples_start..l.ssnd.end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_hash_reader;
    use audio_fixtures::testkit::{
        comt_payload, id3, inst_payload, mark_payload, AiffBuilder, Codec,
    };
    use audio_fixtures::{aiff, be_chunk, comm, ssnd};
    use pretty_assertions::assert_eq;
    use std::io::Cursor;

    fn h(bytes: &[u8]) -> Result<music_primitives::AudioHash, AudioHashError> {
        audio_hash_reader(Cursor::new(bytes))
    }

    fn base() -> AiffBuilder {
        AiffBuilder::new().frames(2000).noise()
    }

    #[test]
    fn tags_and_extra_chunks_do_not_change_the_hash() {
        let want = h(&base().build()).unwrap();
        let variants = [
            base().id3(id3::full_tag(id3::Id3Version::V23)).build(),
            base().id3(id3::full_tag(id3::Id3Version::V24)).build(),
            base()
                .chunk(b"MARK", mark_payload())
                .chunk(b"INST", inst_payload())
                .chunk(b"COMT", comt_payload())
                .chunk(b"ANNO", b"odd")
                .chunk(b"APPL", b"stocABCD")
                .trailing_chunk(b"NAME", b"a title")
                .build(),
        ];
        for (i, v) in variants.iter().enumerate() {
            assert_eq!(h(v).unwrap(), want, "variant {i}");
        }
    }

    #[test]
    fn different_audio_or_format_hashes_differently() {
        let a = h(&base().build()).unwrap();
        assert_ne!(a, h(&AiffBuilder::new().frames(2000).build()).unwrap());
        assert_ne!(a, h(&base().bits(24).build()).unwrap());
        assert_ne!(a, h(&base().rate(48_000).build()).unwrap());
        let mut flipped = base().build();
        let at = flipped.len() - 100;
        flipped[at] ^= 1; // inside SSND: it is the last chunk
        assert_ne!(a, h(&flipped).unwrap());
    }

    #[test]
    fn aifc_variants_hash_including_compressed_and_float() {
        // AudioHash is a hash of encoded bytes, not of PCM: it identifies any
        // AIFF-C payload, which is what lets encoder-refused files be stored.
        let mut seen = Vec::new();
        for codec in [
            Codec::None,
            Codec::Twos,
            Codec::Sowt,
            Codec::Fl32,
            Codec::Ulaw,
        ] {
            let b = base().aifc(codec);
            let x = h(&b.build()).unwrap();
            let tagged = h(&b.clone().id3(id3::full_tag(id3::Id3Version::V24)).build()).unwrap();
            assert_eq!(x, tagged, "{codec:?}");
            assert!(!seen.contains(&x), "{codec:?} collides");
            seen.push(x);
        }
        // AIFF vs AIFF-C NONE of the same samples differ (COMM differs).
        assert_ne!(h(&base().build()).unwrap(), seen[0]);
    }

    #[test]
    fn ssnd_offset_header_is_not_audio() {
        let samples: Vec<u8> = (0..200u8).collect();
        // the fixture writes `offset` filler bytes before the samples
        let mk = |offset: u32, block: u32| {
            let mut s = ssnd(offset, &samples);
            s[12..16].copy_from_slice(&block.to_be_bytes());
            aiff(b"AIFF", &[comm(1, 100, 16, 44_100), s])
        };
        assert_eq!(
            h(&mk(0, 0)).unwrap(),
            h(&mk(0, 4096)).unwrap(),
            "blockSize is ignored"
        );
        assert_eq!(
            h(&mk(0, 0)).unwrap(),
            h(&mk(3, 0)).unwrap(),
            "offset padding is ignored"
        );
    }

    #[test]
    fn malformed_and_truncated_files_error_without_panic() {
        let good = base().id3(id3::full_tag(id3::Id3Version::V24)).build();
        for cut in (0..good.len()).step_by(97).chain([good.len() - 1]) {
            let _ = h(&good[..cut]); // must not panic
        }
        assert!(h(&good[..good.len() / 2]).is_err());
        assert!(h(b"FORM\0\0\0\x04AIFF").is_err());
        // no COMM
        let no_comm = aiff(b"AIFF", &[ssnd(0, &[1, 2, 3, 4])]);
        assert!(matches!(h(&no_comm), Err(AudioHashError::Malformed { .. })));
        // no SSND
        let no_ssnd = aiff(b"AIFF", &[comm(1, 1, 16, 44_100)]);
        assert!(matches!(h(&no_ssnd), Err(AudioHashError::Malformed { .. })));
        // SSND shorter than its header, offset past the end, no samples
        let short = aiff(
            b"AIFF",
            &[comm(1, 1, 16, 44_100), be_chunk(b"SSND", &[0; 4])],
        );
        assert!(matches!(h(&short), Err(AudioHashError::Malformed { .. })));
        let mut far_ssnd = ssnd(0, &[1, 2]);
        far_ssnd[8..12].copy_from_slice(&50u32.to_be_bytes()); // offset beyond the chunk
        let far = aiff(b"AIFF", &[comm(1, 1, 16, 44_100), far_ssnd]);
        assert!(matches!(h(&far), Err(AudioHashError::Malformed { .. })));
        let empty = aiff(b"AIFF", &[comm(1, 0, 16, 44_100), ssnd(0, &[])]);
        assert!(matches!(h(&empty), Err(AudioHashError::NoAudio)));
    }

    #[test]
    fn ranges_are_comm_then_ssnd_whatever_their_order_in_the_file() {
        let comm_chunk = comm(2, 100, 16, 44_100);
        let samples: Vec<u8> = (0..400).map(|i| (i % 251) as u8).collect();
        let ssnd_chunk = ssnd(0, &samples);
        let comm_first = aiff(b"AIFF", &[comm_chunk.clone(), ssnd_chunk.clone()]);
        let ssnd_first = aiff(b"AIFF", &[ssnd_chunk, be_chunk(b"ANNO", b"x"), comm_chunk]);
        assert_eq!(h(&comm_first).unwrap(), h(&ssnd_first).unwrap());
    }

    #[test]
    fn stream_info_is_declared_by_comm_under_any_compression() {
        use crate::{
            pcm_info_reader, stream_info_reader, ByteOrder, PcmHashError, SampleEncoding,
            Unsupported,
        };
        for codec in [Codec::Fl32, Codec::Ulaw, Codec::Sowt] {
            let bytes = base().aifc(codec).chunk(b"COMT", comt_payload()).build();
            let info = stream_info_reader(Cursor::new(&bytes)).unwrap();
            assert_eq!(
                (info.channels, info.sample_rate, info.frames),
                (2, 44_100, 2000)
            );
            let want_bits = if codec == Codec::Fl32 { 32 } else { 16 };
            assert_eq!(info.bits_per_sample, want_bits, "{codec:?}");
            let fourcc = match codec {
                Codec::Fl32 => *b"fl32",
                Codec::Ulaw => *b"ulaw",
                _ => *b"sowt",
            };
            assert_eq!(info.encoding, SampleEncoding::Other(fourcc));
        }
        // a plain AIFF is plain big-endian PCM; so is what pcm_info says
        let plain = base().build();
        let want = SampleEncoding::Pcm {
            order: ByteOrder::Big,
        };
        assert_eq!(
            stream_info_reader(Cursor::new(&plain)).unwrap().encoding,
            want
        );
        assert_eq!(pcm_info_reader(Cursor::new(&plain)).unwrap().encoding, want);
        // an AIFF-C is not plain, even NONE
        let none = base().aifc(Codec::None).build();
        assert_eq!(
            pcm_info_reader(Cursor::new(&none)).unwrap().encoding,
            SampleEncoding::Other(*b"NONE")
        );
        // PcmHash's own check is stricter
        let float = base().aifc(Codec::Fl32).build();
        assert!(matches!(
            pcm_info_reader(Cursor::new(&float)),
            Err(PcmHashError::Unsupported(Unsupported::CompressedAiffC))
        ));
        // and corrupt files are errors
        assert!(stream_info_reader(Cursor::new(b"FORM\0\0\0\x04AIFF")).is_err());
        let short = aiff(b"AIFF", &[be_chunk(b"COMM", &[0; 10]), ssnd(0, &[1, 2])]);
        assert!(stream_info_reader(Cursor::new(&short)).is_err());
    }
}
