//! Test helpers shared by several modules' tests.

use crate::{Cli, Commands};
use bpaf::Parser as _;
use clap::Parser;
use library_ipc_client::{ContentHash, TrackInfo};

/// Parse `args` (without the program name) as an `mdma` command line.
pub(crate) fn parse_cli(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("mdma").chain(args.iter().copied()))
}

/// Parse `args` (without the program name), keeping only the command.
pub(crate) fn parse(args: &[&str]) -> Result<Commands, clap::Error> {
    parse_cli(args).map(|cli| cli.command)
}

/// Parse `args`, which must be a valid command line.
pub(crate) fn parses(args: &[&str]) -> Commands {
    parse(args).unwrap_or_else(|e| panic!("{args:?} should parse: {e}"))
}

/// Parse `args`, which must be rejected as a usage error (exit code 2).
pub(crate) fn usage_error(args: &[&str]) -> clap::Error {
    let err = parse(args).expect_err("should be rejected");
    assert_eq!(err.exit_code(), 2, "{args:?}");
    err
}

/// Run the [`hicli::Arg`] parser of `T` over `args`; an error is bpaf's message
/// (which wraps long lines, so its whitespace is collapsed to single spaces).
pub(crate) fn run_arg<T: hicli::Arg>(args: &[&str]) -> Result<T, String> {
    <T as hicli::Arg>::parser()
        .to_options()
        .run_inner(args)
        .map_err(|failure| {
            let message = failure.unwrap_stderr();
            message.split_whitespace().collect::<Vec<_>>().join(" ")
        })
}

/// Strip ANSI escape codes from a string.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(next) = chars.next() {
                if next == '[' {
                    for seq_char in chars.by_ref() {
                        if seq_char.is_ascii_alphabetic() || seq_char == '~' {
                            break;
                        }
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A track with only its content hash set: everything optional is `None`,
/// collections are empty. Build variations with `TrackInfo { .., ..track(hash) }`.
pub(crate) fn track(hash: &str) -> TrackInfo {
    TrackInfo {
        content_hash: ContentHash::new(hash),
        title: None,
        artist: None,
        album: None,
        duration: None,
        bpm: None,
        key: None,
        blob_path: None,
        cover_art_path: None,
        track_number: None,
        disc_number: None,
        added: None,
        started: None,
        stopped: None,
        memory_cues: vec![],
        beat_grid: None,
        role: None,
        energy: None,
    }
}
