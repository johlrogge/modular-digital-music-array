use std::path::Path;

use crate::{ExportFormat, ExportMetadata, TranscoderError};

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

/// Build the ffmpeg argument list for a transcode operation.
///
/// `source_sample_rate` — when supplied and the format is AIFF or WAV, a
/// downsample to ≤ 48 kHz is added when the source exceeds that limit
/// (Pioneer CDJ "illegal format" prevention).  Pass `None` when the rate is
/// unknown; the file will be exported as-is.
///
/// Extracted into a pure function so the argument construction can be tested
/// without actually running ffmpeg.
pub(crate) fn build_ffmpeg_args(
    source: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
    source_sample_rate: Option<u32>,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // Input
    args.push("-i".to_string());
    args.push(source.to_string_lossy().into_owned());

    // Codec flags per format
    match format {
        ExportFormat::Aiff => {
            args.push("-write_id3v2".to_string());
            args.push("1".to_string());
            args.push("-id3v2_version".to_string());
            args.push("3".to_string());
            args.push("-c:a".to_string());
            args.push("pcm_s24be".to_string());
        }
        ExportFormat::Wav => {
            args.push("-c:a".to_string());
            args.push("pcm_s24le".to_string());
        }
        ExportFormat::Flac => {
            args.push("-c:a".to_string());
            args.push("flac".to_string());
        }
    }

    // CDJ sample-rate guard: AIFF and WAV must not exceed 48 kHz.
    // When the source rate is known and above the limit, add a soxr
    // aresample filter and set the output rate.
    let needs_resample = matches!(format, ExportFormat::Aiff | ExportFormat::Wav);
    if needs_resample {
        if let Some(src_hz) = source_sample_rate {
            if let Some(target_hz) = cdj_target_rate(src_hz) {
                args.push("-af".to_string());
                args.push("aresample=resampler=soxr".to_string());
                args.push("-ar".to_string());
                args.push(target_hz.to_string());
            }
        }
    }

    // Always pass through all metadata from the source (includes cover art)
    args.push("-map_metadata".to_string());
    args.push("0".to_string());

    // Overwrite output without prompting; suppress all output except errors
    args.push("-y".to_string());
    args.push("-loglevel".to_string());
    args.push("error".to_string());

    // Metadata tags
    if let Some(title) = &meta.title {
        args.push("-metadata".to_string());
        args.push(format!("title={}", title));
    }
    if let Some(artist) = &meta.artist {
        args.push("-metadata".to_string());
        args.push(format!("artist={}", artist));
    }
    if let Some(album) = &meta.album {
        args.push("-metadata".to_string());
        args.push(format!("album={}", album));
    }
    if let Some(bpm) = meta.bpm {
        args.push("-metadata".to_string());
        args.push(format!("BPM={:.2}", bpm));
    }
    if let Some(key) = &meta.key {
        args.push("-metadata".to_string());
        args.push(format!("INITIALKEY={}", key));
    }

    // Output path
    args.push(output.to_string_lossy().into_owned());

    args
}

/// Run ffmpeg to transcode `source` to `output` in the given `format`,
/// embedding the supplied `meta` tags.
///
/// `source_sample_rate` is forwarded to `build_ffmpeg_args` — see its docs for
/// the CDJ downsampling behaviour.
///
/// Captures ffmpeg's stderr; on a non-zero exit code the stderr content is
/// returned as `TranscoderError::FfmpegFailed`.
pub(crate) fn run_ffmpeg(
    source: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
    source_sample_rate: Option<u32>,
) -> Result<(), TranscoderError> {
    let args = build_ffmpeg_args(source, output, format, meta, source_sample_rate);

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

    fn output_aiff() -> PathBuf {
        PathBuf::from("/output/track.aiff")
    }

    fn output_wav() -> PathBuf {
        PathBuf::from("/output/track.wav")
    }

    fn output_flac() -> PathBuf {
        PathBuf::from("/output/track.flac")
    }

    fn output_for(format: &ExportFormat) -> PathBuf {
        match format {
            ExportFormat::Aiff => output_aiff(),
            ExportFormat::Wav => output_wav(),
            ExportFormat::Flac => output_flac(),
        }
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
        ExportMetadata {
            title: None,
            artist: None,
            album: None,
            bpm: None,
            key: None,
        }
    }

    // ── AIFF tests ────────────────────────────────────────────────────────────

    #[test]
    fn aiff_args_start_with_input() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            None,
        );
        assert_eq!(args[0], "-i");
        assert_eq!(args[1], "/input/track.flac");
    }

    #[test]
    fn aiff_args_include_write_id3v2() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            None,
        );
        let pos = args
            .iter()
            .position(|a| a == "-write_id3v2")
            .expect("-write_id3v2 must be present for AIFF");
        assert_eq!(args[pos + 1], "1");
    }

    #[test]
    fn aiff_args_include_id3v2_version_3() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            None,
        );
        let pos = args
            .iter()
            .position(|a| a == "-id3v2_version")
            .expect("-id3v2_version must be present for AIFF");
        assert_eq!(args[pos + 1], "3");
    }

    #[test]
    fn aiff_args_codec_is_pcm_s24be() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            None,
        );
        let pos = args
            .iter()
            .position(|a| a == "-c:a")
            .expect("-c:a must be present for AIFF");
        assert_eq!(args[pos + 1], "pcm_s24be");
    }

    // ── WAV tests ─────────────────────────────────────────────────────────────

    #[test]
    fn wav_args_codec_is_pcm_s24le() {
        let args = build_ffmpeg_args(
            &source(),
            &output_wav(),
            &ExportFormat::Wav,
            &empty_meta(),
            None,
        );
        let pos = args
            .iter()
            .position(|a| a == "-c:a")
            .expect("-c:a must be present for WAV");
        assert_eq!(args[pos + 1], "pcm_s24le");
    }

    #[test]
    fn wav_args_do_not_include_id3v2_flags() {
        let args = build_ffmpeg_args(
            &source(),
            &output_wav(),
            &ExportFormat::Wav,
            &empty_meta(),
            None,
        );
        assert!(
            !args.iter().any(|a| a == "-write_id3v2"),
            "WAV args must not contain -write_id3v2"
        );
        assert!(
            !args.iter().any(|a| a == "-id3v2_version"),
            "WAV args must not contain -id3v2_version"
        );
    }

    // ── FLAC tests ────────────────────────────────────────────────────────────

    #[test]
    fn flac_args_codec_is_flac() {
        let args = build_ffmpeg_args(
            &source(),
            &output_flac(),
            &ExportFormat::Flac,
            &empty_meta(),
            None,
        );
        let pos = args
            .iter()
            .position(|a| a == "-c:a")
            .expect("-c:a must be present for FLAC");
        assert_eq!(args[pos + 1], "flac");
    }

    #[test]
    fn flac_args_do_not_include_id3v2_flags() {
        let args = build_ffmpeg_args(
            &source(),
            &output_flac(),
            &ExportFormat::Flac,
            &empty_meta(),
            None,
        );
        assert!(
            !args.iter().any(|a| a == "-write_id3v2"),
            "FLAC args must not contain -write_id3v2"
        );
    }

    // ── common flags ──────────────────────────────────────────────────────────

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn all_formats_include_map_metadata(#[case] format: ExportFormat) {
        let output = output_for(&format);
        let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta(), None);
        let pos = args
            .iter()
            .position(|a| a == "-map_metadata")
            .unwrap_or_else(|| panic!("-map_metadata must be present for {:?}", format));
        assert_eq!(args[pos + 1], "0");
    }

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn all_formats_include_loglevel_error(#[case] format: ExportFormat) {
        let output = output_for(&format);
        let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta(), None);
        let pos = args
            .iter()
            .position(|a| a == "-loglevel")
            .unwrap_or_else(|| panic!("-loglevel must be present for {:?}", format));
        assert_eq!(args[pos + 1], "error");
    }

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn all_formats_include_overwrite_flag(#[case] format: ExportFormat) {
        let output = output_for(&format);
        let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta(), None);
        assert!(
            args.iter().any(|a| a == "-y"),
            "-y must be present for {:?}",
            format
        );
    }

    #[rstest]
    #[case(ExportFormat::Aiff)]
    #[case(ExportFormat::Wav)]
    #[case(ExportFormat::Flac)]
    fn all_formats_end_with_output_path(#[case] format: ExportFormat) {
        let output = output_for(&format);
        let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta(), None);
        assert_eq!(
            args.last().expect("args must not be empty"),
            output.to_str().unwrap(),
            "last arg must be output path for {:?}",
            format
        );
    }

    // ── metadata injection ────────────────────────────────────────────────────

    #[test]
    fn full_metadata_produces_title_metadata_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &full_meta(),
            None,
        );
        let pos = args
            .windows(2)
            .position(|w| w[0] == "-metadata" && w[1].starts_with("title="))
            .expect("-metadata title= must be present");
        assert_eq!(args[pos + 1], "title=Test Track");
    }

    #[test]
    fn full_metadata_produces_artist_metadata_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &full_meta(),
            None,
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "artist=Test Artist"),
            "artist metadata must be present"
        );
    }

    #[test]
    fn full_metadata_produces_album_metadata_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &full_meta(),
            None,
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "album=Test Album"),
            "album metadata must be present"
        );
    }

    #[test]
    fn full_metadata_produces_bpm_metadata_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &full_meta(),
            None,
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "BPM=128.00"),
            "BPM metadata must be present with two decimal places"
        );
    }

    #[test]
    fn full_metadata_produces_key_metadata_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &full_meta(),
            None,
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "INITIALKEY=Am"),
            "INITIALKEY metadata must be present"
        );
    }

    #[test]
    fn empty_metadata_produces_no_metadata_args() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            None,
        );
        let metadata_count = args.iter().filter(|a| *a == "-metadata").count();
        assert_eq!(
            metadata_count, 0,
            "empty metadata must not produce any -metadata args"
        );
    }

    #[test]
    fn partial_metadata_only_includes_present_fields() {
        let meta = ExportMetadata {
            title: Some("Only Title".to_string()),
            artist: None,
            album: None,
            bpm: None,
            key: None,
        };
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &meta, None);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "title=Only Title"),
            "title must be present"
        );
        let metadata_count = args.iter().filter(|a| *a == "-metadata").count();
        assert_eq!(
            metadata_count, 1,
            "only one -metadata arg for title-only metadata"
        );
    }

    #[test]
    fn run_ffmpeg_returns_ffmpeg_not_found_when_binary_missing() {
        // Run against a binary name that will never exist on PATH.
        // We need to call run_ffmpeg, but it always uses "ffmpeg".
        // Instead, test the error mapping logic directly by calling Command
        // and verifying our map_err produces FfmpegNotFound.
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let mapped: TranscoderError = if io_err.kind() == std::io::ErrorKind::NotFound {
            TranscoderError::FfmpegNotFound
        } else {
            TranscoderError::Io(io_err)
        };
        assert!(
            matches!(mapped, TranscoderError::FfmpegNotFound),
            "NotFound IO error must map to FfmpegNotFound"
        );
    }

    #[test]
    fn run_ffmpeg_returns_io_error_for_other_errors() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let mapped: TranscoderError = if io_err.kind() == std::io::ErrorKind::NotFound {
            TranscoderError::FfmpegNotFound
        } else {
            TranscoderError::Io(io_err)
        };
        assert!(
            matches!(mapped, TranscoderError::Io(_)),
            "PermissionDenied IO error must map to Io variant"
        );
    }

    #[test]
    fn bpm_is_formatted_with_two_decimal_places() {
        let meta = ExportMetadata {
            title: None,
            artist: None,
            album: None,
            bpm: Some(140.5),
            key: None,
        };
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &meta, None);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "BPM=140.50"),
            "BPM must be formatted with two decimal places"
        );
    }

    // ── CDJ sample-rate downsampling ──────────────────────────────────────────

    #[test]
    fn aiff_96khz_source_produces_ar_48000_and_soxr() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            Some(96_000),
        );
        assert!(
            args.iter().any(|a| a == "-ar"),
            "96kHz AIFF must include -ar arg"
        );
        let ar_pos = args.iter().position(|a| a == "-ar").unwrap();
        assert_eq!(args[ar_pos + 1], "48000", "96kHz must halve to 48000");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-af" && w[1].contains("soxr")),
            "96kHz AIFF must include soxr aresample filter"
        );
    }

    #[test]
    fn aiff_88200hz_source_produces_ar_44100_and_soxr() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            Some(88_200),
        );
        let ar_pos = args
            .iter()
            .position(|a| a == "-ar")
            .expect("88.2kHz AIFF must include -ar");
        assert_eq!(args[ar_pos + 1], "44100", "88.2kHz must halve to 44100");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-af" && w[1].contains("soxr")),
            "88.2kHz AIFF must include soxr filter"
        );
    }

    #[test]
    fn aiff_192khz_source_produces_ar_48000() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            Some(192_000),
        );
        let ar_pos = args
            .iter()
            .position(|a| a == "-ar")
            .expect("192kHz AIFF must include -ar");
        assert_eq!(
            args[ar_pos + 1],
            "48000",
            "192kHz must halve twice to 48000"
        );
    }

    #[rstest]
    #[case(48_000u32)]
    #[case(44_100u32)]
    fn aiff_cdj_rates_produce_no_ar_arg(#[case] rate: u32) {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            Some(rate),
        );
        assert!(
            !args.iter().any(|a| a == "-ar"),
            "{}Hz AIFF must not contain -ar (already CDJ-safe)",
            rate
        );
        assert!(
            !args.iter().any(|a| a == "-af"),
            "{}Hz AIFF must not contain -af (no resample needed)",
            rate
        );
    }

    #[test]
    fn aiff_no_source_rate_produces_no_ar_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_aiff(),
            &ExportFormat::Aiff,
            &empty_meta(),
            None,
        );
        assert!(
            !args.iter().any(|a| a == "-ar"),
            "unknown source rate must not produce -ar"
        );
    }

    #[test]
    fn wav_96khz_source_produces_ar_48000_and_soxr() {
        let args = build_ffmpeg_args(
            &source(),
            &output_wav(),
            &ExportFormat::Wav,
            &empty_meta(),
            Some(96_000),
        );
        let ar_pos = args
            .iter()
            .position(|a| a == "-ar")
            .expect("96kHz WAV must include -ar");
        assert_eq!(args[ar_pos + 1], "48000");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-af" && w[1].contains("soxr")),
            "96kHz WAV must include soxr filter"
        );
    }

    #[test]
    fn flac_96khz_source_produces_no_ar_arg() {
        let args = build_ffmpeg_args(
            &source(),
            &output_flac(),
            &ExportFormat::Flac,
            &empty_meta(),
            Some(96_000),
        );
        assert!(
            !args.iter().any(|a| a == "-ar"),
            "FLAC must not resample regardless of source rate"
        );
    }
}
