// components/media_downloader/src/utils.rs
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Sanitize a filename to be safe for all filesystems
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            // Replace invalid characters with underscores
            ' ' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// Generate a stable filename for a track
pub fn generate_filename(title: &str, url: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let hash = hex::encode(&hasher.finalize()[..8]);

    PathBuf::from(format!("{}-{}.flac", sanitize_filename(title), hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(
            sanitize_filename(r#"a test/file:with*invalid?chars"#),
            "a_test_file_with_invalid_chars"
        );
    }

    #[test]
    fn test_generate_filename() {
        let title = "Test Song";
        let url = "https://example.com/song";
        let filename = generate_filename(title, url);

        let filename_str = filename.to_string_lossy();
        assert!(
            filename_str.ends_with(".flac"),
            "filename '{}' should end with .flac",
            filename_str
        );
        assert!(
            filename_str.contains("Test_Song"),
            "filename '{}' should contain 'Test_Song'",
            filename_str
        );
    }
}
