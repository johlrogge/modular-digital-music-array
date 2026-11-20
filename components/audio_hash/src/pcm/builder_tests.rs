//! PcmHash as the oracle for the generated AIFF files of `audio-fixtures`: what
//! the builder writes must be what PcmHash reads.

use std::io::Cursor;

use audio_fixtures::testkit::{comt_payload, id3, inst_payload, mark_payload, AiffBuilder, Codec};
use pretty_assertions::assert_eq;
use rstest::rstest;

use super::{pcm_hash_reader, PcmHashError};

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
        Err(PcmHashError::Unsupported("more than 2 channels"))
    ));
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
