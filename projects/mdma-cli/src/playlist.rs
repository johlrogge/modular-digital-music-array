use crate::args::{parse_playlist_name, OptPlaylistArg, PlaylistArg};
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
        name: Option<String>,
    },

    /// Print the name of every playlist that contains any of the tracks piped in on stdin.
    /// Reads track lines from stdin (8–12 char hex hash as first token).
    /// Output: one playlist name per match; pipe to `sort -u` to deduplicate.
    Contains {
        /// Print playlist only if it contains ALL input tracks
        #[arg(long)]
        all: bool,
        /// Print playlist only if it contains at least N input tracks
        #[arg(long)]
        at_least: Option<usize>,
        /// Print playlist only if it contains NONE of the input tracks
        #[arg(long)]
        no: bool,
    },

    /// Create a new playlist from stdin (fails if it already exists)
    New {
        /// Playlist name
        name: String,
    },

    /// Append stdin to an existing playlist
    Append {
        /// Playlist name
        name: String,
    },

    /// Replace playlist content from stdin
    Replace {
        /// Playlist name
        name: String,
    },

    /// Edit playlist in $EDITOR
    Edit {
        /// Playlist name
        name: String,
    },

    /// Remove a playlist
    Remove {
        /// Playlist name
        name: String,
    },

    /// Rename a playlist
    Rename {
        /// Current playlist name
        from: String,
        /// New playlist name
        to: String,
    },
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::List => list(ctx),
        // Names are still clap Strings; validated here until wired in P2.1.D.
        Command::Get { name } => get(
            OptPlaylistArg(name.as_deref().map(parse_playlist_name)),
            ctx,
        ),
        Command::Contains { all, at_least, no } => {
            contains(ContainsArgs { all, at_least, no }, ctx)
        }
        Command::New { name } => new(PlaylistArg(parse_playlist_name(&name)), ctx),
        Command::Append { name } => append(PlaylistArg(parse_playlist_name(&name)), ctx),
        Command::Replace { name } => replace(PlaylistArg(parse_playlist_name(&name)), ctx),
        Command::Edit { name } => edit(PlaylistArg(parse_playlist_name(&name)), ctx),
        Command::Remove { name } => remove(PlaylistArg(parse_playlist_name(&name)), ctx),
        Command::Rename { from, to } => rename(
            RenameArgs {
                from: PlaylistArg(parse_playlist_name(&from)),
                to: PlaylistArg(parse_playlist_name(&to)),
            },
            ctx,
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainsArgs {
    pub(crate) all: bool,
    pub(crate) at_least: Option<usize>,
    pub(crate) no: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameArgs {
    pub(crate) from: PlaylistArg,
    pub(crate) to: PlaylistArg,
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
    /// Build the mode from the `contains` flags, or return the error message
    /// to print when they are mutually exclusive or `--at-least` is 0.
    fn from_flags(args: &ContainsArgs) -> std::result::Result<Self, &'static str> {
        let flag_count =
            usize::from(args.all) + usize::from(args.at_least.is_some()) + usize::from(args.no);
        if flag_count > 1 {
            return Err("Error: --all, --at-least, and --no are mutually exclusive.");
        }
        if args.no {
            Ok(Self::None)
        } else if args.all {
            Ok(Self::All)
        } else if let Some(n) = args.at_least {
            NonZeroUsize::new(n)
                .map(Self::AtLeast)
                .ok_or("Error: --at-least requires a value of at least 1.")
        } else {
            Ok(Self::Any)
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

pub(crate) fn get(name: OptPlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library();
    match name.0 {
        Some(pname) => {
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
                let pname = parse_playlist_name(&n);
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

pub(crate) fn contains(args: ContainsArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    use std::collections::HashSet;

    let library = ctx.library();

    // Validate the filter flags
    let mode = ContainsMode::from_flags(&args).unwrap_or_else(|message| {
        eprintln!("{}", message);
        std::process::exit(1);
    });

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

    fn flags(all: bool, at_least: Option<usize>, no: bool) -> ContainsArgs {
        ContainsArgs { all, at_least, no }
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

    #[test]
    fn contains_mode_from_flags() {
        assert_eq!(
            ContainsMode::from_flags(&flags(false, None, false)),
            Ok(ContainsMode::Any)
        );
        assert_eq!(
            ContainsMode::from_flags(&flags(true, None, false)),
            Ok(ContainsMode::All)
        );
        assert_eq!(
            ContainsMode::from_flags(&flags(false, None, true)),
            Ok(ContainsMode::None)
        );
        assert_eq!(
            ContainsMode::from_flags(&flags(false, Some(2), false)),
            Ok(ContainsMode::AtLeast(NonZeroUsize::new(2).unwrap()))
        );
    }

    #[test]
    fn contains_mode_rejects_mutually_exclusive_flags() {
        assert_eq!(
            ContainsMode::from_flags(&flags(true, None, true)),
            Err("Error: --all, --at-least, and --no are mutually exclusive.")
        );
        assert_eq!(
            ContainsMode::from_flags(&flags(false, Some(0), true)),
            Err("Error: --all, --at-least, and --no are mutually exclusive.")
        );
    }

    #[test]
    fn contains_mode_rejects_zero_at_least() {
        assert_eq!(
            ContainsMode::from_flags(&flags(false, Some(0), false)),
            Err("Error: --at-least requires a value of at least 1.")
        );
    }
}
