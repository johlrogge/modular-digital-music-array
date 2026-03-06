//! Shared inbox utilities for ZIP extraction, audio detection, and filename handling.

use std::path::{Path, PathBuf};

/// All recognized audio file extensions (used for ZIP extraction filtering).
pub const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "wav", "aif", "aiff"];

/// Ingestible audio file extensions — only FLAC and MP3 are accepted for library ingest.
///
/// This is derived from the same policy as `AudioFormat::is_ingestible()` in `library_service`.
/// When adding a new ingestible format, update both this list and that method.
pub const INGEST_EXTENSIONS: &[&str] = &["flac", "mp3"];

/// Human-readable error message for non-ingestible audio files (used in HTTP responses).
///
/// Centralised here so the console and any other HTTP layer show consistent messaging.
pub const NON_INGESTIBLE_ERROR: &str = "Unsupported format: only FLAC and MP3 accepted";

/// Check if a file has any recognized audio extension.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Check if a file has an ingestible audio extension (FLAC or MP3 only).
///
/// WAV and AIFF are recognized as audio but are export-only formats and must not be ingested.
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
            tracing::debug!(path = %entry_path.display(), "Skipping non-ingestible file (only FLAC and MP3 accepted)");
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
    use std::io::Write;

    #[test]
    fn audio_extension_recognition() {
        assert!(is_audio_file(Path::new("track.flac")));
        assert!(is_audio_file(Path::new("track.FLAC")));
        assert!(is_audio_file(Path::new("track.mp3")));
        assert!(is_audio_file(Path::new("track.wav")));
        assert!(is_audio_file(Path::new("track.aif")));
        assert!(is_audio_file(Path::new("track.aiff")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
        assert!(!is_audio_file(Path::new("notes.txt")));
        assert!(!is_audio_file(Path::new("noext")));
    }

    #[test]
    fn ingestible_audio_only_flac_and_mp3() {
        assert!(is_ingestible_audio(Path::new("track.flac")));
        assert!(is_ingestible_audio(Path::new("track.FLAC")));
        assert!(is_ingestible_audio(Path::new("track.mp3")));
        assert!(is_ingestible_audio(Path::new("track.MP3")));
        // WAV and AIFF are export-only, not ingestible
        assert!(!is_ingestible_audio(Path::new("track.wav")));
        assert!(!is_ingestible_audio(Path::new("track.aif")));
        assert!(!is_ingestible_audio(Path::new("track.aiff")));
        assert!(!is_ingestible_audio(Path::new("cover.jpg")));
        assert!(!is_ingestible_audio(Path::new("noext")));
    }

    #[test]
    fn detect_file_type_flac_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.flac");
        std::fs::write(&path, b"fLaC\x00\x00").unwrap();
        assert_eq!(detect_file_type(&path), Some("flac"));
    }

    #[test]
    fn detect_file_type_zip_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.zip");
        std::fs::write(&path, b"\x50\x4B\x03\x04extra").unwrap();
        assert_eq!(detect_file_type(&path), Some("zip"));
    }

    #[test]
    fn detect_file_type_mp3_id3_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mp3");
        std::fs::write(&path, b"\x49\x44\x33\x04data").unwrap();
        assert_eq!(detect_file_type(&path), Some("mp3"));
    }

    #[test]
    fn detect_file_type_wav_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.wav");
        std::fs::write(&path, b"RIFFdata").unwrap();
        assert_eq!(detect_file_type(&path), Some("wav"));
    }

    #[test]
    fn detect_file_type_aiff_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aiff");
        std::fs::write(&path, b"FORMdata").unwrap();
        assert_eq!(detect_file_type(&path), Some("aiff"));
    }

    #[test]
    fn detect_file_type_unknown_magic_bytes_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown");
        std::fs::write(&path, b"\x00\x00\x00\x00").unwrap();
        assert_eq!(detect_file_type(&path), None);
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_filename("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize_filename("ok name"), "ok name");
        assert_eq!(sanitize_filename("a*b?c\"d<e>f|g"), "a_b_c_d_e_f_g");
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
    fn extract_zip_skips_wav_and_aiff() {
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
        // Only FLAC should be extracted; WAV and AIFF are skipped
        assert_eq!(extracted.len(), 1);
        let name = extracted[0]
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");
        assert_eq!(name, "track.flac");
    }
}
