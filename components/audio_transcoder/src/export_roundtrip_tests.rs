//! ADR-037: a stored FLAC (converted from an AIFF at ingest) exported as AIFF through
//! the real export path (`transcode` with the `SourceAudio` the console builds for a
//! blob) gives a file with the same PcmHash as the stored FLAC. They need `flac` 1.4+
//! and `ffmpeg` on PATH (the devenv shell and CI provide both); they fail, not skip,
//! when one is missing.

use std::fs;

use audio_fixtures::testkit::{AiffBuilder, Signal};
use audio_hash::pcm_hash;
use music_primitives::PcmHash;
use pretty_assertions::assert_eq;
use rstest::rstest;
use tempfile::TempDir;

use crate::{describe_source, transcode, ExportFormat, ExportMetadata};

/// A stored FLAC converted from a generated AIFF, with the PcmHash of the AIFF.
struct Stored {
    _dir: TempDir,
    flac: std::path::PathBuf,
    aiff_pcm: PcmHash,
}

fn store(bits: u16, rate: u32) -> Stored {
    let dir = TempDir::new().unwrap();
    let aiff = dir.path().join("track.aiff");
    let builder = AiffBuilder::new()
        .channels(2)
        .bits(bits)
        .rate(rate)
        .frames(rate / 2)
        .signal(Signal::Noise);
    fs::write(&aiff, builder.build()).unwrap();
    let staging = dir.path().join("staging");
    fs::create_dir(&staging).unwrap();

    let verified = lossless_encoder::encode_aiff_to_flac(&aiff, &staging)
        .unwrap_or_else(|e| panic!("{bits}-bit {rate} Hz did not convert: {e}"));
    let aiff_pcm = pcm_hash(&aiff).unwrap();
    assert_eq!(
        verified.pcm_hash, aiff_pcm,
        "the stored FLAC holds the audio"
    );
    Stored {
        flac: verified.flac_path,
        aiff_pcm,
        _dir: dir,
    }
}

/// Export the stored FLAC as AIFF the way the console does and hash the result.
fn exported_pcm_hash(stored: &Stored) -> PcmHash {
    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("export.aiff");
    transcode(
        &stored.flac,
        &out,
        &ExportFormat::Aiff,
        &ExportMetadata::default(),
        &describe_source(&stored.flac, "flac").audio,
    )
    .unwrap();
    pcm_hash(&out).unwrap()
}

#[rstest]
// exported at 16 bits
#[case::twelve_bit_44k(12, 44_100)]
#[case::sixteen_bit_44k(16, 44_100)]
#[case::sixteen_bit_48k(16, 48_000)]
// exported at 24 bits
#[case::twenty_bit_44k(20, 44_100)]
#[case::twenty_four_bit_44k(24, 44_100)]
#[case::twenty_four_bit_48k(24, 48_000)]
fn exporting_a_stored_flac_as_aiff_keeps_its_pcm_hash(#[case] bits: u16, #[case] rate: u32) {
    let stored = store(bits, rate);

    let exported = exported_pcm_hash(&stored);

    assert_eq!(exported, pcm_hash(&stored.flac).unwrap());
    assert_eq!(exported, stored.aiff_pcm);
}

/// A documented ADR-012 false negative: above 48 kHz the export resamples for CDJs
/// (`SourceAudio::cdj_downsample_rate`), so its samples are different samples and
/// the PcmHash differs although it is the same recording. The ADR-013 dedup scan
/// offers such pairs for confirmation instead of merging them. Must not become
/// "equal" by accident: if it does, the export stopped resampling.
#[test]
fn exporting_a_96k_stored_flac_as_aiff_differs_in_pcm_hash() {
    let stored = store(24, 96_000);

    let exported = exported_pcm_hash(&stored);

    assert_ne!(exported, pcm_hash(&stored.flac).unwrap());
}
