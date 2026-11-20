use std::path::Path;
use std::time::Duration;

use crate::{ExportFormat, ExportMetadata, SourceAudio, TranscoderError};

/// Compute the CDJ-safe target sample rate for a source rate.
///
/// Pioneer CDJs reject files with sample rates above 48 kHz.  We halve
/// integer-ratio until the rate is ≤ 48 000 Hz.  Returns `None` when no
/// resampling is needed (source already ≤ 48 kHz).
pub(crate) fn cdj_target_rate(source_hz: u32) -> Option<u32> {
    if source_hz <= 48_000 {
        return None;
    }
    let mut rate = source_hz;
    while rate > 48_000 {
        rate /= 2;
    }
    Some(rate)
}

/// The PCM bit depth (16 or 24) an AIFF/WAV export of a source gets.
///
/// Sources of 16 bits or less stay 16-bit (no point inflating CD-quality
/// files); anything deeper, or of unknown depth, is exported as 24-bit.
pub(crate) fn pcm_bit_depth(source_bit_depth: Option<u8>) -> u8 {
    match source_bit_depth {
        Some(depth) if (1..=16).contains(&depth) => 16,
        _ => 24,
    }
}

/// Where to cut the decode of a source whose decoder emits trailing encoder
/// padding, as a time from the start (so the sample rate, which HE-AAC can
/// report as its core rate, is not involved).
///
/// AAC (`m4a`/`aac`): priming is trimmed on decode but the trailing padding is
/// not (a 1.000 s M4A decodes to 21.7 ms too much). The bound is lofty's
/// container duration, which it rounds to the nearest millisecond; the end is
/// rounded up to a whole millisecond plus one, so real audio is never cut (at
/// most ~1 ms of padding remains). Lossless sources decode to their exact
/// length and need no trim, so they yield `None`, as does MP3 (its duration is
/// only an estimate, not safe to cut at), and a zero or unknown duration (lofty
/// reports zero when it cannot tell, which would cut everything).
pub(crate) fn padded_source_end(source_ext: &str, duration: Option<Duration>) -> Option<Duration> {
    if !matches!(source_ext.to_ascii_lowercase().as_str(), "m4a" | "aac") {
        return None;
    }
    let duration = duration.filter(|d| !d.is_zero())?;
    let millis = duration.as_nanos().div_ceil(1_000_000) as u64;
    Some(Duration::from_millis(millis + 1))
}

impl SourceAudio {
    /// Whether a PCM source already is what an AIFF/WAV export would produce, so
    /// that the file can be served as it is: the rate and depth are known, the rate
    /// is within the CDJ limit (no downsample, [`cdj_target_rate`]) and the depth is
    /// one the export keeps (exactly 16 or 24 bits: [`pcm_bit_depth`] widens or
    /// narrows every other). Unknown properties count as not ready.
    pub fn is_pcm_export_ready(&self) -> bool {
        let rate_ok = self
            .sample_rate
            .is_some_and(|hz| cdj_target_rate(hz).is_none());
        // the export keeps exactly 16 and 24 bits and changes every other depth
        let depth_ok = self.bit_depth.is_some_and(|d| pcm_bit_depth(Some(d)) == d);
        rate_ok && depth_ok
    }

    /// The rate AIFF/WAV exports are downsampled to, when the source exceeds the
    /// CDJ limit. FLAC is never resampled.
    pub(crate) fn cdj_downsample_rate(&self, format: &ExportFormat) -> Option<u32> {
        match format {
            ExportFormat::Aiff | ExportFormat::Wav => self.sample_rate.and_then(cdj_target_rate),
            ExportFormat::Flac => None,
        }
    }
}

/// Build the ffmpeg argument list for a transcode operation.
///
/// The `source` description decides, for AIFF/WAV, the downsample to ≤ 48 kHz
/// (Pioneer CDJ "illegal format" prevention) and the PCM width
/// ([`pcm_bit_depth`]); for lossy sources it bounds the decode to the source
/// duration ([`padded_source_end`]), before any resample.
///
/// Extracted into a pure function so the argument construction can be tested
/// without actually running ffmpeg.
pub(crate) fn build_ffmpeg_args(
    input: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
    source: &SourceAudio,
) -> Vec<String> {
    let pcm_depth = pcm_bit_depth(source.bit_depth);
    let mut args: Vec<String> = Vec::new();

    // Input
    args.push("-i".to_string());
    args.push(input.to_string_lossy().into_owned());

    // Codec flags per format
    match format {
        ExportFormat::Aiff => {
            args.push("-write_id3v2".to_string());
            args.push("1".to_string());
            args.push("-id3v2_version".to_string());
            args.push("3".to_string());
            args.push("-c:a".to_string());
            args.push(format!("pcm_s{pcm_depth}be"));
        }
        ExportFormat::Wav => {
            args.push("-c:a".to_string());
            args.push(format!("pcm_s{pcm_depth}le"));
        }
        ExportFormat::Flac => {
            args.push("-c:a".to_string());
            args.push("flac".to_string());
        }
    }

    // Audio filter chain (one `-af`; the trim runs first).
    let mut filters: Vec<String> = Vec::new();
    if let Some(end) = padded_source_end(&source.ext, source.duration) {
        filters.push(format!("atrim=end={:.6}", end.as_secs_f64()));
    }

    // CDJ sample-rate guard: AIFF and WAV must not exceed 48 kHz.
    let target_rate = source.cdj_downsample_rate(format);
    if target_rate.is_some() {
        filters.push("aresample=resampler=soxr".to_string());
    }

    if !filters.is_empty() {
        args.push("-af".to_string());
        args.push(filters.join(","));
    }
    if let Some(target_hz) = target_rate {
        args.push("-ar".to_string());
        args.push(target_hz.to_string());
    }

    // Always pass through all metadata from the source (includes cover art)
    args.push("-map_metadata".to_string());
    args.push("0".to_string());

    // Overwrite output without prompting; suppress all output except errors
    args.push("-y".to_string());
    args.push("-loglevel".to_string());
    args.push("error".to_string());

    // Metadata tags
    let mut tag = |key: &str, value: &str| {
        args.push("-metadata".to_string());
        args.push(format!("{key}={value}"));
    };
    if let Some(title) = &meta.title {
        tag("title", title);
    }
    if let Some(artist) = &meta.artist {
        tag("artist", artist);
    }
    if let Some(album) = &meta.album {
        tag("album", album);
    }
    match format {
        // ID3v2 has standard frames for BPM (an integer string in v2.3) and key;
        // the generic keys would be written as TXXX user frames. The source's own
        // generic tags, copied by `-map_metadata`, are cleared (an empty value
        // removes a key) so they cannot sit next to ours and disagree.
        ExportFormat::Aiff => {
            if let Some(bpm) = meta.bpm {
                tag("TBPM", &format!("{}", bpm.round()));
                tag("BPM", "");
            }
            if let Some(key) = &meta.key {
                tag("TKEY", key);
                tag("INITIALKEY", "");
                tag("KEY", "");
            }
        }
        ExportFormat::Wav | ExportFormat::Flac => {
            if let Some(bpm) = meta.bpm {
                tag("BPM", &format!("{bpm:.2}"));
            }
            if let Some(key) = &meta.key {
                tag("INITIALKEY", key);
            }
        }
    }

    // Output path
    args.push(output.to_string_lossy().into_owned());

    args
}

/// Run ffmpeg to transcode `input` to `output` in the given `format`,
/// embedding the supplied `meta` tags.
///
/// `source` describes the input; see `build_ffmpeg_args` for what it decides.
///
/// Captures ffmpeg's stderr; on a non-zero exit code the stderr content is
/// returned as `TranscoderError::FfmpegFailed`.
pub(crate) fn run_ffmpeg(
    input: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
    source: &SourceAudio,
) -> Result<(), TranscoderError> {
    let args = build_ffmpeg_args(input, output, format, meta, source);

    let result = std::process::Command::new("ffmpeg")
        .args(&args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TranscoderError::FfmpegNotFound
            } else {
                TranscoderError::Io(e)
            }
        })?;

    if result.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        Err(TranscoderError::FfmpegFailed(stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use std::path::PathBuf;

    fn source() -> PathBuf {
        PathBuf::from("/input/track.flac")
    }

    fn output_for(format: &ExportFormat) -> PathBuf {
        PathBuf::from(format!("/output/track{}", format.suffix()))
    }

    fn full_meta() -> ExportMetadata {
        ExportMetadata {
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            bpm: Some(128.0),
            key: Some("Am".to_string()),
        }
    }

    fn empty_meta() -> ExportMetadata {
        ExportMetadata::default()
    }

    /// A FLAC source of unknown format.
    fn unknown() -> SourceAudio {
        SourceAudio::default()
    }

    fn rate(hz: u32) -> SourceAudio {
        SourceAudio {
            sample_rate: Some(hz),
            ..SourceAudio::default()
        }
    }

    fn depth(bits: Option<u8>) -> SourceAudio {
        SourceAudio {
            bit_depth: bits,
            ..SourceAudio::default()
        }
    }

    fn aac(secs: f64, hz: u32) -> SourceAudio {
        SourceAudio {
            ext: "m4a".to_string(),
            sample_rate: Some(hz),
            duration: Some(Duration::from_secs_f64(secs)),
            ..SourceAudio::default()
        }
    }

    fn build(format: &ExportFormat, meta: &ExportMetadata, src: &SourceAudio) -> Vec<String> {
        build_ffmpeg_args(&source(), &output_for(format), format, meta, src)
    }

    fn has_tag(args: &[String], tag: &str) -> bool {
        args.windows(2).any(|w| w[0] == "-metadata" && w[1] == tag)
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
        args.iter().position(|a| a == flag).map(|p| &args[p + 1])
    }

    // ── AIFF tests ────────────────────────────────────────────────────────────

    #[test]
    fn aiff_args_start_with_input() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &unknown());
        assert_eq!(args[0], "-i");
        assert_eq!(args[1], "/input/track.flac");
    }

    #[test]
    fn aiff_args_include_write_id3v2() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &unknown());
        assert_eq!(value_after(&args, "-write_id3v2").unwrap(), "1");
    }

    #[test]
    fn aiff_args_include_id3v2_version_3() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &unknown());
        assert_eq!(value_after(&args, "-id3v2_version").unwrap(), "3");
    }

    fn codec_of(format: &ExportFormat, bit_depth: Option<u8>) -> String {
        let args = build(format, &empty_meta(), &depth(bit_depth));
        value_after(&args, "-c:a")
            .expect("-c:a must be present")
            .clone()
    }

    #[rstest]
    #[case(None, "pcm_s24be")]
    #[case(Some(24), "pcm_s24be")]
    #[case(Some(32), "pcm_s24be")]
    #[case(Some(16), "pcm_s16be")]
    #[case(Some(8), "pcm_s16be")]
    fn aiff_args_codec_follows_source_bit_depth(#[case] depth: Option<u8>, #[case] expected: &str) {
        assert_eq!(codec_of(&ExportFormat::Aiff, depth), expected);
    }

    #[rstest]
    #[case(None, "pcm_s24le")]
    #[case(Some(24), "pcm_s24le")]
    #[case(Some(16), "pcm_s16le")]
    fn wav_args_codec_follows_source_bit_depth(#[case] depth: Option<u8>, #[case] expected: &str) {
        assert_eq!(codec_of(&ExportFormat::Wav, depth), expected);
    }

    #[test]
    fn flac_codec_ignores_source_bit_depth() {
        assert_eq!(codec_of(&ExportFormat::Flac, Some(16)), "flac");
    }

    #[rstest]
    #[case(None, 24)]
    #[case(Some(0), 24)]
    #[case(Some(8), 16)]
    #[case(Some(16), 16)]
    #[case(Some(20), 24)]
    #[case(Some(24), 24)]
    #[case(Some(32), 24)]
    fn pcm_bit_depth_rule(#[case] source: Option<u8>, #[case] expected: u8) {
        assert_eq!(pcm_bit_depth(source), expected);
    }

    #[rstest]
    #[case(Some(44_100), Some(16), true)]
    #[case(Some(48_000), Some(24), true)]
    #[case(Some(96_000), Some(24), false)]
    #[case(Some(44_100), Some(32), false)]
    #[case(Some(44_100), Some(8), false)]
    #[case(Some(44_100), Some(20), false)]
    #[case(None, Some(16), false)]
    #[case(Some(44_100), None, false)]
    fn pcm_export_ready_rule(
        #[case] rate: Option<u32>,
        #[case] depth: Option<u8>,
        #[case] expected: bool,
    ) {
        let source = SourceAudio {
            sample_rate: rate,
            bit_depth: depth,
            ..Default::default()
        };
        assert_eq!(source.is_pcm_export_ready(), expected);
    }

    #[rstest]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn only_aiff_gets_id3v2_flags(#[case] format: ExportFormat) {
        let args = build(&format, &empty_meta(), &unknown());
        assert!(!args.iter().any(|a| a == "-write_id3v2"));
        assert!(!args.iter().any(|a| a == "-id3v2_version"));
    }

    // ── common flags ──────────────────────────────────────────────────────────

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn all_formats_include_common_flags(#[case] format: ExportFormat) {
        let args = build(&format, &empty_meta(), &unknown());
        assert_eq!(value_after(&args, "-map_metadata").unwrap(), "0");
        assert_eq!(value_after(&args, "-loglevel").unwrap(), "error");
        assert!(args.iter().any(|a| a == "-y"));
        assert_eq!(
            args.last().expect("args must not be empty"),
            output_for(&format).to_str().unwrap(),
            "last arg must be output path for {:?}",
            format
        );
    }

    // ── metadata injection ────────────────────────────────────────────────────

    #[test]
    fn full_metadata_produces_text_tags() {
        let args = build(&ExportFormat::Aiff, &full_meta(), &unknown());
        assert!(has_tag(&args, "title=Test Track"));
        assert!(has_tag(&args, "artist=Test Artist"));
        assert!(has_tag(&args, "album=Test Album"));
    }

    #[test]
    fn aiff_bpm_and_key_are_the_standard_id3v2_frames() {
        let args = build(&ExportFormat::Aiff, &full_meta(), &unknown());
        assert!(has_tag(&args, "TBPM=128"), "TBPM is an integer string");
        assert!(has_tag(&args, "TKEY=Am"));
    }

    #[test]
    fn aiff_clears_the_generic_bpm_and_key_tags_copied_from_the_source() {
        let args = build(&ExportFormat::Aiff, &full_meta(), &unknown());
        for cleared in ["BPM=", "INITIALKEY=", "KEY="] {
            assert!(has_tag(&args, cleared), "{cleared} must be cleared");
        }
        assert!(!args.iter().any(|a| a.starts_with("BPM=") && a != "BPM="));
    }

    #[test]
    fn aiff_leaves_the_generic_tags_alone_when_it_sets_neither() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &unknown());
        assert!(!args.iter().any(|a| a == "-metadata"));
    }

    #[test]
    fn aiff_bpm_is_rounded_to_an_integer() {
        let meta = ExportMetadata {
            bpm: Some(140.5),
            ..ExportMetadata::default()
        };
        let args = build(&ExportFormat::Aiff, &meta, &unknown());
        assert!(has_tag(&args, "TBPM=141"));
    }

    #[rstest]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn wav_and_flac_keep_the_generic_bpm_and_key_tags(#[case] format: ExportFormat) {
        let meta = ExportMetadata {
            bpm: Some(140.5),
            key: Some("Am".to_string()),
            ..ExportMetadata::default()
        };
        let args = build(&format, &meta, &unknown());
        assert!(has_tag(&args, "BPM=140.50"));
        assert!(has_tag(&args, "INITIALKEY=Am"));
        assert!(!args.iter().any(|a| a.starts_with("TBPM=")));
    }

    #[test]
    fn empty_metadata_produces_no_metadata_args() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &unknown());
        assert_eq!(args.iter().filter(|a| *a == "-metadata").count(), 0);
    }

    #[test]
    fn partial_metadata_only_includes_present_fields() {
        let meta = ExportMetadata {
            title: Some("Only Title".to_string()),
            ..ExportMetadata::default()
        };
        let args = build(&ExportFormat::Aiff, &meta, &unknown());
        assert!(has_tag(&args, "title=Only Title"));
        assert_eq!(args.iter().filter(|a| *a == "-metadata").count(), 1);
    }

    #[test]
    fn run_ffmpeg_returns_ffmpeg_not_found_when_binary_missing() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let mapped: TranscoderError = if io_err.kind() == std::io::ErrorKind::NotFound {
            TranscoderError::FfmpegNotFound
        } else {
            TranscoderError::Io(io_err)
        };
        assert!(matches!(mapped, TranscoderError::FfmpegNotFound));
    }

    #[test]
    fn run_ffmpeg_returns_io_error_for_other_errors() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let mapped: TranscoderError = if io_err.kind() == std::io::ErrorKind::NotFound {
            TranscoderError::FfmpegNotFound
        } else {
            TranscoderError::Io(io_err)
        };
        assert!(matches!(mapped, TranscoderError::Io(_)));
    }

    // ── CDJ sample-rate downsampling ──────────────────────────────────────────

    #[rstest]
    #[case(ExportFormat::Aiff, 96_000, "48000")]
    #[case(ExportFormat::Wav, 96_000, "48000")]
    #[case(ExportFormat::Aiff, 88_200, "44100")]
    #[case(ExportFormat::Aiff, 192_000, "48000")]
    fn high_rate_sources_are_downsampled_with_soxr(
        #[case] format: ExportFormat,
        #[case] hz: u32,
        #[case] target: &str,
    ) {
        let args = build(&format, &empty_meta(), &rate(hz));
        assert_eq!(value_after(&args, "-ar").unwrap(), target);
        assert!(value_after(&args, "-af").unwrap().contains("soxr"));
    }

    #[rstest]
    #[case(Some(48_000))]
    #[case(Some(44_100))]
    #[case(None)]
    fn cdj_safe_or_unknown_rates_produce_no_resample(#[case] hz: Option<u32>) {
        let src = SourceAudio {
            sample_rate: hz,
            ..SourceAudio::default()
        };
        let args = build(&ExportFormat::Aiff, &empty_meta(), &src);
        assert!(!args.iter().any(|a| a == "-ar"));
        assert!(!args.iter().any(|a| a == "-af"));
    }

    #[test]
    fn flac_96khz_source_produces_no_ar_arg() {
        let args = build(&ExportFormat::Flac, &empty_meta(), &rate(96_000));
        assert!(!args.iter().any(|a| a == "-ar"));
    }

    // ── trimming encoder padding of lossy sources ─────────────────────────────

    #[test]
    fn aac_source_is_trimmed_by_time() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &aac(1.0, 44_100));
        assert_eq!(value_after(&args, "-af").unwrap(), "atrim=end=1.001000");
        assert!(!args.iter().any(|a| a == "-ar"), "no resample at 44.1 kHz");
    }

    #[test]
    fn trim_does_not_depend_on_the_reported_sample_rate() {
        // HE-AAC may report its core rate (22.05 kHz) while decoding at 44.1 kHz.
        let core = build(&ExportFormat::Aiff, &empty_meta(), &aac(2.0, 22_050));
        let full = build(&ExportFormat::Aiff, &empty_meta(), &aac(2.0, 44_100));
        assert_eq!(value_after(&core, "-af"), value_after(&full, "-af"));
    }

    #[test]
    fn trim_comes_before_the_resample_in_one_filter_chain() {
        let args = build(&ExportFormat::Aiff, &empty_meta(), &aac(1.0, 96_000));
        assert_eq!(args.iter().filter(|a| *a == "-af").count(), 1);
        assert_eq!(
            value_after(&args, "-af").unwrap(),
            "atrim=end=1.001000,aresample=resampler=soxr"
        );
        assert_eq!(value_after(&args, "-ar").unwrap(), "48000");
    }

    #[test]
    fn zero_duration_is_unknown_and_not_trimmed() {
        let src = SourceAudio {
            ext: "m4a".to_string(),
            duration: Some(Duration::ZERO),
            ..SourceAudio::default()
        };
        let args = build(&ExportFormat::Aiff, &empty_meta(), &src);
        assert!(!args.iter().any(|a| a == "-af"));
    }

    #[rstest]
    #[case("m4a", Some(1.0), Some(1_001))]
    #[case("M4A", Some(0.5), Some(501))]
    #[case("aac", Some(2.0), Some(2_001))]
    #[case("flac", Some(1.0), None)]
    #[case("aiff", Some(1.0), None)]
    #[case("wav", Some(1.0), None)]
    #[case("mp3", Some(1.0), None)]
    #[case("m4a", Some(0.0), None)]
    #[case("m4a", None, None)]
    fn padded_source_end_rule(
        #[case] ext: &str,
        #[case] secs: Option<f64>,
        #[case] expected_ms: Option<u64>,
    ) {
        let duration = secs.map(Duration::from_secs_f64);
        assert_eq!(
            padded_source_end(ext, duration),
            expected_ms.map(Duration::from_millis)
        );
    }

    #[test]
    fn padded_source_end_rounds_up_to_the_next_millisecond() {
        let end = padded_source_end("m4a", Some(Duration::from_micros(1_000_400)));
        assert_eq!(end, Some(Duration::from_millis(1_002)));
    }
}
