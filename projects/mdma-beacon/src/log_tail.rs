use std::path::PathBuf;

use futures::Stream;

/// Follow a file, yielding lines as they are appended.
///
/// On start:
/// - If the file doesn't exist, retries with a 1s backoff until it does.
/// - Once open: reverse-seeks to find approximately the last `last_n` newlines
///   and emits those lines first (the backfill).
/// - Then polls every 250ms for new bytes and yields new lines.
/// - Handles svlogd rotation: if the inode changes at a poll tick, reopens the file.
/// - The stream never terminates on its own.
pub fn follow(path: PathBuf, last_n: usize) -> impl Stream<Item = String> {
    async_stream::stream! {
        // Wait for the file to exist.
        let mut file = loop {
            match tokio::fs::File::open(&path).await {
                Ok(f) => break f,
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        };

        // Get initial inode for rotation detection.
        let mut current_inode = inode_of(&path).await;

        // Emit backfill: last `last_n` lines.
        let backfill = read_last_n_lines(&path, last_n).await;
        for line in backfill {
            yield line;
        }

        // Seek to end so we only read new content.
        use tokio::io::AsyncSeekExt;
        let _ = file.seek(std::io::SeekFrom::End(0)).await;

        // Poll loop: every 250ms read new bytes, detect rotation.
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        let mut reader = tokio::io::BufReader::new(file);
        let mut partial_line = String::new();

        loop {
            interval.tick().await;

            // Check for rotation: inode change means svlogd renamed current.
            let new_inode = inode_of(&path).await;
            if new_inode != current_inode {
                // Drain remaining bytes from old file before switching.
                use tokio::io::AsyncBufReadExt;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                            if partial_line.is_empty() {
                                if !trimmed.is_empty() {
                                    yield trimmed.to_string();
                                }
                            } else {
                                partial_line.push_str(trimmed);
                                let out = partial_line.clone();
                                partial_line.clear();
                                yield out;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // Reopen the new current file from the beginning.
                loop {
                    match tokio::fs::File::open(&path).await {
                        Ok(f) => {
                            reader = tokio::io::BufReader::new(f);
                            break;
                        }
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        }
                    }
                }
                current_inode = new_inode;
                partial_line.clear();
                continue;
            }

            // Read any new complete lines.
            use tokio::io::AsyncBufReadExt;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // No new data yet.
                    Ok(_) => {
                        if line.ends_with('\n') {
                            let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                            partial_line.push_str(trimmed);
                            let out = partial_line.clone();
                            partial_line.clear();
                            yield out;
                        } else {
                            // Partial line at end of file; buffer until newline arrives.
                            partial_line.push_str(&line);
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// Returns the inode number of the file at `path`, or 0 if unavailable.
async fn inode_of(path: &PathBuf) -> u64 {
    use std::os::unix::fs::MetadataExt;
    match tokio::fs::metadata(path).await {
        Ok(m) => m.ino(),
        Err(_) => 0,
    }
}

/// Read the last `n` lines from `path` using a reverse-seek strategy.
///
/// Reads the file from end backwards in 8 KB chunks until `n + 1` newlines
/// are found (or start of file). Then reads that range forward to extract lines.
async fn read_last_n_lines(path: &PathBuf, n: usize) -> Vec<String> {
    if n == 0 {
        return vec![];
    }

    let content = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(_) => return vec![],
    };

    let total = content.len();
    if total == 0 {
        return vec![];
    }

    const CHUNK: usize = 8192;

    // Walk backwards through the bytes, counting newlines.
    let mut newlines_found: usize = 0;
    let mut pos = total;
    let mut found_offset: Option<usize> = None;

    'outer: loop {
        let chunk_start = pos.saturating_sub(CHUNK);
        let slice = &content[chunk_start..pos];

        // Scan the chunk backwards.
        for (i, &byte) in slice.iter().enumerate().rev() {
            if byte == b'\n' {
                newlines_found += 1;
                if newlines_found > n {
                    // We've found n+1 newlines; the content starts right after this newline.
                    found_offset = Some(chunk_start + i + 1);
                    break 'outer;
                }
            }
        }

        if chunk_start == 0 {
            // Reached the start of the file.
            break;
        }
        pos = chunk_start;
    }

    let start_offset = found_offset.unwrap_or(0);

    // Parse lines from start_offset to end.
    let tail = &content[start_offset..];
    let text = String::from_utf8_lossy(tail);
    text.lines().map(|l| l.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tokio::io::AsyncWriteExt;

    /// Helper: collect up to `count` items from the stream within `timeout_ms`.
    async fn collect_n(
        stream: impl Stream<Item = String> + Unpin,
        count: usize,
        timeout_ms: u64,
    ) -> Vec<String> {
        tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            stream.take(count).collect::<Vec<_>>(),
        )
        .await
        .unwrap_or_default()
    }

    #[tokio::test]
    async fn tails_appended_lines() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line1").unwrap();
        writeln!(tmp, "line2").unwrap();
        writeln!(tmp, "line3").unwrap();
        tmp.flush().unwrap();

        let path = tmp.path().to_path_buf();
        let stream = Box::pin(follow(path.clone(), 100));

        // Give the follow a moment to open the file and emit backfill.
        // Then append line4 and collect 4 items.
        let path2 = path.clone();
        let appender = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let mut f = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&path2)
                .await
                .unwrap();
            f.write_all(b"line4\n").await.unwrap();
        });

        let lines = collect_n(stream, 4, 3000).await;
        appender.await.unwrap();

        assert_eq!(lines, vec!["line1", "line2", "line3", "line4"]);
    }

    #[tokio::test]
    async fn handles_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current");
        let rotated = dir.path().join("@x.s");

        // Write initial content.
        tokio::fs::write(&current, b"old1\nold2\n").await.unwrap();

        let stream = Box::pin(follow(current.clone(), 100));

        let current2 = current.clone();
        let rotated2 = rotated.clone();
        let rotator = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            // Rotate: rename current → @x.s, create new current with new content.
            tokio::fs::rename(&current2, &rotated2).await.unwrap();
            let mut f = tokio::fs::File::create(&current2).await.unwrap();
            f.write_all(b"new1\nnew2\n").await.unwrap();
        });

        // Expect old lines from backfill + new lines from new file.
        let lines = collect_n(stream, 4, 4000).await;
        rotator.await.unwrap();

        assert!(
            lines.contains(&"old1".to_string()),
            "missing old1: {lines:?}"
        );
        assert!(
            lines.contains(&"old2".to_string()),
            "missing old2: {lines:?}"
        );
        assert!(
            lines.contains(&"new1".to_string()),
            "missing new1: {lines:?}"
        );
        assert!(
            lines.contains(&"new2".to_string()),
            "missing new2: {lines:?}"
        );
    }

    #[tokio::test]
    async fn initial_backfill_returns_last_n() {
        let mut tmp = NamedTempFile::new().unwrap();
        for i in 1..=1000 {
            writeln!(tmp, "line{i}").unwrap();
        }
        tmp.flush().unwrap();

        let path = tmp.path().to_path_buf();
        let stream = Box::pin(follow(path, 100));

        let lines = collect_n(stream, 100, 5000).await;

        assert_eq!(
            lines.len(),
            100,
            "expected 100 backfill lines, got {}",
            lines.len()
        );
        assert_eq!(lines[0], "line901");
        assert_eq!(lines[99], "line1000");
    }

    #[tokio::test]
    async fn handles_missing_file_with_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current");

        let path2 = path.clone();
        // File doesn't exist yet; create it after 200ms.
        let creator = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let mut f = tokio::fs::File::create(&path2).await.unwrap();
            f.write_all(b"hello\nworld\n").await.unwrap();
        });

        let stream = Box::pin(follow(path, 100));
        let lines = collect_n(stream, 2, 5000).await;
        creator.await.unwrap();

        assert_eq!(lines, vec!["hello", "world"]);
    }
}
