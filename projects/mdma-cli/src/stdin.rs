//! Helpers for commands that take their input from stdin.
//!
//! A command connects (and so resolves the node, which may prompt on stdin)
//! before it reads stdin.

use library_ipc_client::{ContentHash, TrackInfo};
use mdma_client::LibraryBackend;

pub fn read_stdin_to_string() -> String {
    use std::io::Read;
    let mut content = String::new();
    std::io::stdin()
        .read_to_string(&mut content)
        .unwrap_or_else(|e| {
            eprintln!("Failed to read stdin: {}", e);
            std::process::exit(1);
        });
    content
}

pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Return the provided hash as a single-element vec, or read ALL lines from stdin and
/// extract valid hashes (8+ lowercase hex chars, or sha256:-prefixed full hashes, as first token).
pub fn hashes_arg_or_stdin(hash: Option<String>) -> Vec<String> {
    match hash {
        Some(h) => vec![h],
        None => {
            use std::io::BufRead;
            let hashes: Vec<String> = std::io::stdin()
                .lock()
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| parse_hash_from_line(&line))
                .collect();
            if hashes.is_empty() {
                eprintln!("No hash provided and stdin was empty");
                std::process::exit(1);
            }
            hashes
        }
    }
}

/// Resolve a list of raw hash strings to `TrackInfo`, skipping any that the library
/// cannot find and printing a warning for each skipped entry.
pub fn resolve_tracks_skip_errors(client: &LibraryBackend, hashes: Vec<String>) -> Vec<TrackInfo> {
    hashes
        .into_iter()
        .filter_map(|hash| {
            let content_hash = ContentHash::new(hash.clone());
            match client.get_track(&content_hash) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("Warning: skipping track {}: {}", hash, e);
                    None
                }
            }
        })
        .collect()
}

/// Extract a content hash from a line of text.
///
/// Handles multiple formats:
/// - Plain hash: `abcd1234ef567890`
/// - Full sha256: prefix: `sha256:abcdef0123...`
/// - Search output: `{8-char-hash}  {Artist} - {Title}  [{duration}]`
/// - Any line whose first whitespace-separated token looks like a hash
///
/// Returns `None` for blank lines, comment lines (starting with `#`), and lines
/// where the first token doesn't resemble a hash.
pub fn parse_hash_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let token = line.split_whitespace().next()?;
    // Accept sha256: prefixed full hashes
    if token.starts_with("sha256:") {
        let hex_part = token.strip_prefix("sha256:")?;
        if hex_part.len() >= 8
            && hex_part
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        {
            return Some(token.to_string());
        }
        return None;
    }
    // Accept plain hex tokens of 8+ chars (short or full)
    if token.len() >= 8
        && token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
    {
        return Some(token.to_string());
    }
    None
}
