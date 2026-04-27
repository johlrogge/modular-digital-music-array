use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct LogFile {
    pub name: String,
    pub size_bytes: u64,
    pub modified: SystemTime,
}

/// List files in a log dir. Returns newest-first.
/// Includes `current` and any `@<ts>.s` files.
/// Skips other svlogd files (`config`, `lock`, `state`, etc.).
pub fn list(dir: &Path) -> std::io::Result<Vec<LogFile>> {
    let mut files = Vec::new();

    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        if !is_svlogd_log_name(&name) {
            continue;
        }

        let metadata = entry.metadata()?;
        let size_bytes = metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        files.push(LogFile {
            name,
            size_bytes,
            modified,
        });
    }

    // Sort newest-first by modification time
    files.sort_by_key(|f| std::cmp::Reverse(f.modified));

    Ok(files)
}

/// Returns true if `name` is a valid svlogd log filename:
/// - exactly `"current"`, or
/// - starts with `@`, ends with `.s`, and the middle portion is at least
///   14 ASCII chars all in `[0-9a-v]` (svlogd base32 timestamp, plus extras).
///
/// This is the single canonical predicate used by both the filesystem
/// filter and the HTTP path-validation guard.
pub(crate) fn is_svlogd_log_name(name: &str) -> bool {
    if name == "current" {
        return true;
    }
    if let Some(middle) = name.strip_prefix('@').and_then(|s| s.strip_suffix(".s")) {
        middle.len() >= 14
            && middle
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'v'))
    } else {
        false
    }
}

/// Read a page of lines from a log file.
/// `offset_lines` = number of leading lines to skip (0-based).
/// `limit` = max lines to return.
/// Returns (lines, has_more) where `has_more` is true if more lines remain past the page.
pub fn read_page(
    path: &Path,
    offset_lines: usize,
    limit: usize,
) -> std::io::Result<(Vec<String>, bool)> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut lines = Vec::new();
    let mut iter = reader.lines().skip(offset_lines);

    // Read up to limit + 1 lines to detect has_more
    for _ in 0..=limit {
        match iter.next() {
            Some(Ok(line)) => lines.push(line),
            Some(Err(e)) => return Err(e),
            None => break,
        }
    }

    if lines.len() > limit {
        lines.truncate(limit);
        Ok((lines, true))
    } else {
        Ok((lines, false))
    }
}

/// Construct the full path to a named log file within the directory.
pub fn log_file_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn set_mtime(path: &Path, secs_since_epoch: u64) {
        use std::time::{Duration, UNIX_EPOCH};
        let mtime = UNIX_EPOCH + Duration::from_secs(secs_since_epoch);
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(mtime).unwrap();
    }

    // Valid svlogd timestamp used in list tests (14 lowercase base32 chars)
    const ROTATED_A: &str = "@4000000067aaaaaa.s";
    const ROTATED_B: &str = "@4000000067bbbbbb.s";

    #[test]
    fn lists_newest_first() {
        let dir = tempdir().unwrap();

        // Create files with staggered mtimes
        let current_path = dir.path().join("current");
        let a_path = dir.path().join(ROTATED_A);
        let b_path = dir.path().join(ROTATED_B);

        std::fs::write(&current_path, b"current content").unwrap();
        std::fs::write(&a_path, b"a content").unwrap();
        std::fs::write(&b_path, b"b content").unwrap();

        // Set deterministic mtimes: ROTATED_B newest, then current, then ROTATED_A oldest
        set_mtime(&b_path, 1_700_000_003);
        set_mtime(&current_path, 1_700_000_002);
        set_mtime(&a_path, 1_700_000_001);

        let files = list(dir.path()).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].name, ROTATED_B);
        assert_eq!(files[1].name, "current");
        assert_eq!(files[2].name, ROTATED_A);
    }

    #[test]
    fn lists_skips_non_log_files() {
        let dir = tempdir().unwrap();

        std::fs::write(dir.path().join("current"), b"log").unwrap();
        std::fs::write(dir.path().join("config"), b"").unwrap();
        std::fs::write(dir.path().join("lock"), b"").unwrap();
        std::fs::write(dir.path().join("state"), b"").unwrap();
        std::fs::write(dir.path().join(ROTATED_A), b"rotated").unwrap();

        let files = list(dir.path()).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();

        assert!(names.contains(&"current"), "missing current");
        assert!(names.contains(&ROTATED_A), "missing {ROTATED_A}");
        assert!(!names.contains(&"config"), "config should be excluded");
        assert!(!names.contains(&"lock"), "lock should be excluded");
        assert!(!names.contains(&"state"), "state should be excluded");
        assert_eq!(files.len(), 2);
    }

    // --- is_svlogd_log_name unit tests ---

    #[test]
    fn accepts_current() {
        assert!(is_svlogd_log_name("current"));
    }

    #[test]
    fn accepts_valid_svlogd_filename() {
        // 14-char base32 middle (standard tai64n length)
        assert!(is_svlogd_log_name("@4000000067abc123.s"));
        // Longer middle — future extension accepted
        assert!(is_svlogd_log_name("@4000000067abc1234567ef.s"));
    }

    #[test]
    fn rejects_empty_string() {
        assert!(!is_svlogd_log_name(""));
    }

    #[test]
    fn rejects_dotdot() {
        assert!(!is_svlogd_log_name(".."));
        assert!(!is_svlogd_log_name("../etc/passwd"));
    }

    #[test]
    fn rejects_path_separator() {
        assert!(!is_svlogd_log_name("current/foo"));
        assert!(!is_svlogd_log_name("@4000000067abc123/extra.s"));
    }

    #[test]
    fn rejects_at_with_short_middle() {
        // Empty middle
        assert!(!is_svlogd_log_name("@.s"));
        // 13 chars — one too short
        assert!(!is_svlogd_log_name("@4000000067abc.s"));
    }

    #[test]
    fn rejects_at_with_invalid_chars() {
        // Space in middle
        assert!(!is_svlogd_log_name("@a b c12345678.s"));
        // Uppercase — svlogd uses only lowercase base32
        assert!(!is_svlogd_log_name("@4000000067ABCDEF.s"));
        // @ in middle
        assert!(!is_svlogd_log_name("@4000000067a@bcde.s"));
        // Newline
        assert!(!is_svlogd_log_name("@4000000067abc\n12.s"));
    }

    #[test]
    fn rejects_unrelated_files() {
        assert!(!is_svlogd_log_name("config"));
        assert!(!is_svlogd_log_name("lock"));
        assert!(!is_svlogd_log_name("state"));
        assert!(!is_svlogd_log_name("@.txt"));
        assert!(!is_svlogd_log_name("current.bak"));
    }

    #[test]
    fn read_page_pagination() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("current");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..1000 {
            writeln!(f, "line{i}").unwrap();
        }

        // First page: offset=0, limit=100
        let (lines, has_more) = read_page(&path, 0, 100).unwrap();
        assert_eq!(lines.len(), 100);
        assert!(has_more, "expected has_more=true for first page");
        assert_eq!(lines[0], "line0");
        assert_eq!(lines[99], "line99");

        // Near-end page: offset=900, limit=200 — only 100 lines remain
        let (lines, has_more) = read_page(&path, 900, 200).unwrap();
        assert_eq!(lines.len(), 100);
        assert!(!has_more, "expected has_more=false at end");
        assert_eq!(lines[0], "line900");
        assert_eq!(lines[99], "line999");
    }

    #[test]
    fn read_page_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("current");
        std::fs::write(&path, b"").unwrap();

        let (lines, has_more) = read_page(&path, 0, 100).unwrap();
        assert!(lines.is_empty());
        assert!(!has_more);
    }

    #[test]
    fn read_page_offset_past_end() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("current");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..10 {
            writeln!(f, "line{i}").unwrap();
        }

        let (lines, has_more) = read_page(&path, 20, 10).unwrap();
        assert!(lines.is_empty());
        assert!(!has_more);
    }
}
