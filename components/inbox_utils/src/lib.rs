//! Shared inbox utilities for ZIP extraction, audio detection, and filename handling.

use std::path::{Path, PathBuf};

pub use audio_format::AudioFormat;

fn audio_format(path: &Path) -> Option<AudioFormat> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(AudioFormat::from_extension)
}

/// Check if a file has any recognized audio extension.
pub fn is_audio_file(path: &Path) -> bool {
    audio_format(path).is_some()
}

/// Check if a file has an ingestible audio extension (case-insensitive).
///
/// The policy is `AudioFormat::is_ingestible`, the single source of truth; nothing
/// here lists extensions.
pub fn is_ingestible_audio(path: &Path) -> bool {
    audio_format(path).is_some_and(|f| f.is_ingestible())
}

/// The extensions an upload accepts, for a file input's `accept` attribute: every
/// ingestible extension with its dot, and `.zip`.
pub fn upload_accept() -> Vec<String> {
    AudioFormat::ingestible_extensions()
        .into_iter()
        .map(|ext| format!(".{ext}"))
        .chain(std::iter::once(".zip".to_string()))
        .collect()
}

/// What an uploaded or downloaded file holds, by its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upload {
    /// A ZIP archive of audio files.
    Zip,
    /// A single audio file of this format.
    Audio(AudioFormat),
}

/// Detect what a file is by magic bytes: a ZIP archive, or audio as `audio_format::sniff`
/// (the one sniffer) identifies it. `None` for anything else, or if it cannot be read.
pub fn detect_file_type(path: &Path) -> Option<Upload> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_ok() && magic == *b"PK\x03\x04" {
        return Some(Upload::Zip);
    }
    file.rewind().ok()?;
    let audio_format::Sniffed::Format(format) = audio_format::sniff(&mut file, len).ok()? else {
        return None;
    };
    // The sniffer says "MP4 container"; uploads only take what ingest takes, and an
    // MP4 that is a picture or a video is not an `.m4a`.
    if !format.is_ingestible() {
        return None;
    }
    if format == AudioFormat::M4a {
        file.rewind().ok()?;
        let mut head = Vec::new();
        file.take(FTYP_READ_LIMIT).read_to_end(&mut head).ok()?;
        if !is_audio_mp4(&head) {
            return None;
        }
    }
    Some(Upload::Audio(format))
}

/// How much of the head is read to see the `ftyp` brands.
const FTYP_READ_LIMIT: u64 = 256;

/// Major brands of an MP4 that holds audio, or may (`isom`, `mp42`, ... name the
/// ISO base format, which carries audio or video; only the codec tells, and the
/// ingest reads that).
const AUDIO_MP4_BRANDS: [&[u8; 4]; 9] = [
    b"M4A ", b"M4B ", b"M4P ", b"mp41", b"mp42", b"isom", b"iso2", b"iso4", b"dash",
];

/// Brands that say audio outright, wherever they stand in the list.
const AUDIO_ONLY_BRANDS: [&[u8; 4]; 2] = [b"M4A ", b"M4B "];

/// Whether the `ftyp` box at the start of `head` is that of an audio MP4: its major
/// brand is an ISO/audio one, or an M4A/M4B brand is among the compatible ones. A HEIC
/// (`heic`, `mif1`), QuickTime (`qt  `) or 3GP (`3gp4`) file is not.
fn is_audio_mp4(head: &[u8]) -> bool {
    if head.len() < 16 || &head[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let end = size.clamp(16, head.len());
    let major = &head[8..12];
    AUDIO_MP4_BRANDS.iter().any(|b| major == &b[..])
        || head[16..end]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|c| AUDIO_ONLY_BRANDS.contains(&c))
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

        // Only extract ingestible audio files
        if !is_ingestible_audio(&entry_path) {
            tracing::debug!(path = %entry_path.display(), "Skipping non-ingestible file");
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
    #[case("track.m4a", true)]
    #[case("track.M4A", true)]
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
    #[case("track.m4a", true)]
    #[case("track.M4A", true)]
    #[case("track.aif", true)]
    #[case("track.AIF", true)]
    #[case("track.aiff", true)]
    #[case("track.AIFF", true)]
    #[case("cover.jpg", false)]
    #[case("noext", false)]
    fn ingestible_audio_follows_the_ingest_policy(#[case] filename: &str, #[case] expected: bool) {
        assert_eq!(is_ingestible_audio(Path::new(filename)), expected);
    }

    #[test]
    fn upload_accept_lists_the_ingestible_extensions_and_zip() {
        let accept = upload_accept();
        for ext in [".flac", ".mp3", ".wav", ".m4a", ".aiff", ".aif", ".zip"] {
            assert!(accept.iter().any(|a| a == ext), "{ext} missing: {accept:?}");
        }
        // Everything listed but `.zip` is ingestible per `is_ingestible_audio`.
        for ext in accept.iter().filter(|a| *a != ".zip") {
            assert!(is_ingestible_audio(Path::new(&format!("x{ext}"))));
        }
    }

    #[rstest]
    #[case(b"fLaC\x00\x00" as &[u8], Some(Upload::Audio(AudioFormat::Flac)))]
    #[case(b"\x50\x4B\x03\x04extra", Some(Upload::Zip))]
    #[case(b"\xFF\xFBdata\x00", Some(Upload::Audio(AudioFormat::Mp3)))]
    #[case(b"RIFF\x04\0\0\0WAVEfmt ", Some(Upload::Audio(AudioFormat::Wav)))]
    #[case(b"FORM\0\0\0\x04AIFF", Some(Upload::Audio(AudioFormat::Aiff)))]
    #[case(b"FORM\0\0\0\x04AIFCCOMM", Some(Upload::Audio(AudioFormat::Aiff)))]
    #[case(b"\0\0\0\x18ftypM4A \0\0\0\0", Some(Upload::Audio(AudioFormat::M4a)))]
    #[case(
        b"\0\0\0\x18ftypisom\0\0\0\0mp42",
        Some(Upload::Audio(AudioFormat::M4a))
    )]
    // a 3GP whose compatible brands say M4A is audio
    #[case(
        b"\0\0\0\x18ftyp3gp4\0\0\0\0M4A ",
        Some(Upload::Audio(AudioFormat::M4a))
    )]
    // MP4 containers that are not audio: HEIC, QuickTime MOV, 3GP
    #[case(b"\0\0\0\x18ftypheic\0\0\0\0mif1", None)]
    #[case(b"\0\0\0\x18ftypqt  \0\0\0\0qt  ", None)]
    #[case(b"\0\0\0\x18ftyp3gp4\x00\x00\x00\x003gp4", None)]
    // not audio: an 8SVX IFF, a non-WAVE RIFF, a bare ID3 stub, zeros
    #[case(b"FORM\0\0\0\x048SVXVHDR", None)]
    #[case(b"RIFF\x04\0\0\0AVI LIST", None)]
    #[case(b"\x49\x44\x33\x04data", None)]
    #[case(b"\x00\x00\x00\x00", None)]
    fn magic_byte_detection(#[case] bytes: &[u8], #[case] expected: Option<Upload>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_file");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(detect_file_type(&path), expected);
    }

    #[test]
    fn id3_prefixed_flac_and_mp3_are_told_apart() {
        // A 20-byte ID3v2.4 tag (10-byte header, 10 bytes of body), then the audio.
        let tag = |body: &[u8]| {
            let mut v = b"ID3\x04\0\0\0\0\0\x0a".to_vec();
            v.extend([0u8; 10]);
            v.extend(body);
            v
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_file");
        std::fs::write(&path, tag(b"fLaC\0\0\0\0")).unwrap();
        assert_eq!(
            detect_file_type(&path),
            Some(Upload::Audio(AudioFormat::Flac))
        );
        std::fs::write(&path, tag(&[0xFF, 0xFB, 0x90, 0x00])).unwrap();
        assert_eq!(
            detect_file_type(&path),
            Some(Upload::Audio(AudioFormat::Mp3))
        );
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
    fn extract_zip_extracts_audio_including_aiff_and_skips_the_rest() {
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

        zip_writer.start_file("track.m4a", options).unwrap();
        zip_writer
            .write_all(b"\x00\x00\x00\x20ftyp fake m4a data")
            .unwrap();

        zip_writer.start_file("track.aiff", options).unwrap();
        zip_writer.write_all(b"FORM fake aiff data").unwrap();

        zip_writer.start_file("Other.AIF", options).unwrap();
        zip_writer.write_all(b"FORM fake aiff data").unwrap();

        zip_writer.start_file("cover.jpg", options).unwrap();
        zip_writer.write_all(b"fake image data").unwrap();

        zip_writer.finish().unwrap();

        let extracted = extract_zip(&zip_path, &output_dir).unwrap();
        assert_eq!(extracted.len(), 5);
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
        assert!(names.contains(&"track.m4a".to_string()));
        assert!(names.contains(&"track.aiff".to_string()));
        assert!(names.contains(&"Other.AIF".to_string()));
    }
}
