//! Shared inbox utilities for ZIP extraction, audio detection, and filename handling.

use std::path::{Path, PathBuf};

/// All recognized audio file extensions (used for ZIP extraction filtering).
pub const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "wav", "aif", "aiff"];

/// Ingestible audio file extensions — FLAC, MP3, and WAV are accepted for library ingest.
///
/// WAV is ingestible (no embedded tags expected; metadata derived from filename if absent).
/// This is derived from the same policy as `AudioFormat::is_ingestible()` in `library_service`.
/// When adding a new ingestible format, update both this list and that method.
pub const INGEST_EXTENSIONS: &[&str] = &["flac", "mp3", "wav"];

/// Human-readable error message for non-ingestible audio files (used in HTTP responses).
///
/// Centralised here so the console and any other HTTP layer show consistent messaging.
pub const NON_INGESTIBLE_ERROR: &str = "Unsupported format: only FLAC, MP3, and WAV accepted";

/// Check if a file has any recognized audio extension.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Check if a file has an ingestible audio extension (FLAC, MP3, or WAV).
///
/// AIFF is recognized as audio but is an export-only format and must not be ingested.
/// WAV is ingestible (no embedded tags expected; metadata derived from filename if absent).
pub fn is_ingestible_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| INGEST_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Detect file type by magic bytes.
pub fn detect_file_type(path: &Path) -> Option<&'static str> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).ok()?;

    match &magic {
        // ZIP: PK\x03\x04
        [0x50, 0x4B, 0x03, 0x04] => Some("zip"),
        // FLAC: fLaC
        [0x66, 0x4C, 0x61, 0x43] => Some("flac"),
        // MP3: ID3 or \xFF\xFB
        [0x49, 0x44, 0x33, _] => Some("mp3"),
        [0xFF, 0xFB, _, _] => Some("mp3"),
        // WAV: RIFF
        [0x52, 0x49, 0x46, 0x46] => Some("wav"),
        // AIFF: FORM
        [0x46, 0x4F, 0x52, 0x4D] => Some("aiff"),
        _ => None,
    }
}

/// Sanitize a string for use in filenames.
pub fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Generate a unique path, adding a numeric suffix if a file already exists.
pub fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let dest_path = dir.join(filename);
    if !dest_path.exists() {
        return dest_path;
    }

    let path = Path::new(filename);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("flac");

    let mut counter = 1;
    loop {
        let new_name = format!("{}_{}.{}", stem, counter, ext);
        let new_path = dir.join(&new_name);
        if !new_path.exists() {
            return new_path;
        }
        counter += 1;
    }
}

/// Extract audio files from a ZIP archive to the output directory.
/// Returns the list of extracted file paths.
pub fn extract_zip(zip_path: &Path, output_dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut extracted = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Skip directories
        if entry.is_dir() {
            continue;
        }

        let entry_path = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue, // Skip entries with invalid paths
        };

        // Only extract ingestible audio files (FLAC and MP3)
        if !is_ingestible_audio(&entry_path) {
            tracing::debug!(path = %entry_path.display(), "Skipping non-ingestible file (only FLAC, MP3, and WAV accepted)");
            continue;
        }

        // Use just the filename, not the full path from the ZIP
        let filename = match entry_path.file_name().and_then(|f| f.to_str()) {
            Some(f) => f.to_string(),
            None => continue,
        };

        let final_path = unique_path(output_dir, &filename);

        // Extract the file
        let mut outfile = std::fs::File::create(&final_path)?;
        std::io::copy(&mut entry, &mut outfile)?;

        tracing::info!(
            source = %entry_path.display(),
            dest = %final_path.display(),
            "Extracted audio file"
        );
        extracted.push(final_path);
    }

    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use std::io::Write;

    #[rstest]
    #[case("track.flac", true)]
    #[case("track.FLAC", true)]
    #[case("track.mp3", true)]
    #[case("track.wav", true)]
    #[case("track.aif", true)]
    #[case("track.aiff", true)]
    #[case("cover.jpg", false)]
    #[case("notes.txt", false)]
    #[case("noext", false)]
    fn audio_extension_recognition(#[case] filename: &str, #[case] expected: bool) {
        assert_eq!(is_audio_file(Path::new(filename)), expected);
    }

    #[rstest]
    #[case("track.flac", true)]
    #[case("track.FLAC", true)]
    #[case("track.mp3", true)]
    #[case("track.MP3", true)]
    #[case("track.wav", true)]
    #[case("track.WAV", true)]
    #[case("track.aif", false)]
    #[case("track.aiff", false)]
    #[case("cover.jpg", false)]
    #[case("noext", false)]
    fn ingestible_audio_flac_mp3_and_wav(#[case] filename: &str, #[case] expected: bool) {
        assert_eq!(is_ingestible_audio(Path::new(filename)), expected);
    }

    #[rstest]
    #[case(b"fLaC\x00\x00" as &[u8], Some("flac"))]
    #[case(b"\x50\x4B\x03\x04extra", Some("zip"))]
    #[case(b"\x49\x44\x33\x04data", Some("mp3"))]
    #[case(b"\xFF\xFBdata\x00", Some("mp3"))]
    #[case(b"RIFFdata", Some("wav"))]
    #[case(b"FORMdata", Some("aiff"))]
    #[case(b"\x00\x00\x00\x00", None)]
    fn magic_byte_detection(#[case] bytes: &[u8], #[case] expected: Option<&'static str>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_file");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(detect_file_type(&path), expected);
    }

    #[rstest]
    #[case("a/b\\c:d", "a_b_c_d")]
    #[case("ok name", "ok name")]
    #[case("a*b?c\"d<e>f|g", "a_b_c_d_e_f_g")]
    fn sanitize_special_chars(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(sanitize_filename(input), expected);
    }

    #[test]
    fn unique_path_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let result = unique_path(dir.path(), "track.flac");
        assert_eq!(result, dir.path().join("track.flac"));
    }

    #[test]
    fn unique_path_with_collision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track.flac"), b"").unwrap();
        let result = unique_path(dir.path(), "track.flac");
        assert_eq!(result, dir.path().join("track_1.flac"));
    }

    #[test]
    fn unique_path_multiple_collisions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track.flac"), b"").unwrap();
        std::fs::write(dir.path().join("track_1.flac"), b"").unwrap();
        let result = unique_path(dir.path(), "track.flac");
        assert_eq!(result, dir.path().join("track_2.flac"));
    }

    #[test]
    fn extract_zip_filters_audio_only() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("test.zip");
        let output_dir = dir.path().join("output");
        std::fs::create_dir(&output_dir).unwrap();

        // Create a ZIP with one audio file and one non-audio file
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);

        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer.start_file("track.flac", options).unwrap();
        // Write FLAC-like content
        zip_writer.write_all(b"fLaC fake audio data").unwrap();

        zip_writer.start_file("cover.jpg", options).unwrap();
        zip_writer.write_all(b"fake image data").unwrap();

        zip_writer
            .start_file("subfolder/other.mp3", options)
            .unwrap();
        zip_writer.write_all(b"ID3 fake mp3 data").unwrap();

        zip_writer.finish().unwrap();

        let extracted = extract_zip(&zip_path, &output_dir).unwrap();
        assert_eq!(extracted.len(), 2);

        // Should have extracted track.flac and other.mp3 (flattened from subfolder)
        let names: Vec<String> = extracted
            .iter()
            .filter_map(|p| {
                p.file_name()
                    .and_then(|f| f.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        assert!(names.contains(&"track.flac".to_string()));
        assert!(names.contains(&"other.mp3".to_string()));
    }

    #[test]
    fn extract_zip_extracts_wav_skips_aiff() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("test.zip");
        let output_dir = dir.path().join("output");
        std::fs::create_dir(&output_dir).unwrap();

        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer.start_file("track.flac", options).unwrap();
        zip_writer.write_all(b"fLaC fake audio data").unwrap();

        zip_writer.start_file("track.wav", options).unwrap();
        zip_writer.write_all(b"RIFF fake wav data").unwrap();

        zip_writer.start_file("track.aiff", options).unwrap();
        zip_writer.write_all(b"FORM fake aiff data").unwrap();

        zip_writer.finish().unwrap();

        let extracted = extract_zip(&zip_path, &output_dir).unwrap();
        // FLAC and WAV should be extracted; AIFF is still export-only
        assert_eq!(extracted.len(), 2);
        let names: Vec<String> = extracted
            .iter()
            .filter_map(|p| {
                p.file_name()
                    .and_then(|f| f.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        assert!(names.contains(&"track.flac".to_string()));
        assert!(names.contains(&"track.wav".to_string()));
    }
}
