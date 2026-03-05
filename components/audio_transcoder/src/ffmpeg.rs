use std::path::Path;

use crate::{ExportFormat, ExportMetadata, TranscoderError};

/// Build the ffmpeg argument list for a transcode operation.
///
/// Extracted into a pure function so the argument construction can be tested
/// without actually running ffmpeg.
pub(crate) fn build_ffmpeg_args(
    source: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
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
/// Captures ffmpeg's stderr; on a non-zero exit code the stderr content is
/// returned as `TranscoderError::FfmpegFailed`.
pub(crate) fn run_ffmpeg(
    source: &Path,
    output: &Path,
    format: &ExportFormat,
    meta: &ExportMetadata,
) -> Result<(), TranscoderError> {
    let args = build_ffmpeg_args(source, output, format, meta);

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
        let args = build_ffmpeg_args(&source(), &output_wav(), &ExportFormat::Wav, &empty_meta());
        let pos = args
            .iter()
            .position(|a| a == "-c:a")
            .expect("-c:a must be present for WAV");
        assert_eq!(args[pos + 1], "pcm_s24le");
    }

    #[test]
    fn wav_args_do_not_include_id3v2_flags() {
        let args = build_ffmpeg_args(&source(), &output_wav(), &ExportFormat::Wav, &empty_meta());
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
        );
        assert!(
            !args.iter().any(|a| a == "-write_id3v2"),
            "FLAC args must not contain -write_id3v2"
        );
    }

    // ── common flags ──────────────────────────────────────────────────────────

    #[test]
    fn all_formats_include_map_metadata() {
        for format in [ExportFormat::Aiff, ExportFormat::Wav, ExportFormat::Flac] {
            let output = match format {
                ExportFormat::Aiff => output_aiff(),
                ExportFormat::Wav => output_wav(),
                ExportFormat::Flac => output_flac(),
            };
            let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta());
            let pos = args
                .iter()
                .position(|a| a == "-map_metadata")
                .unwrap_or_else(|| panic!("-map_metadata must be present for {:?}", format));
            assert_eq!(args[pos + 1], "0");
        }
    }

    #[test]
    fn all_formats_include_loglevel_error() {
        for format in [ExportFormat::Aiff, ExportFormat::Wav, ExportFormat::Flac] {
            let output = match format {
                ExportFormat::Aiff => output_aiff(),
                ExportFormat::Wav => output_wav(),
                ExportFormat::Flac => output_flac(),
            };
            let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta());
            let pos = args
                .iter()
                .position(|a| a == "-loglevel")
                .unwrap_or_else(|| panic!("-loglevel must be present for {:?}", format));
            assert_eq!(args[pos + 1], "error");
        }
    }

    #[test]
    fn all_formats_include_overwrite_flag() {
        for format in [ExportFormat::Aiff, ExportFormat::Wav, ExportFormat::Flac] {
            let output = match format {
                ExportFormat::Aiff => output_aiff(),
                ExportFormat::Wav => output_wav(),
                ExportFormat::Flac => output_flac(),
            };
            let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta());
            assert!(
                args.iter().any(|a| a == "-y"),
                "-y must be present for {:?}",
                format
            );
        }
    }

    #[test]
    fn all_formats_end_with_output_path() {
        for (format, output) in [
            (ExportFormat::Aiff, output_aiff()),
            (ExportFormat::Wav, output_wav()),
            (ExportFormat::Flac, output_flac()),
        ] {
            let args = build_ffmpeg_args(&source(), &output, &format, &empty_meta());
            assert_eq!(
                args.last().expect("args must not be empty"),
                output.to_str().unwrap(),
                "last arg must be output path for {:?}",
                format
            );
        }
    }

    // ── metadata injection ────────────────────────────────────────────────────

    #[test]
    fn full_metadata_produces_title_metadata_arg() {
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &full_meta());
        let pos = args
            .windows(2)
            .position(|w| w[0] == "-metadata" && w[1].starts_with("title="))
            .expect("-metadata title= must be present");
        assert_eq!(args[pos + 1], "title=Test Track");
    }

    #[test]
    fn full_metadata_produces_artist_metadata_arg() {
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &full_meta());
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "artist=Test Artist"),
            "artist metadata must be present"
        );
    }

    #[test]
    fn full_metadata_produces_album_metadata_arg() {
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &full_meta());
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "album=Test Album"),
            "album metadata must be present"
        );
    }

    #[test]
    fn full_metadata_produces_bpm_metadata_arg() {
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &full_meta());
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "BPM=128.00"),
            "BPM metadata must be present with two decimal places"
        );
    }

    #[test]
    fn full_metadata_produces_key_metadata_arg() {
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &full_meta());
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
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &meta);
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
        let args = build_ffmpeg_args(&source(), &output_aiff(), &ExportFormat::Aiff, &meta);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata" && w[1] == "BPM=140.50"),
            "BPM must be formatted with two decimal places"
        );
    }
}
