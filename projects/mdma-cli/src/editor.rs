//! Editing text in the user's `$EDITOR` (falls back to vi).

use std::path::Path;

/// Write `initial` to `path`, open it in `$EDITOR`, and return the saved text.
///
/// Exits 1 when the file cannot be written or read back, the editor cannot be
/// launched, or it exits with a non-zero status. The file is left in place;
/// the caller removes it when done.
pub(crate) fn edit_text(path: &Path, initial: &str) -> String {
    std::fs::write(path, initial).unwrap_or_else(|e| {
        eprintln!("Failed to write temp file: {}", e);
        std::process::exit(1);
    });

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

    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read temp file: {}", e);
        std::process::exit(1);
    })
}
