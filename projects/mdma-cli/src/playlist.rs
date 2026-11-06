use crate::args::{impl_arg, OptPlaylistArg, PlaylistArg};
use crate::ctx::Ctx;
use crate::output::interpret_library;
use crate::stdin::{hashes_in, read_hashes, read_stdin_to_string};
use bpaf::{construct, long, Bpaf, Parser};
use color_eyre::Result;
use colored::Colorize;
use hicli::Arg;
use library_ipc_client::{ContentHash, LibraryResponse, PlaylistName};
use std::num::NonZeroUsize;

#[derive(Debug, Clone, PartialEq, Eq, Bpaf, Arg)]
pub(crate) struct RenameArgs {
    /// Current playlist name
    #[bpaf(positional("FROM"))]
    pub(crate) from: PlaylistArg,
    /// New playlist name
    #[bpaf(positional("TO"))]
    pub(crate) to: PlaylistArg,
}

/// Minimum number of matching tracks for `--at-least`: a whole number, at least 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AtLeastArg(pub NonZeroUsize);

impl std::str::FromStr for AtLeastArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<usize>()
            .ok()
            .and_then(NonZeroUsize::new)
            .map(Self)
            .ok_or_else(|| format!("'{s}' must be a whole number of at least 1"))
    }
}

impl_arg!(
    AtLeastArg,
    long "at-least",
    "AT_LEAST",
    "Print playlist only if it contains at least N input tracks"
);

impl Arg for ContainsMode {
    /// At most one of `--all`, `--at-least N`, `--no`; none means any.
    fn parser() -> impl Parser<Self> {
        let all = long("all")
            .help("Print playlist only if it contains ALL input tracks")
            .req_flag(Self::All);
        let at_least = <AtLeastArg as Arg>::parser().map(|AtLeastArg(n)| Self::AtLeast(n));
        let no = long("no")
            .help("Print playlist only if it contains NONE of the input tracks")
            .req_flag(Self::None);
        construct!([all, at_least, no]).fallback(Self::Any)
    }
}

/// Which playlists `playlist contains` prints, relative to the piped-in tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainsMode {
    Any,
    All,
    AtLeast(NonZeroUsize),
    None,
}

impl ContainsMode {
    /// Returns `(threshold, invert)` for matching against `input_len` tracks.
    fn threshold(self, input_len: usize) -> (usize, bool) {
        match self {
            Self::None => (1, true),
            Self::All => (input_len, false),
            Self::AtLeast(n) => (n.get(), false),
            Self::Any => (1, false),
        }
    }
}

fn playlist_content(response: LibraryResponse) -> Option<String> {
    match response {
        LibraryResponse::PlaylistContent(c) => Some(c),
        _ => None,
    }
}

fn playlist_names(response: LibraryResponse) -> Option<Vec<PlaylistName>> {
    match response {
        LibraryResponse::PlaylistNames(names) => Some(names),
        _ => None,
    }
}

/// List all playlists
#[hicli::command]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{DurationSeconds, LibraryRequest};

    let library = ctx.library()?;
    let names = interpret_library(
        library.request(&LibraryRequest::PlaylistList),
        playlist_names,
    )?;

    for name in &names {
        let content = match library.request(&LibraryRequest::PlaylistGet { name: name.clone() }) {
            Ok(LibraryResponse::PlaylistContent(c)) => c,
            _ => continue,
        };

        let hashes = hashes_in(&content);

        let track_count = hashes.len();

        let total_secs: u32 = hashes
            .iter()
            .filter_map(|h| {
                let track = library.get_track(h).ok()?;
                track.duration.map(|d| d.value())
            })
            .sum();

        let duration = DurationSeconds::new(total_secs);
        println!(
            "{}  {}  [{}]",
            name.to_string().bold(),
            format!("{} tracks", track_count).green(),
            duration
        );
    }

    Ok(())
}

/// Get playlist content (pipe to mdma queue replace to load)
///
/// If name is omitted, reads playlist names from stdin (first whitespace-delimited
/// token per line - handles both bare names and enriched `playlist list` output).
#[hicli::command]
pub(crate) fn get(name: OptPlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let OptPlaylistArg(name) = name;
    let library = ctx.library()?;
    match name {
        Some(PlaylistArg(pname)) => {
            let content = interpret_library(
                library.request(&LibraryRequest::PlaylistGet { name: pname }),
                playlist_content,
            )?;
            print!("{}", content);
        }
        None => {
            use std::io::BufRead;
            let names: Vec<String> = std::io::stdin()
                .lock()
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| {
                    let first = line.split_whitespace().next()?;
                    Some(first.to_string())
                })
                .collect();
            if names.is_empty() {
                eprintln!("No playlist name provided and stdin was empty");
                std::process::exit(1);
            }
            for n in names {
                let pname = PlaylistName::new(&n).unwrap_or_else(|e| {
                    eprintln!("Invalid playlist name: {}", e);
                    std::process::exit(1);
                });
                let content = interpret_library(
                    library.request(&LibraryRequest::PlaylistGet { name: pname }),
                    playlist_content,
                )?;
                print!("{}", content);
            }
        }
    }
    Ok(())
}

/// Print the name of every playlist that contains any of the tracks piped in on stdin
///
/// Reads track lines from stdin (8-12 char hex hash as first token).
/// Output: one playlist name per match; pipe to `sort -u` to deduplicate.
#[hicli::command]
pub(crate) fn contains(mode: ContainsMode, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    use std::collections::HashSet;

    let library = ctx.library()?;

    // 1. Read hashes from stdin
    let input_hashes = read_hashes();

    if input_hashes.is_empty() {
        eprintln!("No hashes provided on stdin.");
        std::process::exit(1);
    }

    // 2. Determine threshold and invert
    let (threshold, invert) = mode.threshold(input_hashes.len());

    let input_set: HashSet<&ContentHash> = input_hashes.iter().collect();

    // 3. Get all playlist names
    let names = interpret_library(
        library.request(&LibraryRequest::PlaylistList),
        playlist_names,
    )?;

    // 4. For each playlist, count how many input hashes appear in it
    for pname in &names {
        let content = match library.request(&LibraryRequest::PlaylistGet {
            name: pname.clone(),
        }) {
            Ok(LibraryResponse::PlaylistContent(c)) => c,
            _ => continue,
        };

        let playlist_hashes: HashSet<ContentHash> = hashes_in(&content).into_iter().collect();

        let match_count = input_set
            .iter()
            .filter(|h| playlist_hashes.contains(**h))
            .count();

        let should_print = if invert {
            match_count == 0
        } else {
            match_count >= threshold
        };

        if should_print {
            println!("{}", pname);
        }
    }

    Ok(())
}

/// Create a new playlist from stdin (fails if it already exists)
#[hicli::command]
pub(crate) fn new(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string();
    interpret_library(
        library.request(&LibraryRequest::PlaylistNew {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    )?;
    eprintln!("Created playlist '{}'", pname);
    Ok(())
}

/// Append stdin to an existing playlist
#[hicli::command]
pub(crate) fn append(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string();
    interpret_library(
        library.request(&LibraryRequest::PlaylistAppend {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    )?;
    eprintln!("Appended to playlist '{}'", pname);
    Ok(())
}

/// Replace playlist content from stdin
#[hicli::command]
pub(crate) fn replace(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string();
    interpret_library(
        library.request(&LibraryRequest::PlaylistReplace {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    )?;
    eprintln!("Replaced playlist '{}'", pname);
    Ok(())
}

/// Edit playlist in $EDITOR
#[hicli::command]
pub(crate) fn edit(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse, ProtocolError};

    let library = ctx.library()?;
    let PlaylistArg(pname) = name;

    // 1. Get current content (or start empty if not found)
    let current = match library.request(&LibraryRequest::PlaylistGet {
        name: pname.clone(),
    }) {
        Ok(LibraryResponse::PlaylistContent(c)) => c,
        Ok(LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. })) => String::new(),
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    };

    // 2. Edit a temp file in $EDITOR
    let new_content = crate::editor::edit_text("mdma-playlist-", &current);

    // 3. Replace
    interpret_library(
        library.request(&LibraryRequest::PlaylistReplace {
            name: pname.clone(),
            content: new_content,
        }),
        playlist_content,
    )?;
    eprintln!("Updated playlist '{}'", pname);
    Ok(())
}

/// Remove a playlist
#[hicli::command]
pub(crate) fn remove(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    let response = library.request(&LibraryRequest::PlaylistRemove {
        name: pname.clone(),
    });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Removed playlist '{}'", pname);
        }
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Rename a playlist
#[hicli::command]
pub(crate) fn rename(args: RenameArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let library = ctx.library()?;
    let PlaylistArg(from_name) = args.from;
    let PlaylistArg(to_name) = args.to;
    let response = library.request(&LibraryRequest::PlaylistRename {
        from: from_name.clone(),
        to: to_name.clone(),
    });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Renamed playlist '{}' to '{}'", from_name, to_name);
        }
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    use crate::test_support::{parses, run_arg, usage_error};

    #[test]
    fn rename_args_parser_reads_from_and_to() {
        let name = |n: &str| PlaylistArg(PlaylistName::new(n).unwrap());
        assert_eq!(
            run_arg::<RenameArgs>(&["old", "new"]),
            Ok(RenameArgs {
                from: name("old"),
                to: name("new"),
            })
        );
        assert!(run_arg::<RenameArgs>(&["old"]).is_err());
        assert!(run_arg::<RenameArgs>(&["old", "has space"]).is_err());
    }

    #[test]
    fn at_least_arg_parser_reads_a_non_zero_count() {
        let two = AtLeastArg(NonZeroUsize::new(2).unwrap());
        assert_eq!(run_arg::<AtLeastArg>(&["--at-least", "2"]), Ok(two));
        let err = run_arg::<AtLeastArg>(&["--at-least", "0"]).unwrap_err();
        assert!(
            err.contains("must be a whole number of at least 1"),
            "{err}"
        );
        assert!(run_arg::<AtLeastArg>(&[]).is_err());
    }

    #[test]
    fn contains_mode_parser_picks_at_most_one_mode() {
        let three = NonZeroUsize::new(3).unwrap();
        assert_eq!(run_arg::<ContainsMode>(&[]), Ok(ContainsMode::Any));
        assert_eq!(run_arg::<ContainsMode>(&["--all"]), Ok(ContainsMode::All));
        assert_eq!(run_arg::<ContainsMode>(&["--no"]), Ok(ContainsMode::None));
        assert_eq!(
            run_arg::<ContainsMode>(&["--at-least", "3"]),
            Ok(ContainsMode::AtLeast(three))
        );
        assert!(run_arg::<ContainsMode>(&["--all", "--no"]).is_err());
        assert!(run_arg::<ContainsMode>(&["--all", "--at-least", "3"]).is_err());
        let err = run_arg::<ContainsMode>(&["--at-least", "0"]).unwrap_err();
        assert!(
            err.contains("must be a whole number of at least 1"),
            "{err}"
        );
    }

    fn argv<'a>(args: &[&'a str]) -> Vec<&'a str> {
        std::iter::once("playlist")
            .chain(args.iter().copied())
            .collect()
    }

    /// The path `mdma playlist <args>` reaches.
    fn playlist(args: &[&str]) -> Vec<&'static str> {
        parses(&argv(args))
    }

    /// `mdma playlist <args>` must be rejected as a usage error.
    fn playlist_usage_error(args: &[&str]) -> String {
        usage_error(&argv(args))
    }

    #[test]
    fn invalid_playlist_name_is_a_usage_error() {
        for cmd in ["new", "append", "replace", "edit", "remove", "get"] {
            playlist_usage_error(&[cmd, "has space"]);
            playlist_usage_error(&[cmd, "a/b"]);
        }
        playlist_usage_error(&["rename", "bad name", "ok"]);
        playlist_usage_error(&["rename", "ok", "bad/name"]);
    }

    #[test]
    fn valid_playlist_names_are_accepted() {
        assert_eq!(playlist(&["new", "my-list_1"]), ["playlist", "new"]);
        assert_eq!(playlist(&["get"]), ["playlist", "get"]);
        assert_eq!(playlist(&["get", "mix"]), ["playlist", "get"]);
        assert_eq!(playlist(&["rename", "a", "b"]), ["playlist", "rename"]);
        assert_eq!(
            run_arg::<PlaylistArg>(&["my-list_1"])
                .unwrap()
                .0
                .to_string(),
            "my-list_1"
        );
    }

    #[test]
    fn contains_conflicting_flags_are_a_usage_error() {
        playlist_usage_error(&["contains", "--all", "--no"]);
        playlist_usage_error(&["contains", "--all", "--at-least", "2"]);
        playlist_usage_error(&["contains", "--no", "--at-least", "2"]);
    }

    #[test]
    fn contains_zero_at_least_is_a_usage_error() {
        for bad in ["0", "abc", "1.5", ""] {
            let err = playlist_usage_error(&["contains", "--at-least", bad]);
            assert!(
                err.contains("must be a whole number of at least 1"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn contains_flags_build_the_right_mode() {
        assert_eq!(playlist(&["contains"]), ["playlist", "contains"]);
        assert_eq!(run_arg::<ContainsMode>(&[]), Ok(ContainsMode::Any));
        assert_eq!(
            run_arg::<ContainsMode>(&["--at-least", "2"]),
            Ok(ContainsMode::AtLeast(NonZeroUsize::new(2).unwrap()))
        );
    }

    #[test]
    fn contains_threshold_default_is_one_not_inverted() {
        assert_eq!(ContainsMode::Any.threshold(3), (1, false));
    }

    #[test]
    fn contains_threshold_all_uses_input_len() {
        assert_eq!(ContainsMode::All.threshold(5), (5, false));
    }

    #[test]
    fn contains_threshold_at_least_uses_given_value() {
        let mode = ContainsMode::AtLeast(NonZeroUsize::new(3).unwrap());
        assert_eq!(mode.threshold(7), (3, false));
    }

    #[test]
    fn contains_threshold_no_flag_inverts() {
        assert_eq!(ContainsMode::None.threshold(4), (1, true));
    }
}
