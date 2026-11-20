//! PcmHash as the oracle for the generated AIFF files of `audio-fixtures`: what
//! the builder writes must be what PcmHash reads.

use std::io::Cursor;

use audio_fixtures::testkit::{comt_payload, id3, inst_payload, mark_payload, AiffBuilder, Codec};
use pretty_assertions::assert_eq;
use rstest::rstest;

use super::{pcm_hash_reader, PcmHashError, Unsupported};

fn hash(b: &AiffBuilder) -> music_primitives::PcmHash {
    pcm_hash_reader(Cursor::new(b.build())).expect("PcmHash reads it")
}

#[rstest]
#[case(8, 1)]
#[case(8, 2)]
#[case(16, 1)]
#[case(16, 2)]
#[case(24, 1)]
#[case(24, 2)]
#[case(32, 1)]
#[case(32, 2)]
fn integer_pcm_is_hashable(#[case] bits: u16, #[case] channels: u16) {
    hash(
        &AiffBuilder::new()
            .bits(bits)
            .channels(channels)
            .rate(48_000),
    );
}

#[test]
fn different_signals_hash_differently_and_equal_builds_hash_equally() {
    assert_eq!(hash(&AiffBuilder::new()), hash(&AiffBuilder::new()));
    assert!(hash(&AiffBuilder::new()) != hash(&AiffBuilder::new().noise()));
}

#[test]
fn twenty_bit_samples_hash_as_the_same_samples_at_24_bits() {
    let twenty = AiffBuilder::new().bits(20).noise().frames(2000);
    let shifted: Vec<i32> = twenty.samples().iter().map(|s| s << 4).collect();
    let twenty_four = AiffBuilder::new().bits(24).pcm(shifted);
    assert_eq!(hash(&twenty), hash(&twenty_four));
}

#[test]
fn supported_aifc_variants_hash_like_plain_aiff() {
    let plain = hash(&AiffBuilder::new());
    for codec in [Codec::None, Codec::Twos, Codec::Sowt] {
        assert_eq!(hash(&AiffBuilder::new().aifc(codec)), plain, "{codec:?}");
    }
}

#[test]
fn public_layout_describes_the_sample_bytes_exactly() {
    use super::{aiff_pcm_layout, ByteOrder};
    let b = AiffBuilder::new()
        .bits(20)
        .frames(1000)
        .aifc(Codec::Sowt)
        .id3(id3::full_tag(id3::Id3Version::V24));
    let bytes = b.build();
    let l = aiff_pcm_layout(Cursor::new(&bytes)).unwrap();
    assert_eq!(l.byte_order(), ByteOrder::Little);
    assert_eq!(
        (l.container_bytes(), l.significant_bits(), l.channels()),
        (3, 20, 2)
    );
    assert_eq!((l.sample_rate(), l.frames()), (44_100, 1000));
    // exactly the sample bytes, none of the trailing chunk
    assert_eq!(
        &bytes[l.data().start as usize..l.data().end as usize],
        b.sample_bytes()
    );
    let mut ints = Vec::new();
    l.decode_into(&b.sample_bytes(), &mut ints);
    let want: Vec<i32> = b.samples().iter().map(|s| s << 4).collect();
    assert_eq!(ints, want);
}

#[test]
fn lenient_layout_bounds_frames_by_what_is_there_while_hashes_stay_strict() {
    use super::{aiff_pcm_layout, aiff_pcm_layout_lenient};
    use crate::audio_hash_reader;
    let b = AiffBuilder::new().bits(24).frames(1000);
    let bytes = b.build();
    // 6 bytes per frame: cutting 100 bytes leaves 983 whole frames
    let cut = &bytes[..bytes.len() - 100];
    let l = aiff_pcm_layout_lenient(Cursor::new(cut)).unwrap();
    assert_eq!(
        (l.frames(), l.declared_frames(), l.is_exact()),
        (983, 1000, false)
    );
    assert_eq!(l.data().end - l.data().start, 983 * 6);
    assert!(l.data().end <= cut.len() as u64);
    // strict layout, PcmHash and AudioHash all refuse the cut file
    assert!(aiff_pcm_layout(Cursor::new(cut)).is_err());
    assert!(pcm_hash_reader(Cursor::new(cut)).is_err());
    assert!(audio_hash_reader(Cursor::new(cut)).is_err());
    // a whole file reads the same either way
    assert_eq!(
        aiff_pcm_layout_lenient(Cursor::new(&bytes)).unwrap(),
        aiff_pcm_layout(Cursor::new(&bytes)).unwrap()
    );
}

#[test]
fn public_layout_refuses_what_is_not_integer_pcm_aiff() {
    use super::aiff_pcm_layout;
    for codec in [Codec::Fl32, Codec::Ulaw] {
        let bytes = AiffBuilder::new().aifc(codec).build();
        assert!(matches!(
            aiff_pcm_layout(Cursor::new(bytes)),
            Err(PcmHashError::Unsupported(Unsupported::CompressedAiffC))
        ));
    }
    assert!(matches!(
        aiff_pcm_layout(Cursor::new(b"RIFF....WAVEfmt ".to_vec())),
        Err(PcmHashError::Unsupported(_) | PcmHashError::Container(_))
    ));
}

#[rstest]
#[case(Codec::Fl32)]
#[case(Codec::Ulaw)]
fn compressed_and_float_aifc_are_refused(#[case] codec: Codec) {
    let bytes = AiffBuilder::new().aifc(codec).build();
    assert!(matches!(
        pcm_hash_reader(Cursor::new(&bytes)),
        Err(PcmHashError::Unsupported(_))
    ));
}

#[test]
fn three_channels_are_refused() {
    let bytes = AiffBuilder::new().channels(3).build();
    assert!(matches!(
        pcm_hash_reader(Cursor::new(&bytes)),
        Err(PcmHashError::Unsupported(Unsupported::MoreThanTwoChannels))
    ));
}

#[rstest]
#[case(8, 1)]
#[case(16, 2)]
#[case(20, 2)]
#[case(24, 1)]
#[case(32, 2)]
fn aiff_info_reports_comm(#[case] bits: u16, #[case] channels: u16) {
    let bytes = AiffBuilder::new()
        .bits(bits)
        .channels(channels)
        .rate(48_000)
        .frames(1234)
        .build();
    let info = super::pcm_info_reader(Cursor::new(&bytes)).expect("info");
    assert_eq!(
        info,
        super::PcmInfo {
            container: crate::Container::Aiff,
            channels: channels as u8,
            sample_rate: 48_000,
            frames: 1234,
            bits_per_sample: bits as u8,
            encoding: super::SampleEncoding::Pcm {
                order: super::ByteOrder::Big
            },
        }
    );
}

#[test]
fn aiff_info_refuses_what_the_hash_refuses_with_typed_reasons() {
    let info = |b: &AiffBuilder| super::pcm_info_reader(Cursor::new(b.build()));
    assert!(matches!(
        info(&AiffBuilder::new().channels(3)),
        Err(PcmHashError::Unsupported(Unsupported::MoreThanTwoChannels))
    ));
    for codec in [Codec::Fl32, Codec::Ulaw] {
        assert!(matches!(
            info(&AiffBuilder::new().aifc(codec)),
            Err(PcmHashError::Unsupported(Unsupported::CompressedAiffC))
        ));
    }
    assert!(info(&AiffBuilder::new().bits(32).channels(1).rate(48_000)).is_ok());
}

#[test]
fn extra_chunks_and_the_id3_chunk_do_not_disturb_the_hash() {
    let plain = hash(&AiffBuilder::new().frames(500));
    let busy = AiffBuilder::new()
        .frames(500)
        .chunk(b"MARK", mark_payload())
        .chunk(b"INST", inst_payload())
        .chunk(b"COMT", comt_payload())
        .chunk(b"ANNO", b"odd")
        .id3(id3::full_tag(id3::Id3Version::V24));
    assert_eq!(hash(&busy), plain);
}
