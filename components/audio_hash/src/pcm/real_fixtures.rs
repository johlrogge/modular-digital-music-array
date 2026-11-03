//! PcmHash against real libFLAC / SoX output (`testdata/pcm`, see its README).
//!
//! The fixtures are embedded at compile time, so a missing file fails the
//! build rather than a test run.

use std::collections::BTreeMap;
use std::io::Cursor;

use pretty_assertions::assert_eq;
use proptest::prelude::*;
use sha2::{Digest, Sha256};

use crate::fixtures::{crc16, crc8, flac, flac_streaminfo};
use crate::{audio_hash_reader, pcm_hash_reader, PcmHashError};

macro_rules! fx {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/pcm/fixtures/",
            $name
        ))
    };
}

type Fixture = (&'static str, &'static [u8]);

fn pcm(bytes: &[u8]) -> Result<String, PcmHashError> {
    pcm_hash_reader(Cursor::new(bytes)).map(|h| h.as_str().to_owned())
}

fn ok(name: &str, bytes: &[u8]) -> String {
    pcm(bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"))
}

const S16: &[Fixture] = &[
    ("s16.wav", fx!("s16.wav")),
    ("s16_w24.wav", fx!("s16_w24.wav")),
    ("s16_w32.wav", fx!("s16_w32.wav")),
    ("s16.aiff", fx!("s16.aiff")),
    ("s16_none.aifc", fx!("s16_none.aifc")),
    ("s16_l0.flac", fx!("s16_l0.flac")),
    ("s16_l5.flac", fx!("s16_l5.flac")),
    ("s16_l8.flac", fx!("s16_l8.flac")),
    ("s16_l8_bs192.flac", fx!("s16_l8_bs192.flac")),
    ("s16_l8_bs4096.flac", fx!("s16_l8_bs4096.flac")),
    ("s16_l8_ms.flac", fx!("s16_l8_ms.flac")),
    ("s16_l8_noms.flac", fx!("s16_l8_noms.flac")),
    ("s16_l8_md5zero.flac", fx!("s16_l8_md5zero.flac")),
    ("s16_l8_tags.flac", fx!("s16_l8_tags.flac")),
    ("s16_l8_picture.flac", fx!("s16_l8_picture.flac")),
    ("s16_l8_pad4096.flac", fx!("s16_l8_pad4096.flac")),
    ("s16_l8_id3v2.flac", fx!("s16_l8_id3v2.flac")),
    ("s16_l8_id3v1.flac", fx!("s16_l8_id3v1.flac")),
    ("s16_l8_id3both.flac", fx!("s16_l8_id3both.flac")),
];

const M16: &[Fixture] = &[
    ("m16.wav", fx!("m16.wav")),
    ("m16.aiff", fx!("m16.aiff")),
    ("m16_l0.flac", fx!("m16_l0.flac")),
    ("m16_l8.flac", fx!("m16_l8.flac")),
];

const S24: &[Fixture] = &[
    ("s24.wav", fx!("s24.wav")),
    ("s24.aiff", fx!("s24.aiff")),
    ("s24_l0.flac", fx!("s24_l0.flac")),
    ("s24_l8.flac", fx!("s24_l8.flac")),
];

const S8: &[Fixture] = &[
    ("s8.wav", fx!("s8.wav")),
    ("s8.aiff", fx!("s8.aiff")),
    ("s8_l8.flac", fx!("s8_l8.flac")),
];

const GROUPS: &[(&str, &[Fixture])] = &[("s16", S16), ("m16", M16), ("s24", S24), ("s8", S8)];

#[test]
fn every_variant_of_a_master_has_the_same_pcm_hash() {
    for (group, files) in GROUPS {
        let (first_name, first) = files[0];
        let want = ok(first_name, first);
        for (name, bytes) in *files {
            assert_eq!(ok(name, bytes), want, "{group}: {name} vs {first_name}");
        }
    }
}

#[test]
fn different_masters_have_different_pcm_hashes() {
    let hashes: BTreeMap<&str, String> = GROUPS
        .iter()
        .map(|(g, files)| (*g, ok(files[0].0, files[0].1)))
        .collect();
    let distinct: std::collections::BTreeSet<_> = hashes.values().collect();
    assert_eq!(distinct.len(), hashes.len(), "{hashes:?}");
}

#[test]
fn audio_hash_v1_still_tells_flac_encodes_apart() {
    let v1 = |name: &str, bytes: &[u8]| {
        audio_hash_reader(Cursor::new(bytes))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
            .as_str()
            .to_owned()
    };
    let l0 = v1("s16_l0", fx!("s16_l0.flac"));
    let l5 = v1("s16_l5", fx!("s16_l5.flac"));
    let l8 = v1("s16_l8", fx!("s16_l8.flac"));
    assert_ne!(l0, l8);
    assert_ne!(l5, l8);
    assert_ne!(l0, l5);
    assert_ne!(v1("s16.wav", fx!("s16.wav")), l8);
}

#[test]
fn trailing_junk_after_the_last_frame_is_a_decode_error() {
    let r = pcm_hash_reader(Cursor::new(fx!("s16_l8_junk.flac")));
    assert!(matches!(r, Err(PcmHashError::Decode(_))), "{r:?}");
}

#[test]
fn thirty_two_bit_flac_is_refused() {
    // Documented false negative (ADR-012). claxon reports it as a reserved
    // frame header value, so it surfaces as Decode, not Unsupported.
    let r = pcm_hash_reader(Cursor::new(fx!("s32_l8.flac")));
    assert!(
        matches!(
            r,
            Err(PcmHashError::Decode(
                "invalid frame header, encountered reserved value"
            ))
        ),
        "{r:?}"
    );
}

#[test]
fn eighteen_bit_flac_without_a_frame_bps_code_is_refused() {
    // libFLAC will not encode 18-bit here, so build it: mono, STREAMINFO bps
    // 18, one frame whose header carries bps code 000 ("see STREAMINFO"),
    // with a CONSTANT subframe of four zero samples (8 + 18 bits, padded).
    let mut f = vec![0xFF, 0xF8, 0x69, 0x00, 0x00, 0x03];
    f.push(crc8(&f));
    f.extend([0u8; 4]);
    f.extend(crc16(&f).to_be_bytes());
    let file = flac(&[(0, flac_streaminfo(44100, 1, 18, 4, [0; 16]))], &f);
    let r = pcm_hash_reader(Cursor::new(file));
    assert!(matches!(r, Err(PcmHashError::Unsupported(_))), "{r:?}");
}

// --- Symphonia as a second decoder ---

/// The PcmHash preimage computed from Symphonia's decoded samples
/// (converted to left-justified 32-bit), with no code shared with ours.
fn symphonia_pcm_hash(bytes: &[u8]) -> Result<String, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no track")?;
    let (id, params) = (track.id, track.codec_params.clone());
    let rate = params.sample_rate.ok_or("no sample rate")?;
    let channels = params.channels.ok_or("no channels")?.count();
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;

    let mut sha = Sha256::new();
    sha.update(b"mdma-pcmhash\0v1\0");
    sha.update([0x01u8, channels as u8]);
    sha.update(rate.to_le_bytes());
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("packet: {e}")),
        };
        if packet.track_id() != id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|e| format!("decode: {e}"))?;
        let mut sb = SampleBuffer::<i32>::new(decoded.capacity() as u64, *decoded.spec());
        sb.copy_interleaved_ref(decoded);
        for s in sb.samples() {
            sha.update(s.to_le_bytes());
        }
    }
    let hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("pcm-sha256-v1:{hex}"))
}

#[test]
fn symphonia_decodes_to_the_same_pcm_hash() {
    let mut report = Vec::new();
    for (group, files) in GROUPS {
        for (name, bytes) in *files {
            // Symphonia mis-handles a trailing ID3v1 tag on FLAC (it hashes
            // differently from every other variant); ours agrees with the
            // STREAMINFO MD5 there, so these two are not compared.
            if name.ends_with("id3v1.flac") || name.ends_with("id3both.flac") {
                continue;
            }
            let want = ok(name, bytes);
            match symphonia_pcm_hash(bytes) {
                Ok(got) if got == want => {}
                Ok(got) => report.push(format!("{group}/{name}: symphonia {got}, ours {want}")),
                Err(e) => report.push(format!("{group}/{name}: symphonia failed: {e}")),
            }
        }
    }
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}

// --- mutation of real LPC streams ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn mutated_real_flac_never_panics_the_decoder(
        base in 0usize..3,
        edits in proptest::collection::vec(
            (any::<usize>(), any::<[u8; 8]>(), any::<bool>()),
            0..6,
        ),
        trunc in proptest::option::of(any::<usize>()),
    ) {
        let originals: [&[u8]; 3] = [
            fx!("s16_l8.flac"),
            fx!("s16_l8_bs192.flac"),
            fx!("s24_l8.flac"),
        ];
        let mut bytes = originals[base].to_vec();
        for (at, window, wide) in edits {
            let len = if wide { 8 } else { 2 };
            let at = at % (bytes.len() - len + 1);
            bytes[at..at + len].copy_from_slice(&window[..len]);
        }
        if let Some(t) = trunc {
            let t = t % (bytes.len() + 1);
            bytes.truncate(t);
        }
        let res = pcm_hash_reader(Cursor::new(bytes));
        prop_assert!(!matches!(res, Err(PcmHashError::DecoderPanic)));
    }
}
