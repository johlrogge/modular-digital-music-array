//! Editing text in the user's `$EDITOR` (falls back to vi).

use color_eyre::eyre::eyre;
use color_eyre::Result;
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
/// Fails when the file cannot be written or read back, the editor cannot be
/// launched, or it exits with a non-zero status.
pub(crate) fn edit_text(prefix: &str, initial: &str) -> Result<String> {
    let file = create_temp(prefix, initial).map_err(|e| eyre!("Failed to write temp file: {e}"))?;
    let path = file.path();

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| eyre!("Failed to launch editor '{editor}': {e}"))?;
    if !status.success() {
        return Err(eyre!("Editor exited with non-zero status"));
    }

    // Read by path: editors may replace the file, so the open handle can be stale.
    std::fs::read_to_string(path).map_err(|e| eyre!("Failed to read temp file: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, failure_text};

    /// Puts `$EDITOR` back to what it was when dropped, even if the test panics.
    struct RestoreEditor(Option<String>);

    impl Drop for RestoreEditor {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("EDITOR", value),
                None => std::env::remove_var("EDITOR"),
            }
        }
    }

    /// Run `f` with `$EDITOR` set to `editor`, restoring the previous value afterwards.
    fn with_editor<T>(editor: &str, f: impl FnOnce() -> T) -> T {
        let _lock = env_lock();
        let _restore = RestoreEditor(std::env::var("EDITOR").ok());
        std::env::set_var("EDITOR", editor);
        f()
    }

    #[test]
    fn editor_failing_is_an_error() {
        let result = with_editor("false", || edit_text("mdma-test-", "x"));
        assert_eq!(failure_text(result), "Editor exited with non-zero status");
    }

    #[test]
    fn editor_that_cannot_launch_is_an_error() {
        let result = with_editor("mdma-no-such-editor", || edit_text("mdma-test-", "x"));
        let text = failure_text(result);
        assert!(
            text.starts_with("Failed to launch editor 'mdma-no-such-editor': "),
            "{text}"
        );
    }

    #[test]
    fn unchanged_text_comes_back_when_the_editor_succeeds() {
        let result = with_editor("true", || edit_text("mdma-test-", "hello\n"));
        assert_eq!(result.unwrap(), "hello\n");
    }

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
