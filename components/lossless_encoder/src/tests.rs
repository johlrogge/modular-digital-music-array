//! The encoder against generated AIFF files (`audio-fixtures`) and the real
//! `flac` binary. These tests need `flac` 1.4 or newer on PATH (the devenv
//! shell and CI provide it); they fail, not skip, when it is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use audio_fixtures::testkit::id3::{self, Id3Version};
use audio_fixtures::testkit::{comt_payload, inst_payload, mark_payload, AiffBuilder, Codec};
use library_ingestion::compute_hash;
use library_ingestion::fact_generator::tag_facts;
use lofty::{Accessor, ItemKey, Probe, Tag, TagExt, TaggedFileExt};
use music_facts::{FactSource, MusicValue};
use pretty_assertions::assert_eq;
use rstest::rstest;
use tempfile::TempDir;

use crate::{EncodeError, Encoder, EnvironmentError, Refusal, RefusalReason, Stage, Verified};

fn refusal_of(e: EncodeError) -> Refusal {
    match e {
        EncodeError::Refused(r) => r,
        other => panic!("expected a refusal, got {other}"),
    }
}

fn environment_of(e: EncodeError) -> EnvironmentError {
    match e {
        EncodeError::Environment(e) => e,
        other => panic!("expected an environment error, got {other}"),
    }
}

/// A source file and an empty staging directory.
struct Run {
    _src_dir: TempDir,
    staging_dir: TempDir,
    src: PathBuf,
    bytes: Vec<u8>,
}

impl Run {
    fn new(builder: &AiffBuilder) -> Run {
        Self::with_bytes(builder.build())
    }

    fn with_bytes(bytes: Vec<u8>) -> Run {
        let src_dir = TempDir::new().unwrap();
        let src = src_dir.path().join("track.aiff");
        fs::write(&src, &bytes).unwrap();
        Run {
            _src_dir: src_dir,
            staging_dir: TempDir::new().unwrap(),
            src,
            bytes,
        }
    }

    fn staging(&self) -> &Path {
        self.staging_dir.path()
    }

    fn encode(&self) -> Result<Verified, EncodeError> {
        crate::encode_aiff_to_flac(&self.src, self.staging())
    }

    fn encode_with(&self, encoder: &Encoder) -> Result<Verified, EncodeError> {
        encoder.encode(&self.src, self.staging())
    }

    /// The refusal of an encode that must be refused (not an environment error).
    fn refusal(&self) -> Refusal {
        refusal_of(self.encode().expect_err("a refusal"))
    }

    fn refusal_with(&self, encoder: &Encoder) -> Refusal {
        refusal_of(self.encode_with(encoder).expect_err("a refusal"))
    }

    fn staged_files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.staging())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn assert_staging_empty(&self) {
        assert_eq!(self.staged_files(), Vec::<String>::new());
    }
}

fn tagged(version: Id3Version) -> AiffBuilder {
    AiffBuilder::new()
        .frames(20_000)
        .id3(id3::full_tag(version))
}

/// `flac -d --keep-foreign-metadata`, run here independently of the encoder.
fn rebuild(flac: &Path) -> Vec<u8> {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.aiff");
    let status = Command::new("flac")
        .args(["-d", "-s", "--keep-foreign-metadata", "-o"])
        .arg(&out)
        .arg(flac)
        .status()
        .expect("flac runs");
    assert!(status.success());
    fs::read(out).unwrap()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn position(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("needle is in the file")
}

fn refused(builder: &AiffBuilder, reason: RefusalReason) -> Refusal {
    let run = Run::new(builder);
    let refusal = run.refusal();
    assert_eq!(refusal.reason, reason, "{refusal}");
    run.assert_staging_empty();
    refusal
}

fn edit_tag(path: &Path, edit: impl FnOnce(&mut Tag)) {
    let mut file = Probe::open(path).unwrap().read().unwrap();
    let tag = file.primary_tag_mut().expect("a tag");
    edit(tag);
    tag.save_to_path(path).unwrap();
}

fn assert_verified(run: &Run, v: &Verified) {
    assert_eq!(v.pcm_hash, audio_hash::pcm_hash(&run.src).unwrap());
    assert_eq!(v.pcm_hash, audio_hash::pcm_hash(&v.flac_path).unwrap());
    assert_eq!(v.source_content_hash, compute_hash(&run.src).unwrap());
    assert_eq!(v.flac_content_hash, compute_hash(&v.flac_path).unwrap());
    assert_eq!(v.source_bytes, run.bytes.len() as u64);
    assert_eq!(v.flac_bytes, fs::metadata(&v.flac_path).unwrap().len());
    let hex = v.source_content_hash.hex();
    assert_eq!(v.flac_path, run.staging().join(format!("{hex}.flac")));
    assert!(v.flac_path.is_absolute());
    // The tag facts are those of the FLAC, bar the vendor's EncoderSoftware.
    let mut from_flac: Vec<MusicValue> = tag_facts(&v.flac_path, &FactSource::new("t", "t", "T"))
        .unwrap()
        .into_iter()
        .map(|(value, _)| value)
        .filter(|value| {
            !matches!(value, MusicValue::EncoderSoftware(_)) || v.tag_facts.contains(value)
        })
        .collect();
    for fact in &v.tag_facts {
        let at = from_flac.iter().position(|f| f == fact);
        assert!(at.is_some(), "{fact:?} is in the FLAC");
        from_flac.swap_remove(at.unwrap());
    }
    assert_eq!(from_flac, vec![]);
    // Only the verified FLAC is left in staging.
    assert_eq!(run.staged_files(), vec![format!("{hex}.flac")]);
    // The original file comes back byte for byte.
    assert!(rebuild(&v.flac_path) == run.bytes, "rebuilt AIFF differs");
}

// --- verified conversions ---

#[rstest]
#[case(16, 1, Id3Version::V23)]
#[case(16, 2, Id3Version::V23)]
#[case(24, 1, Id3Version::V23)]
#[case(24, 2, Id3Version::V23)]
#[case(16, 1, Id3Version::V24)]
#[case(16, 2, Id3Version::V24)]
#[case(24, 1, Id3Version::V24)]
#[case(24, 2, Id3Version::V24)]
fn full_tagged_files_are_verified_and_keep_serato_and_traktor_data(
    #[case] bits: u16,
    #[case] channels: u16,
    #[case] version: Id3Version,
) {
    let run = Run::new(&tagged(version).bits(bits).channels(channels));
    let v = run.encode().expect("verified");
    assert_verified(&run, &v);

    // GEOB (Serato) and PRIV (Traktor) are in the rebuilt AIFF, and in the
    // FLAC's `aiff` APPLICATION block, which holds the chunks verbatim.
    let rebuilt = rebuild(&v.flac_path);
    let flac = fs::read(&v.flac_path).unwrap();
    // (v2.3 writes the GEOB description in UTF-16, so only v2.4 has it as ASCII.)
    let mut needles = vec![id3::GEOB_DATA, id3::PRIV_DATA, id3::PRIV_OWNER.as_bytes()];
    if version == Id3Version::V24 {
        needles.push(id3::GEOB_DESCRIPTION.as_bytes());
    }
    for needle in needles {
        assert!(contains(&rebuilt, needle), "{needle:?} in the rebuilt AIFF");
        assert!(contains(&flac, needle), "{needle:?} in the FLAC");
    }
    assert!(contains(&flac, b"aiff"));
}

#[rstest]
#[case(Id3Version::V23)]
#[case(Id3Version::V24)]
fn the_flac_carries_the_tags_and_pictures_of_the_aiff(#[case] version: Id3Version) {
    let run = Run::new(&tagged(version));
    let v = run.encode().expect("verified");

    let meta = audio_metadata::extract_metadata(&v.flac_path).unwrap();
    assert_eq!(meta.title.as_deref(), Some(id3::TITLE));
    assert_eq!(meta.artist.as_deref(), Some(id3::ARTIST));
    assert_eq!(meta.album.as_deref(), Some(id3::ALBUM));
    assert_eq!(meta.genre.as_deref(), Some(id3::GENRE));
    assert_eq!(meta.track_number, Some(3));
    assert_eq!(meta.disc_number, Some(1));
    assert_eq!(meta.bpm, Some(128.0));
    assert_eq!(meta.key.as_deref(), Some(id3::KEY));
    assert_eq!(meta.comment.as_deref(), Some(id3::COMMENT));
    assert_eq!(meta.year, Some(2024));
    assert!(meta.has_picture);

    let file = Probe::open(&v.flac_path).unwrap().read().unwrap();
    let pictures = file.primary_tag().unwrap().pictures();
    let seen: Vec<_> = pictures
        .iter()
        .map(|p| {
            (
                format!("{:?}", p.pic_type()),
                p.mime_type().map(|m| m.as_str().to_string()),
                p.data().to_vec(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                "CoverFront".to_string(),
                Some("image/png".to_string()),
                id3::TINY_PNG.to_vec()
            ),
            (
                "CoverBack".to_string(),
                Some("image/jpeg".to_string()),
                id3::TINY_JPEG.to_vec()
            ),
        ]
    );
}

/// The outcome per AIFF-C codec with libFLAC 1.5.0, pinned: `twos` is rejected
/// at encode, `NONE` and `sowt` cannot be rebuilt byte for byte (check 5.3).
/// So every AIFF-C is refused and stays an AIFF blob. A libFLAC that fixes
/// this flips these to verified: then update the cases (and ADR-037).
#[rstest]
#[case(Codec::None, RefusalReason::RebuildMismatch)]
#[case(Codec::Sowt, RefusalReason::RebuildMismatch)]
#[case(Codec::Twos, RefusalReason::EncoderFailed)]
fn aiff_c_is_refused_by_libflac_1_5(#[case] codec: Codec, #[case] reason: RefusalReason) {
    let run = Run::new(
        &tagged(Id3Version::V24)
            .frames(3_000)
            .aifc(codec)
            .compression_name("not compressed"),
    );
    let refusal = run.refusal();
    assert_eq!(refusal.reason, reason, "{refusal}");
    assert!(!refusal.detail.is_empty());
    run.assert_staging_empty();
}

#[rstest]
#[case(8)]
#[case(12)]
#[case(20)]
fn depths_claxon_reads_are_verified(#[case] bits: u16) {
    let run = Run::new(&tagged(Id3Version::V24).bits(bits).noise());
    let v = run.encode().expect("verified");
    assert_verified(&run, &v);
}

#[test]
fn extra_chunks_and_odd_lengths_survive_the_rebuild() {
    let builder = AiffBuilder::new()
        .frames(5_000)
        .chunk(b"MARK", mark_payload())
        .chunk(b"INST", inst_payload())
        .chunk(b"COMT", even_comt_payload())
        .chunk(b"ANNO", b"odd")
        .chunk(b"APPL", b"MDMAsome application data")
        .trailing_chunk(b"ANNO", b"tail!")
        .id3(id3::full_tag(Id3Version::V23))
        .trailing_chunk(b"ANNO", b"x");
    let run = Run::new(&builder);
    let v = run.encode().expect("verified");
    assert_verified(&run, &v);
    let rebuilt = rebuild(&v.flac_path);
    for needle in [
        b"MARK".as_slice(),
        b"INST",
        b"COMT",
        b"fixture note",
        b"APPL",
        b"tail!",
    ] {
        assert!(
            contains(&rebuilt, needle),
            "{:?}",
            String::from_utf8_lossy(needle)
        );
    }
}

/// A `COMT` of even length: lofty 0.18 skips the pad byte of an odd one twice
/// and loses its place (see the next test).
fn even_comt_payload() -> Vec<u8> {
    let text = b"fixture note";
    let mut p = 1u16.to_be_bytes().to_vec();
    p.extend(0u32.to_be_bytes());
    p.extend(0u16.to_be_bytes());
    p.extend((text.len() as u16).to_be_bytes());
    p.extend(text);
    p
}

#[test]
fn an_odd_comt_chunk_is_refused_because_lofty_misreads_it() {
    // The tag facts cannot be read, so the conversion cannot be verified: the
    // AIFF is kept as it is. Delete this test when lofty reads it.
    let builder = AiffBuilder::new()
        .frames(500)
        .chunk(b"COMT", comt_payload())
        .id3(id3::full_tag(Id3Version::V24));
    refused(&builder, RefusalReason::UnreadableSource);
}

/// An AIFF whose ID3 tag states an encoder (`TSSE`) and a title.
fn run_with_encoder_tag(encoder: &str) -> Run {
    // A hand-built ID3v2.4 tag: TIT2 and TSSE, UTF-8.
    let frame = |id: &[u8; 4], text: &str| {
        let mut f = id.to_vec();
        f.extend(audio_fixtures::syncsafe(text.len() as u32 + 1));
        f.extend([0, 0, 3]); // flags, UTF-8
        f.extend(text.as_bytes());
        f
    };
    let frames = [frame(b"TIT2", "Has an encoder"), frame(b"TSSE", encoder)].concat();
    let mut tag = b"ID3\x04\0\0".to_vec();
    tag.extend(audio_fixtures::syncsafe(frames.len() as u32));
    tag.extend(frames);
    Run::new(&AiffBuilder::new().frames(3_000).id3(tag))
}

#[test]
fn an_id3_encoder_frame_is_kept_in_the_flac_but_is_no_encoder_software_fact() {
    // lofty reads ID3 `TSSE` as EncoderSettings, which is no tag fact, so an
    // AIFF cannot state an EncoderSoftware fact through its ID3 tag.
    let run = run_with_encoder_tag("Fixture Encoder 1.0");
    let v = run.encode().expect("verified");
    assert_verified(&run, &v);
    assert!(!v
        .tag_facts
        .iter()
        .any(|f| matches!(f, MusicValue::EncoderSoftware(_))));
    let flac = Probe::open(&v.flac_path).unwrap().read().unwrap();
    assert_eq!(
        flac.primary_tag()
            .unwrap()
            .get_string(&ItemKey::EncoderSettings),
        Some("Fixture Encoder 1.0")
    );
}

#[test]
fn a_flac_side_encoder_software_the_aiff_does_not_state_is_ignored() {
    // The exception of check 5.4: the vendor string is not a tag the AIFF states.
    let run = run_with_encoder_tag("Fixture Encoder 1.0");
    let encoder = Encoder::new().with_hook(|stage, flac| {
        if stage == Stage::Tagged {
            edit_tag(flac, |tag| {
                tag.remove_key(&ItemKey::EncoderSoftware);
                tag.insert_text(ItemKey::EncoderSoftware, "Another Encoder".to_string());
            });
        }
    });
    let v = run.encode_with(&encoder).expect("verified");
    let flac = tag_facts(&v.flac_path, &FactSource::new("t", "t", "T")).unwrap();
    assert!(
        flac.iter()
            .any(|(value, _)| *value == MusicValue::EncoderSoftware("Another Encoder".into())),
        "the hook put the vendor in"
    );
}

#[test]
fn the_flacs_own_vendor_string_is_not_a_tag_the_aiff_states() {
    // No encoder in the AIFF: the FLAC's libFLAC vendor must not count.
    let run = Run::new(&tagged(Id3Version::V24));
    let v = run.encode().expect("verified");
    assert!(!v
        .tag_facts
        .iter()
        .any(|f| matches!(f, MusicValue::EncoderSoftware(_))));
}

#[test]
fn identical_encodes_at_the_same_time_do_not_trample_each_other() {
    let run = Run::new(&AiffBuilder::new().frames(20_000).noise());
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4).map(|_| scope.spawn(|| run.encode())).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for result in results {
        assert_verified(&run, &result.expect("verified"));
    }
}

#[test]
fn a_file_without_tags_is_verified() {
    let run = Run::new(&AiffBuilder::new().frames(3_000));
    let v = run.encode().expect("verified");
    assert_verified(&run, &v);
}

#[test]
fn a_missing_staging_directory_is_created_and_a_stale_file_replaced() {
    let run = Run::new(&AiffBuilder::new().frames(3_000));
    let nested = run.staging().join("a").join("b");
    let hex = compute_hash(&run.src).unwrap();
    let hex = hex.hex().to_string();
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join(format!("{hex}.flac")), b"left by a crash").unwrap();
    let v = crate::encode_aiff_to_flac(&run.src, &nested).expect("verified");
    assert_eq!(v.flac_path, nested.join(format!("{hex}.flac")));
    assert_eq!(audio_hash::pcm_hash(&v.flac_path).unwrap(), v.pcm_hash);
}

#[test]
fn flac_is_smaller_than_the_aiff() {
    let run = Run::new(&tagged(Id3Version::V24).bits(24));
    let v = run.encode().expect("verified");
    let ratio = v.flac_bytes as f64 / v.source_bytes as f64;
    assert!(ratio < 0.8, "ratio {ratio:.3}");
}

// --- refusals, before flac runs ---

#[test]
fn float_and_compressed_aiff_c_are_refused() {
    for codec in [Codec::Fl32, Codec::Ulaw] {
        refused(
            &AiffBuilder::new().frames(500).aifc(codec),
            RefusalReason::FloatOrCompressed,
        );
    }
}

#[test]
fn three_channels_are_refused() {
    refused(
        &AiffBuilder::new().frames(500).channels(3),
        RefusalReason::TooManyChannels,
    );
}

#[rstest]
#[case(32)]
#[case(18)]
#[case(4)]
fn depths_claxon_cannot_read_are_refused(#[case] bits: u16) {
    let refusal = refused(
        &AiffBuilder::new().frames(500).bits(bits),
        RefusalReason::UnsupportedDepth,
    );
    assert!(refusal.detail.contains(&bits.to_string()), "{refusal}");
}

#[test]
fn what_is_not_an_aiff_is_refused() {
    let wav = Run::with_bytes(b"RIFF\x24\0\0\0WAVEfmt ".to_vec());
    assert_eq!(wav.refusal().reason, RefusalReason::UnsupportedFormat);
    wav.assert_staging_empty();

    let junk = Run::with_bytes(vec![7; 100]);
    assert_eq!(junk.refusal().reason, RefusalReason::UnreadableSource);

    let gone = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let refusal = refusal_of(
        crate::encode_aiff_to_flac(&gone.path().join("nope.aiff"), staging.path()).unwrap_err(),
    );
    assert_eq!(refusal.reason, RefusalReason::UnreadableSource);
}

#[test]
fn a_flac_source_is_an_unsupported_format() {
    let aiff = Run::new(&AiffBuilder::new().frames(3_000));
    let v = aiff.encode().expect("verified");
    let flac = Run::with_bytes(fs::read(&v.flac_path).unwrap());
    let refusal = flac.refusal();
    assert_eq!(
        refusal.reason,
        RefusalReason::UnsupportedFormat,
        "{refusal}"
    );
    flac.assert_staging_empty();
}

#[test]
fn a_fractional_sample_rate_is_refused_with_its_own_reason() {
    let mut bytes = AiffBuilder::new().frames(500).build();
    // COMM starts at 12; its 10-byte rate is at 8 (chunk header) + 8 into it.
    bytes[28..38].copy_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0x80, 0, 0, 0, 0, 0]); // 44100.5
    let run = Run::with_bytes(bytes);
    assert_eq!(run.refusal().reason, RefusalReason::NonIntegerRate);
    run.assert_staging_empty();
}

#[test]
fn format_refusals_do_not_need_flac() {
    let encoder = Encoder::with_flac_binary("/nonexistent/flac");
    let run = Run::new(&AiffBuilder::new().frames(500).aifc(Codec::Fl32));
    let refusal = run.refusal_with(&encoder);
    assert_eq!(refusal.reason, RefusalReason::FloatOrCompressed);
}

// --- the machine, not the file ---

#[test]
fn a_missing_flac_is_an_environment_error_not_a_refusal() {
    let run = Run::new(&AiffBuilder::new().frames(500));
    let error = environment_of(
        run.encode_with(&Encoder::with_flac_binary("/nonexistent/flac"))
            .unwrap_err(),
    );
    assert!(matches!(error, EnvironmentError::FlacMissing(_)), "{error}");
    assert!(error.to_string().contains("flac program was not found"));
    run.assert_staging_empty();
}

#[test]
fn a_program_that_is_not_a_flac_version_is_an_environment_error() {
    // `echo --version` prints a GNU version line, not `flac x.y`.
    let run = Run::new(&AiffBuilder::new().frames(500));
    let error = environment_of(
        run.encode_with(&Encoder::with_flac_binary("echo"))
            .unwrap_err(),
    );
    assert!(matches!(error, EnvironmentError::FlacTooOld(_)), "{error}");
    run.assert_staging_empty();
}

#[test]
fn a_staging_directory_that_cannot_be_made_is_an_environment_error() {
    let run = Run::new(&AiffBuilder::new().frames(500));
    // A file where a directory is needed.
    let blocker = run.staging().join("blocker");
    fs::write(&blocker, b"x").unwrap();
    let error =
        environment_of(crate::encode_aiff_to_flac(&run.src, &blocker.join("staging")).unwrap_err());
    assert!(matches!(error, EnvironmentError::Staging(_)), "{error}");
}

// --- a full disk is the machine's, not the file's ---

#[test]
fn the_real_free_space_probe_reads_the_file_system() {
    let dir = TempDir::new().unwrap();
    assert!(crate::free_bytes(dir.path()).unwrap() > 0);
    assert!(crate::free_bytes(&dir.path().join("missing")).is_err());
}

#[test]
fn too_little_free_space_is_a_staging_error_before_flac_runs() {
    let run = Run::new(&AiffBuilder::new().frames(3_000));
    let encoder = Encoder::new().with_free_space(|_| Ok(0));
    let error = environment_of(run.encode_with(&encoder).unwrap_err());
    assert!(
        matches!(&error, EnvironmentError::Staging(what) if what.contains("not enough space")),
        "{error}"
    );
    run.assert_staging_empty();
}

#[test]
fn a_failing_space_probe_is_a_staging_error() {
    let run = Run::new(&AiffBuilder::new().frames(3_000));
    let encoder =
        Encoder::new().with_free_space(|_| Err(std::io::Error::other("statvfs went wrong")));
    let error = environment_of(run.encode_with(&encoder).unwrap_err());
    assert!(matches!(error, EnvironmentError::Staging(_)), "{error}");
}

#[test]
fn a_refusal_with_the_disk_full_afterwards_is_blamed_on_the_disk() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Plenty of room at the start; none once flac has failed. `twos` makes
    // flac fail, which on its own is a refusal (see the AIFF-C test).
    let calls = AtomicUsize::new(0);
    let encoder = Encoder::new().with_free_space(move |_| {
        Ok(if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            u64::MAX
        } else {
            0
        })
    });
    let run = Run::new(&AiffBuilder::new().frames(3_000).aifc(Codec::Twos));
    let error = environment_of(run.encode_with(&encoder).unwrap_err());
    assert!(
        matches!(&error, EnvironmentError::Staging(what) if what.contains("not enough space")),
        "{error}"
    );
    run.assert_staging_empty();
}

#[test]
fn a_refusal_with_room_to_spare_stays_a_refusal() {
    let encoder = Encoder::new().with_free_space(|_| Ok(u64::MAX));
    let run = Run::new(&AiffBuilder::new().frames(3_000).aifc(Codec::Twos));
    let refusal = refusal_of(run.encode_with(&encoder).unwrap_err());
    assert_eq!(refusal.reason, RefusalReason::EncoderFailed);
}

#[test]
fn io_on_the_staged_flac_is_a_staged_file_error() {
    // The staged FLAC vanishes after flac wrote it: tagging fails on I/O.
    let run = Run::new(&tagged(Id3Version::V24));
    let encoder = Encoder::new().with_hook(|stage, flac| {
        if stage == Stage::Encoded {
            fs::remove_file(flac).unwrap();
        }
    });
    let error = environment_of(run.encode_with(&encoder).unwrap_err());
    assert!(matches!(error, EnvironmentError::StagedFile(_)), "{error}");
    run.assert_staging_empty();
}

#[test]
fn io_errors_are_told_from_parse_errors() {
    use audio_hash::{AudioHashError, PcmHashError};
    let flac = Path::new("/staging/x.encoding.flac");
    let refuse = |d: String| Refusal {
        reason: RefusalReason::PcmMismatch,
        detail: d,
    };

    let io = PcmHashError::Container(AudioHashError::Io(std::io::Error::other("EIO")));
    let classified = crate::staged_pcm_error(flac, io, refuse);
    assert!(
        matches!(
            classified,
            EncodeError::Environment(EnvironmentError::StagedFile(_))
        ),
        "{classified}"
    );

    for parse in [
        PcmHashError::Decode("bad frame"),
        PcmHashError::Md5Mismatch,
        PcmHashError::Container(AudioHashError::NoAudio),
    ] {
        let classified = crate::staged_pcm_error(flac, parse, refuse);
        assert!(
            matches!(classified, EncodeError::Refused(_)),
            "{classified}"
        );
    }

    let lofty_io = audio_metadata::MetadataError::Lofty(lofty::LoftyError::from(
        std::io::Error::other("ENOSPC"),
    ));
    assert!(crate::tags::is_io(&lofty_io));
    assert!(crate::tags::is_io(&audio_metadata::MetadataError::Io(
        std::io::Error::other("EIO")
    )));
    assert!(!crate::tags::is_io(
        &audio_metadata::MetadataError::ParseError("bad".into())
    ));
}

// --- acceptance fails: the checks catch damage ---

#[test]
fn a_flipped_audio_byte_is_caught_and_staging_is_cleaned() {
    let run = Run::new(&tagged(Id3Version::V24).noise());
    let encoder = Encoder::new().with_hook(|stage, flac| {
        if stage == Stage::Encoded {
            let mut bytes = fs::read(flac).unwrap();
            let at = bytes.len() * 9 / 10;
            bytes[at] ^= 0x55;
            fs::write(flac, bytes).unwrap();
        }
    });
    let refusal = run.refusal_with(&encoder);
    assert!(
        matches!(
            refusal.reason,
            RefusalReason::PcmMismatch | RefusalReason::RebuildMismatch
        ),
        "{refusal}"
    );
    run.assert_staging_empty();
}

#[test]
fn a_damaged_aiff_block_fails_the_rebuild_check() {
    // The audio is intact, the stored chunks are not: only 5.3 can tell.
    let run = Run::new(&tagged(Id3Version::V24));
    let encoder = Encoder::new().with_hook(|stage, flac| {
        if stage == Stage::Encoded {
            let mut bytes = fs::read(flac).unwrap();
            let at = position(&bytes, id3::PRIV_DATA);
            bytes[at] ^= 0x01;
            fs::write(flac, bytes).unwrap();
        }
    });
    let refusal = run.refusal_with(&encoder);
    assert_eq!(refusal.reason, RefusalReason::RebuildMismatch, "{refusal}");
    run.assert_staging_empty();
}

#[test]
fn changed_tags_fail_the_tag_check() {
    let run = Run::new(&tagged(Id3Version::V24));
    let encoder = Encoder::new().with_hook(|stage, flac| {
        if stage == Stage::Tagged {
            edit_tag(flac, |tag| {
                tag.set_title("Another title".to_string());
            });
        }
    });
    let refusal = run.refusal_with(&encoder);
    assert_eq!(refusal.reason, RefusalReason::TagMismatch, "{refusal}");
    run.assert_staging_empty();
}

#[test]
fn missing_pictures_fail_the_picture_check() {
    let run = Run::new(&tagged(Id3Version::V23));
    let encoder = Encoder::new().with_hook(|stage, flac| {
        if stage == Stage::Tagged {
            edit_tag(flac, |tag| {
                tag.remove_picture(1);
            });
        }
    });
    let refusal = run.refusal_with(&encoder);
    assert_eq!(refusal.reason, RefusalReason::PictureMismatch, "{refusal}");
    assert!(refusal.detail.contains("2 in the AIFF, 1 in the FLAC"));
    run.assert_staging_empty();
}

#[test]
fn refusals_read_well() {
    let refusal = Refusal {
        reason: RefusalReason::UnsupportedDepth,
        detail: "32-bit".to_string(),
    };
    assert_eq!(refusal.to_string(), "unsupported bit depth: 32-bit");
}

// --- real files ---

/// `MDMA_REAL_AIFF_DIR=/path cargo test -p lossless-encoder -- --ignored --nocapture`
///
/// Every `*.aif` / `*.aiff` of the directory goes through the encoder. Refusals
/// are outcomes, not failures; a panic is.
#[test]
#[ignore = "needs MDMA_REAL_AIFF_DIR, real files that are not in the repo"]
fn real_files() {
    let dir = std::env::var("MDMA_REAL_AIFF_DIR").expect("MDMA_REAL_AIFF_DIR is set");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("the directory reads")
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("aif") || e.eq_ignore_ascii_case("aiff"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .aif or .aiff files in {dir}");

    let staging = TempDir::new().unwrap();
    println!(
        "{:<40} {:<9} {:<22} {:>9} {:>9} {:>6} {:>8}",
        "file", "outcome", "reason", "aiff MB", "flac MB", "ratio", "seconds"
    );
    for file in &files {
        let started = Instant::now();
        let result = crate::encode_aiff_to_flac(file, staging.path());
        let seconds = started.elapsed().as_secs_f64();
        let name = file.file_name().unwrap().to_string_lossy();
        let name: String = name.chars().take(40).collect();
        let size = |bytes: u64| bytes as f64 / 1_048_576.0;
        let aiff_mb = size(fs::metadata(file).unwrap().len());
        match result {
            Ok(v) => {
                println!(
                    "{name:<40} {:<9} {:<22} {:>9.2} {:>9.2} {:>6.2} {seconds:>8.1}",
                    "verified",
                    "",
                    aiff_mb,
                    size(v.flac_bytes),
                    v.flac_bytes as f64 / v.source_bytes as f64,
                );
                fs::remove_file(&v.flac_path).unwrap();
            }
            Err(EncodeError::Refused(refusal)) => {
                println!(
                    "{name:<40} {:<9} {:<22} {:>9.2} {:>9} {:>6} {seconds:>8.1}   {}",
                    "refused",
                    format!("{:?}", refusal.reason),
                    aiff_mb,
                    "-",
                    "-",
                    refusal.detail,
                );
            }
            Err(EncodeError::Environment(error)) => {
                println!(
                    "{name:<40} {:<9} {:<22} {:>9.2} {:>9} {:>6} {seconds:>8.1}   {error}",
                    "retry", "Environment", aiff_mb, "-", "-",
                );
            }
        }
        assert_eq!(
            fs::read_dir(staging.path()).unwrap().count(),
            0,
            "staging is clean after {name}"
        );
    }
}
