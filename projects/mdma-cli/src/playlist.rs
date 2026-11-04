use crate::args::PlaylistArg;
use crate::ctx::Ctx;
use crate::stdin::{hashes_in, read_hashes, read_stdin_to_string};
use clap::Subcommand;
use color_eyre::Result;
use colored::Colorize;
use library_ipc_client::{ClientError, ContentHash, LibraryResponse, PlaylistName};
use std::num::NonZeroUsize;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// List all playlists
    List,

    /// Get playlist content (pipe to mdma queue replace to load).
    /// If name is omitted, reads playlist names from stdin (first whitespace-delimited
    /// token per line — handles both bare names and enriched `playlist list` output).
    Get {
        /// Playlist name. Omit to read names from stdin.
        name: Option<PlaylistArg>,
    },

    /// Print the name of every playlist that contains any of the tracks piped in on stdin.
    /// Reads track lines from stdin (8–12 char hex hash as first token).
    /// Output: one playlist name per match; pipe to `sort -u` to deduplicate.
    Contains {
        /// Print playlist only if it contains ALL input tracks
        #[arg(long, conflicts_with_all = ["at_least", "no"])]
        all: bool,
        /// Print playlist only if it contains at least N input tracks
        #[arg(long, conflicts_with_all = ["all", "no"])]
        at_least: Option<AtLeastArg>,
        /// Print playlist only if it contains NONE of the input tracks
        #[arg(long, conflicts_with_all = ["all", "at_least"])]
        no: bool,
    },

    /// Create a new playlist from stdin (fails if it already exists)
    New {
        /// Playlist name
        name: PlaylistArg,
    },

    /// Append stdin to an existing playlist
    Append {
        /// Playlist name
        name: PlaylistArg,
    },

    /// Replace playlist content from stdin
    Replace {
        /// Playlist name
        name: PlaylistArg,
    },

    /// Edit playlist in $EDITOR
    Edit {
        /// Playlist name
        name: PlaylistArg,
    },

    /// Remove a playlist
    Remove {
        /// Playlist name
        name: PlaylistArg,
    },

    /// Rename a playlist
    Rename {
        /// Current playlist name
        from: PlaylistArg,
        /// New playlist name
        to: PlaylistArg,
    },
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::List => list(ctx),
        Command::Get { name } => get(name, ctx),
        Command::Contains { all, at_least, no } => {
            contains(ContainsMode::from_flags(all, at_least, no), ctx)
        }
        Command::New { name } => new(name, ctx),
        Command::Append { name } => append(name, ctx),
        Command::Replace { name } => replace(name, ctx),
        Command::Edit { name } => edit(name, ctx),
        Command::Remove { name } => remove(name, ctx),
        Command::Rename { from, to } => rename(RenameArgs { from, to }, ctx),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameArgs {
    pub(crate) from: PlaylistArg,
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

/// Which playlists `playlist contains` prints, relative to the piped-in tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainsMode {
    Any,
    All,
    AtLeast(NonZeroUsize),
    None,
}

impl ContainsMode {
    /// Build the mode from the `contains` flags. Clap guarantees at most one
    /// is set (they conflict) and that `--at-least` is non-zero.
    fn from_flags(all: bool, at_least: Option<AtLeastArg>, no: bool) -> Self {
        if no {
            Self::None
        } else if all {
            Self::All
        } else if let Some(AtLeastArg(n)) = at_least {
            Self::AtLeast(n)
        } else {
            Self::Any
        }
    }

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

/// Hand a library response to `accept`. An error response, a response it
/// declines, or a failed request is reported and exits 1.
fn expect_library<T>(
    response: std::result::Result<LibraryResponse, ClientError>,
    accept: impl FnOnce(LibraryResponse) -> std::result::Result<T, LibraryResponse>,
) -> T {
    match response {
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        Ok(response) => accept(response).unwrap_or_else(|_| {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }),
        Err(e) => {
            eprintln!("Request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn playlist_content(response: LibraryResponse) -> std::result::Result<String, LibraryResponse> {
    match response {
        LibraryResponse::PlaylistContent(c) => Ok(c),
        other => Err(other),
    }
}

fn playlist_names(
    response: LibraryResponse,
) -> std::result::Result<Vec<PlaylistName>, LibraryResponse> {
    match response {
        LibraryResponse::PlaylistNames(names) => Ok(names),
        other => Err(other),
    }
}

pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{DurationSeconds, LibraryRequest};

    let library = ctx.library();
    let names = expect_library(
        library.request(&LibraryRequest::PlaylistList),
        playlist_names,
    );

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

pub(crate) fn get(name: Option<PlaylistArg>, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library();
    match name {
        Some(PlaylistArg(pname)) => {
            let content = expect_library(
                library.request(&LibraryRequest::PlaylistGet { name: pname }),
                playlist_content,
            );
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
                let content = expect_library(
                    library.request(&LibraryRequest::PlaylistGet { name: pname }),
                    playlist_content,
                );
                print!("{}", content);
            }
        }
    }
    Ok(())
}

pub(crate) fn contains(mode: ContainsMode, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    use std::collections::HashSet;

    let library = ctx.library();

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
    let names = expect_library(
        library.request(&LibraryRequest::PlaylistList),
        playlist_names,
    );

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

pub(crate) fn new(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library();
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string();
    expect_library(
        library.request(&LibraryRequest::PlaylistNew {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    );
    eprintln!("Created playlist '{}'", pname);
    Ok(())
}

pub(crate) fn append(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library();
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string();
    expect_library(
        library.request(&LibraryRequest::PlaylistAppend {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    );
    eprintln!("Appended to playlist '{}'", pname);
    Ok(())
}

pub(crate) fn replace(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library();
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string();
    expect_library(
        library.request(&LibraryRequest::PlaylistReplace {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    );
    eprintln!("Replaced playlist '{}'", pname);
    Ok(())
}

pub(crate) fn edit(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse, ProtocolError};

    let library = ctx.library();
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
    expect_library(
        library.request(&LibraryRequest::PlaylistReplace {
            name: pname.clone(),
            content: new_content,
        }),
        playlist_content,
    );
    eprintln!("Updated playlist '{}'", pname);
    Ok(())
}

pub(crate) fn rename(args: RenameArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let library = ctx.library();
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

pub(crate) fn remove(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let library = ctx.library();
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

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    use crate::{Cli, Commands};
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        let mut argv = vec!["mdma", "playlist"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Commands::Playlist { command } => command,
            other => panic!("expected playlist command, got {other:?}"),
        })
    }

    fn mode(args: &[&str]) -> ContainsMode {
        let mut full = vec!["contains"];
        full.extend_from_slice(args);
        match parse(&full).unwrap() {
            Command::Contains { all, at_least, no } => ContainsMode::from_flags(all, at_least, no),
            other => panic!("expected contains, got {other:?}"),
        }
    }

    fn assert_usage_error(args: &[&str]) {
        let err = parse(args).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
    }

    #[test]
    fn invalid_playlist_name_is_a_usage_error() {
        for cmd in ["new", "append", "replace", "edit", "remove", "get"] {
            assert_usage_error(&[cmd, "has space"]);
            assert_usage_error(&[cmd, "a/b"]);
        }
        assert_usage_error(&["rename", "bad name", "ok"]);
        assert_usage_error(&["rename", "ok", "bad/name"]);
    }

    #[test]
    fn valid_playlist_names_are_accepted() {
        assert!(matches!(
            parse(&["new", "my-list_1"]).unwrap(),
            Command::New { name } if name.0.to_string() == "my-list_1"
        ));
        assert!(matches!(
            parse(&["get"]).unwrap(),
            Command::Get { name: None }
        ));
        assert!(matches!(
            parse(&["get", "mix"]).unwrap(),
            Command::Get { name: Some(_) }
        ));
        match parse(&["rename", "a", "b"]).unwrap() {
            Command::Rename { from, to } => {
                assert_eq!(from.0.to_string(), "a");
                assert_eq!(to.0.to_string(), "b");
            }
            other => panic!("expected rename, got {other:?}"),
        }
    }

    #[test]
    fn contains_conflicting_flags_are_a_usage_error() {
        assert_usage_error(&["contains", "--all", "--no"]);
        assert_usage_error(&["contains", "--all", "--at-least", "2"]);
        assert_usage_error(&["contains", "--no", "--at-least", "2"]);
    }

    #[test]
    fn contains_zero_at_least_is_a_usage_error() {
        for bad in ["0", "abc", "1.5", ""] {
            let err = parse(&["contains", "--at-least", bad]).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{bad}: {err}");
            assert!(
                err.to_string()
                    .contains("must be a whole number of at least 1"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn contains_flags_build_the_right_mode() {
        assert_eq!(mode(&[]), ContainsMode::Any);
        assert_eq!(mode(&["--all"]), ContainsMode::All);
        assert_eq!(mode(&["--no"]), ContainsMode::None);
        assert_eq!(
            mode(&["--at-least", "2"]),
            ContainsMode::AtLeast(NonZeroUsize::new(2).unwrap())
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
