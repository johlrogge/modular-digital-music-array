//! Helpers for commands that take their input from stdin.
//!
//! A command connects (and so resolves the node, which may prompt on stdin)
//! before it reads stdin.

use crate::args::{OptTrackArg, TrackArg};
use crate::ctx::Ctx;
use crate::playback::playing_track;
use color_eyre::eyre::{bail, eyre};
use color_eyre::Result;
use library_ipc_client::{ContentHash, TrackInfo};
use mdma_client::LibraryBackend;
use std::io::BufRead;

/// All of stdin as text.
pub(crate) fn read_stdin_to_string() -> Result<String> {
    read_all(std::io::stdin())
}

fn read_all(mut reader: impl std::io::Read) -> Result<String> {
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .map_err(|e| eyre!("Failed to read stdin: {e}"))?;
    Ok(content)
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Print `prompt` (which ends in `[y/N] `) and read the answer from stdin:
/// `true` only for y/yes.
pub(crate) fn confirm(prompt: &str) -> std::io::Result<bool> {
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
pub(crate) fn hashes_in(text: &str) -> Vec<ContentHash> {
    hashes_from_lines(text.lines())
}

/// Read ALL lines from stdin and extract the hashes in them (empty when there are none).
pub(crate) fn read_hashes() -> Vec<ContentHash> {
    hashes_from_reader(std::io::stdin().lock())
}

/// The hashes in the lines of `reader`; reading stops at the first unreadable line.
fn hashes_from_reader(reader: impl BufRead) -> Vec<ContentHash> {
    hashes_from_lines(reader.lines().map_while(std::result::Result::ok))
}

/// Read ALL lines from stdin and extract valid hashes (8+ lowercase hex chars, or
/// sha256:-prefixed full hashes, as first token); an error when there are none.
pub(crate) fn require_stdin_hashes() -> Result<Vec<ContentHash>> {
    require_hashes(std::io::stdin().lock(), no_hash_provided)
}

/// Like [`require_stdin_hashes`], but the error is "No hashes provided on stdin."
/// followed by the `usage` lines.
pub(crate) fn require_stdin_hashes_or_usage(usage: &[&str]) -> Result<Vec<ContentHash>> {
    hashes_or_usage(std::io::stdin().lock(), usage)
}

fn hashes_or_usage(reader: impl BufRead, usage: &[&str]) -> Result<Vec<ContentHash>> {
    require_hashes(reader, || no_hashes_on_stdin(usage))
}

/// The hashes in `reader`; `on_none` makes the error when there are none.
fn require_hashes(
    reader: impl BufRead,
    on_none: impl FnOnce() -> color_eyre::Report,
) -> Result<Vec<ContentHash>> {
    let hashes = hashes_from_reader(reader);
    if hashes.is_empty() {
        return Err(on_none());
    }
    Ok(hashes)
}

fn no_hash_provided() -> color_eyre::Report {
    eyre!("No hash provided and stdin was empty")
}

/// The "no hashes on stdin" error, with `usage` lines under it.
fn no_hashes_on_stdin(usage: &[&str]) -> color_eyre::Report {
    let lines = std::iter::once("No hashes provided on stdin.").chain(usage.iter().copied());
    eyre!(lines.collect::<Vec<_>>().join("\n"))
}

/// Return the provided hash as a single-element vec, or the hashes from stdin
/// (see [`require_stdin_hashes`]).
pub(crate) fn hashes_arg_or_stdin(hash: OptTrackArg) -> Result<Vec<ContentHash>> {
    hashes_arg_or_reader(hash, std::io::stdin().lock())
}

fn hashes_arg_or_reader(hash: OptTrackArg, reader: impl BufRead) -> Result<Vec<ContentHash>> {
    let OptTrackArg(hash) = hash;
    match hash {
        Some(TrackArg(h)) => Ok(vec![h]),
        None => require_hashes(reader, no_hash_provided),
    }
}

/// The hashes piped on stdin (empty when it holds none); `None` when stdin is a
/// terminal, i.e. nothing is piped.
pub(crate) fn piped_hashes() -> Result<Option<Vec<ContentHash>>> {
    use std::io::IsTerminal;
    Ok((!std::io::stdin().is_terminal()).then(read_hashes))
}

/// The tracks a command acts on: the hashes piped on stdin (a pipe without any
/// is an error), else, when stdin is a terminal, the track that is playing.
pub(crate) fn hashes_from_pipe_or_playing(ctx: &Ctx) -> Result<Vec<ContentHash>> {
    hashes_or_playing(piped_hashes()?, || playing_track(&ctx.playback()?))
}

/// `piped` when stdin is piped (`Some`; it must hold a hash), else the hash
/// `playing` finds.
fn hashes_or_playing(
    piped: Option<Vec<ContentHash>>,
    playing: impl FnOnce() -> Result<ContentHash>,
) -> Result<Vec<ContentHash>> {
    match piped {
        Some(hashes) if hashes.is_empty() => Err(no_hashes_on_stdin(&[])),
        Some(hashes) => Ok(hashes),
        None => Ok(vec![playing()?]),
    }
}

/// Resolve a list of hashes to `TrackInfo`, skipping any that the library
/// cannot find and printing a warning for each skipped entry.
pub(crate) fn resolve_tracks_skip_errors(
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

/// Like [`resolve_tracks_skip_errors`], but an error when none could be resolved.
pub(crate) fn resolve_tracks_or_abort(
    library: &LibraryBackend,
    hashes: Vec<ContentHash>,
) -> Result<Vec<TrackInfo>> {
    require_tracks(resolve_tracks_skip_errors(library, hashes))
}

fn require_tracks(tracks: Vec<TrackInfo>) -> Result<Vec<TrackInfo>> {
    if tracks.is_empty() {
        bail!("No tracks could be resolved. Aborting.");
    }
    Ok(tracks)
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
pub(crate) fn parse_hash_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let token = line.split_whitespace().next()?;
    is_hash_token(token).then(|| token.to_string())
}

/// The one rule for what counts as a content-hash token, shared by stdin
/// parsing and argv parsing: at least 8 lowercase hex characters, optionally
/// prefixed with `sha256:`.
pub(crate) fn is_hash_token(token: &str) -> bool {
    let hex = token.strip_prefix("sha256:").unwrap_or(token);
    hex.len() >= 8
        && hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

#[cfg(test)]
mod tests {

    use crate::args::{OptTrackArg, TrackArg};
    use color_eyre::eyre::eyre;

    use crate::stdin::{
        hashes_arg_or_reader, hashes_in, hashes_or_playing, hashes_or_usage, is_yes,
        no_hash_provided, parse_hash_from_line, read_all, require_hashes, require_tracks,
    };
    use crate::test_support::{failure_text, track};
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
    fn hash_token_rule() {
        use crate::stdin::is_hash_token;
        assert!(is_hash_token("abcd1234"));
        assert!(is_hash_token("sha256:abcd1234"));
        assert!(!is_hash_token("abcd123"));
        assert!(!is_hash_token("sha256:abcd123"));
        assert!(!is_hash_token("ABCD1234"));
        assert!(!is_hash_token("sha256:"));
        assert!(!is_hash_token("ghijklmn"));
        assert!(!is_hash_token(""));
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
    fn required_hashes_are_read_in_order_skipping_blank_and_invalid_lines() {
        let input = "\n   \nnot a hash\nabcd1234  Artist - Title\n# comment\nsha256:0123abcd\n";
        assert_eq!(
            require_hashes(input.as_bytes(), no_hash_provided).unwrap(),
            vec![
                ContentHash::new("abcd1234"),
                ContentHash::new("sha256:0123abcd")
            ]
        );
    }

    #[test]
    fn empty_input_is_an_error() {
        assert_eq!(
            failure_text(require_hashes("".as_bytes(), no_hash_provided)),
            "No hash provided and stdin was empty"
        );
    }

    #[test]
    fn input_without_valid_hashes_is_an_error() {
        assert_eq!(
            failure_text(require_hashes(
                "\n\nABCDEF12\nshort\n".as_bytes(),
                no_hash_provided
            )),
            "No hash provided and stdin was empty"
        );
    }

    #[test]
    fn a_hash_argument_wins_over_stdin() {
        let arg = OptTrackArg(Some(TrackArg(ContentHash::new("abcd1234"))));
        assert_eq!(
            hashes_arg_or_reader(arg, "deadbeef\n".as_bytes()).unwrap(),
            vec![ContentHash::new("abcd1234")]
        );
    }

    #[test]
    fn without_a_hash_argument_stdin_is_read() {
        let arg = OptTrackArg(None);
        assert_eq!(
            hashes_arg_or_reader(arg, "deadbeef\n".as_bytes()).unwrap(),
            vec![ContentHash::new("deadbeef")]
        );
        assert_eq!(
            failure_text(hashes_arg_or_reader(OptTrackArg(None), "".as_bytes())),
            "No hash provided and stdin was empty"
        );
    }

    #[test]
    fn piped_hashes_win_over_the_playing_track() {
        let piped = vec![ContentHash::new("abcd1234")];
        let hashes = hashes_or_playing(Some(piped.clone()), || panic!("not asked")).unwrap();
        assert_eq!(hashes, piped);
    }

    #[test]
    fn a_pipe_without_hashes_is_an_error_not_the_playing_track() {
        assert_eq!(
            failure_text(hashes_or_playing(Some(vec![]), || panic!("not asked"))),
            "No hashes provided on stdin."
        );
    }

    #[test]
    fn on_a_terminal_the_playing_track_is_used() {
        let hashes = hashes_or_playing(None, || Ok(ContentHash::new("deadbeef"))).unwrap();
        assert_eq!(hashes, vec![ContentHash::new("deadbeef")]);
        assert_eq!(
            failure_text(hashes_or_playing(None, || Err(eyre!("Nothing playing")))),
            "Nothing playing"
        );
    }

    #[test]
    fn stdin_text_is_read_whole() {
        assert_eq!(read_all("a\n\nb".as_bytes()).unwrap(), "a\n\nb");
    }

    #[test]
    fn unreadable_stdin_is_an_error() {
        // Invalid UTF-8 cannot be read as text.
        assert_eq!(
            failure_text(read_all([0xff, 0xfe].as_slice())),
            "Failed to read stdin: stream did not contain valid UTF-8"
        );
    }

    #[test]
    fn no_hashes_error_lists_its_usage_lines() {
        assert_eq!(
            failure_text(hashes_or_usage("".as_bytes(), &[])),
            "No hashes provided on stdin."
        );
        assert_eq!(
            failure_text(hashes_or_usage("\n".as_bytes(), &["Usage: a", "       b"])),
            "No hashes provided on stdin.\nUsage: a\n       b"
        );
        assert_eq!(
            hashes_or_usage("abcd1234\n".as_bytes(), &["Usage: a"]).unwrap(),
            vec![ContentHash::new("abcd1234")]
        );
    }

    #[test]
    fn no_resolved_tracks_is_an_error() {
        assert_eq!(
            failure_text(require_tracks(vec![])),
            "No tracks could be resolved. Aborting."
        );
        assert_eq!(require_tracks(vec![track("abcd1234")]).unwrap().len(), 1);
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
