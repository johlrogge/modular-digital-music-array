//! Editing text in the user's `$EDITOR` (falls back to vi).

use std::io::Write;

const SUFFIX: &str = ".plist";

/// Create a uniquely named temp file `<prefix><random>.plist` containing `initial`.
/// The file is removed when the returned handle is dropped.
fn create_temp(prefix: &str, initial: &str) -> std::io::Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(SUFFIX)
        .tempfile()?;
    file.write_all(initial.as_bytes())?;
    file.flush()?;
    Ok(file)
}

/// Write `initial` to a unique temp file named `<prefix><random>.plist`, open it
/// in `$EDITOR`, and return the saved text. The file is removed afterwards.
///
/// Exits 1 when the file cannot be written or read back, the editor cannot be
/// launched, or it exits with a non-zero status.
pub(crate) fn edit_text(prefix: &str, initial: &str) -> String {
    let file = create_temp(prefix, initial).unwrap_or_else(|e| {
        eprintln!("Failed to write temp file: {}", e);
        std::process::exit(1);
    });
    let path = file.path();

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to launch editor '{}': {}", editor, e);
            std::process::exit(1);
        });
    if !status.success() {
        eprintln!("Editor exited with non-zero status");
        std::process::exit(1);
    }

    // Read by path: editors may replace the file, so the open handle can be stale.
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read temp file: {}", e);
        std::process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_temp_files_get_distinct_paths() {
        let a = create_temp("mdma-queue-", "a").unwrap();
        let b = create_temp("mdma-queue-", "b").unwrap();
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn temp_file_has_prefix_suffix_and_initial_content() {
        let f = create_temp("mdma-queue-", "hello").unwrap();
        let name = f.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("mdma-queue-"), "{name}");
        assert!(name.ends_with(".plist"), "{name}");
        assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "hello");
    }

    #[test]
    fn temp_file_is_removed_on_drop() {
        let f = create_temp("mdma-queue-", "x").unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        assert!(!path.exists());
    }
}
