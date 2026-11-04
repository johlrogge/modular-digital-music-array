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

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Print `prompt` (which ends in `[y/N] `) and read the answer from stdin:
/// `true` only for y/yes.
pub fn confirm(prompt: &str) -> std::io::Result<bool> {
    use std::io::Write;
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(is_yes(&answer))
}

/// The hashes in `lines`, in order (see [`parse_hash_from_line`]).
fn hashes_from_lines<L: AsRef<str>>(lines: impl Iterator<Item = L>) -> Vec<ContentHash> {
    lines
        .filter_map(|line| parse_hash_from_line(line.as_ref()))
        .map(ContentHash::new)
        .collect()
}

/// The hashes in `text`, one per line (e.g. playlist content).
pub fn hashes_in(text: &str) -> Vec<ContentHash> {
    hashes_from_lines(text.lines())
}

/// Read ALL lines from stdin and extract the hashes in them (empty when there are none).
pub fn read_hashes() -> Vec<ContentHash> {
    use std::io::BufRead;
    hashes_from_lines(std::io::stdin().lock().lines().map_while(Result::ok))
}

/// Return the provided hash as a single-element vec, or read ALL lines from stdin and
/// extract valid hashes (8+ lowercase hex chars, or sha256:-prefixed full hashes, as first token).
pub fn hashes_arg_or_stdin(hash: Option<ContentHash>) -> Vec<ContentHash> {
    match hash {
        Some(h) => vec![h],
        None => {
            let hashes = read_hashes();
            if hashes.is_empty() {
                eprintln!("No hash provided and stdin was empty");
                std::process::exit(1);
            }
            hashes
        }
    }
}

/// Resolve a list of hashes to `TrackInfo`, skipping any that the library
/// cannot find and printing a warning for each skipped entry.
pub fn resolve_tracks_skip_errors(
    library: &LibraryBackend,
    hashes: Vec<ContentHash>,
) -> Vec<TrackInfo> {
    hashes
        .into_iter()
        .filter_map(|hash| match library.get_track(&hash) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("Warning: skipping track {}: {}", hash, e);
                None
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

#[cfg(test)]
mod tests {

    use crate::stdin::{hashes_in, is_yes, parse_hash_from_line};
    use library_ipc_client::ContentHash;

    use pretty_assertions::assert_eq;

    #[test]
    fn parse_hash_from_plain_hash_line() {
        // Plain hash with no surrounding text
        let result = parse_hash_from_line("abcd1234ef567890");
        assert_eq!(result, Some("abcd1234ef567890".to_string()));
    }

    #[test]
    fn parse_hash_from_search_output_line() {
        // Canonical playlist line format: {short_hash}  {Artist} - {Title}  [{duration}]
        let result = parse_hash_from_line("abcd1234  Carbon Based Lifeforms - Init  [8:28]");
        assert_eq!(result, Some("abcd1234".to_string()));
    }

    #[test]
    fn parse_hash_from_sha256_prefixed_line() {
        // Full sha256: prefixed hash on its own line
        let result = parse_hash_from_line(
            "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        );
        assert_eq!(
            result,
            Some(
                "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                    .to_string()
            )
        );
    }

    #[test]
    fn parse_hash_from_empty_line_returns_none() {
        let result = parse_hash_from_line("");
        assert!(result.is_none());
    }

    #[test]
    fn parse_hash_from_comment_line_returns_none() {
        // Lines starting with # are comments
        let result = parse_hash_from_line("# this is a comment");
        assert!(result.is_none());
    }

    #[test]
    fn parse_hash_from_uppercase_returns_none() {
        assert!(parse_hash_from_line("ABCDEF1234567890").is_none());
    }

    #[test]
    fn parse_hash_from_short_token_returns_none() {
        assert!(parse_hash_from_line("abcd12").is_none());
    }

    #[test]
    fn parse_hash_from_unavailable_comment_line() {
        // Queue edit writes unresolvable tracks as: {short_hash}  # [unavailable: error]
        // The hash must survive the round-trip through parse_hash_from_line.
        let result = parse_hash_from_line("a1b2c3d4  # [unavailable: track not found]");
        assert_eq!(result, Some("a1b2c3d4".to_string()));
    }

    #[test]
    fn hashes_in_keeps_hash_lines_in_order_and_skips_the_rest() {
        let text = "# header\n\nabcd1234  Artist - Title  [1:00]\nnot a hash\nsha256:0123abcd\n";
        assert_eq!(
            hashes_in(text),
            vec![
                ContentHash::new("abcd1234"),
                ContentHash::new("sha256:0123abcd")
            ]
        );
    }

    #[test]
    fn only_yes_confirms_a_merge() {
        for yes in ["y", "Y", "yes", " YES \n"] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["", "n", "no", "maybe", "\n"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }
}
