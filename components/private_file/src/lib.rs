//! Atomic file writes with an explicit mode.
//!
//! Used for credentials (cookies, wifi PSKs) that must never be briefly
//! readable under the process umask and must never be observed half-written.

use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Write `contents` to `path` atomically with permission bits `mode`.
///
/// The data goes to a uniquely named sibling temp file (`.<name>.<random>.tmp`,
/// created exclusively with `mode` applied exactly, regardless of umask), is
/// fsynced, and is renamed over `path`. Concurrent writers each use their own
/// temp file, so the final content is always one writer's complete payload.
/// A symlink at `path` itself is replaced, not followed; symlinks in parent
/// directories are followed. The parent directory is fsynced best-effort. The
/// temp file is removed on error.
pub fn write_file_atomic(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        )
    })?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };

    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{}.", name.to_string_lossy()))
        .suffix(".tmp")
        .permissions(std::fs::Permissions::from_mode(mode))
        .tempfile_in(parent)?;
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;

    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Write `contents` to `path` atomically with mode 0600 (owner only).
pub fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_file_atomic(path, contents, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn fresh_file_gets_requested_mode_and_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        write_file_atomic(&path, b"hello", 0o640).expect("write");
        assert_eq!(mode_of(&path), 0o640);
        assert_eq!(std::fs::read(&path).expect("read"), b"hello");
    }

    #[test]
    fn existing_0644_file_becomes_0600_with_new_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, "old\n".repeat(200)).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        write_private_file(&path, b"new").expect("write");

        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
    }

    #[test]
    fn symlink_at_target_is_replaced_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "untouched").expect("write");
        let path = dir.path().join("link");
        symlink(&victim, &path).expect("symlink");

        write_private_file(&path, b"secret").expect("write");

        assert_eq!(std::fs::read(&victim).expect("read"), b"untouched");
        let meta = std::fs::symlink_metadata(&path).expect("meta");
        assert!(meta.file_type().is_file());
        assert_eq!(std::fs::read(&path).expect("read"), b"secret");
    }

    #[test]
    fn stale_temp_file_does_not_block_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        let stale = dir.path().join(".f.tmp");
        std::fs::write(&stale, "stale").expect("write");

        write_private_file(&path, b"fresh").expect("write");

        assert_eq!(std::fs::read(&path).expect("read"), b"fresh");
        assert_eq!(mode_of(&path), 0o600);
        // The stale file is not ours; it is neither used nor required to go.
        assert_eq!(std::fs::read(&stale).expect("read"), b"stale");
    }

    #[test]
    fn concurrent_writers_leave_one_complete_payload_and_no_temp_files() {
        const WRITERS: u8 = 4;
        const WRITES: usize = 25;
        const SIZE: usize = 64 * 1024;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");

        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let path = &path;
                s.spawn(move || {
                    let payload = vec![b'a' + w; SIZE];
                    for _ in 0..WRITES {
                        write_private_file(path, &payload).expect("write");
                    }
                });
            }
        });

        let content = std::fs::read(&path).expect("read");
        assert_eq!(content.len(), SIZE);
        assert!(content.iter().all(|b| *b == content[0]));
        assert!((b'a'..b'a' + WRITERS).contains(&content[0]));
        assert_eq!(mode_of(&path), 0o600);

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("f")]);
    }

    #[test]
    fn failed_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Target is a non-empty directory: rename over it fails.
        let path = dir.path().join("d");
        std::fs::create_dir(&path).expect("mkdir");
        std::fs::write(path.join("x"), "x").expect("write");

        assert!(write_private_file(&path, b"data").is_err());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("d")]);
    }
}
