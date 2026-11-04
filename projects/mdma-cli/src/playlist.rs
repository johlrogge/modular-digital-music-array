use crate::args::parse_playlist_name;
use crate::ctx::Ctx;
use crate::stdin::{parse_hash_from_line, read_stdin_to_string};
use clap::Subcommand;
use color_eyre::Result;
use colored::Colorize;
use library_ipc_client::ContentHash;
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
        Command::Get { name } => get(OptPlaylist(name), ctx),
        Command::Contains { all, at_least, no } => {
            contains(ContainsArgs { all, at_least, no }, ctx)
        }
        Command::New { name } => new(Playlist(name), ctx),
        Command::Append { name } => append(Playlist(name), ctx),
        Command::Replace { name } => replace(Playlist(name), ctx),
        Command::Edit { name } => edit(Playlist(name), ctx),
        Command::Remove { name } => remove(Playlist(name), ctx),
        Command::Rename { from, to } => rename(
            RenameArgs {
                from: Playlist(from),
                to: Playlist(to),
            },
            ctx,
        ),
    }
}

/// Name of an MDMA playlist as typed. Validated into a `PlaylistName` after
/// the library connection is made, so invalid names keep failing at the same
/// point as before (phase-2 parse-time candidate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Playlist(pub String);

impl Playlist {
    fn name(&self) -> library_ipc_client::PlaylistName {
        parse_playlist_name(&self.0)
    }
}

/// Optional name of an MDMA playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptPlaylist(pub Option<String>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainsArgs {
    pub(crate) all: bool,
    pub(crate) at_least: Option<usize>,
    pub(crate) no: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameArgs {
    pub(crate) from: Playlist,
    pub(crate) to: Playlist,
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

fn playlist_expect_content(
    response: std::result::Result<
        library_ipc_client::LibraryResponse,
        library_ipc_client::ClientError,
    >,
) -> String {
    use library_ipc_client::LibraryResponse;
    match response {
        Ok(LibraryResponse::PlaylistContent(c)) => c,
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn playlist_expect_names(
    response: std::result::Result<
        library_ipc_client::LibraryResponse,
        library_ipc_client::ClientError,
    >,
) -> Vec<library_ipc_client::PlaylistName> {
    use library_ipc_client::LibraryResponse;
    match response {
        Ok(LibraryResponse::PlaylistNames(names)) => names,
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn list(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{DurationSeconds, LibraryRequest, LibraryResponse};

    let client = ctx.library();
    let names = playlist_expect_names(client.request(&LibraryRequest::PlaylistList));

    for name in &names {
        let content = match client.request(&LibraryRequest::PlaylistGet { name: name.clone() }) {
            Ok(LibraryResponse::PlaylistContent(c)) => c,
            _ => continue,
        };

        let hashes: Vec<ContentHash> = content
            .lines()
            .filter_map(|line| parse_hash_from_line(line).map(ContentHash::new))
            .collect();

        let track_count = hashes.len();

        let total_secs: u32 = hashes
            .iter()
            .filter_map(|h| {
                let track = client.get_track(h).ok()?;
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

fn get(name: OptPlaylist, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let client = ctx.library();
    match name.0 {
        Some(n) => {
            let pname = parse_playlist_name(&n);
            let content = playlist_expect_content(
                client.request(&LibraryRequest::PlaylistGet { name: pname }),
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
            for n in &names {
                let pname = parse_playlist_name(n);
                let content = playlist_expect_content(
                    client.request(&LibraryRequest::PlaylistGet { name: pname }),
                );
                print!("{}", content);
            }
        }
    }
    Ok(())
}

fn contains(args: ContainsArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    use std::collections::HashSet;
    use std::io::BufRead;

    let client = ctx.library();

    // Validate the filter flags
    let mode = ContainsMode::from_flags(&args).unwrap_or_else(|message| {
        eprintln!("{}", message);
        std::process::exit(1);
    });

    // 1. Read hashes from stdin
    let input_hashes: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    if input_hashes.is_empty() {
        eprintln!("No hashes provided on stdin.");
        std::process::exit(1);
    }

    // 2. Determine threshold and invert
    let (threshold, invert) = mode.threshold(input_hashes.len());

    let input_set: HashSet<&String> = input_hashes.iter().collect();

    // 3. Get all playlist names
    let names = playlist_expect_names(client.request(&LibraryRequest::PlaylistList));

    // 4. For each playlist, count how many input hashes appear in it
    for pname in &names {
        let content = match client.request(&LibraryRequest::PlaylistGet {
            name: pname.clone(),
        }) {
            Ok(LibraryResponse::PlaylistContent(c)) => c,
            _ => continue,
        };

        let playlist_hashes: HashSet<String> =
            content.lines().filter_map(parse_hash_from_line).collect();

        let match_count = input_set
            .iter()
            .filter(|h| playlist_hashes.contains(h.as_str()))
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

fn new(name: Playlist, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let client = ctx.library();
    let pname = name.name();
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistNew {
        name: pname,
        content,
    }));
    eprintln!("Created playlist '{}'", name.0);
    Ok(())
}

fn append(name: Playlist, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let client = ctx.library();
    let pname = name.name();
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistAppend {
        name: pname,
        content,
    }));
    eprintln!("Appended to playlist '{}'", name.0);
    Ok(())
}

fn replace(name: Playlist, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let client = ctx.library();
    let pname = name.name();
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistReplace {
        name: pname,
        content,
    }));
    eprintln!("Replaced playlist '{}'", name.0);
    Ok(())
}

fn edit(name: Playlist, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse, ProtocolError};

    let client = ctx.library();
    let pname = name.name();

    // 1. Get current content (or start empty if not found)
    let current = match client.request(&LibraryRequest::PlaylistGet {
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

    // 2. Write to temp file
    let tmp_path = std::env::temp_dir().join(format!("mdma_playlist_{}.plist", name.0));
    std::fs::write(&tmp_path, &current).unwrap_or_else(|e| {
        eprintln!("Failed to write temp file: {}", e);
        std::process::exit(1);
    });

    // 3. Open in $EDITOR
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to launch editor '{}': {}", editor, e);
            std::process::exit(1);
        });
    if !status.success() {
        eprintln!("Editor exited with non-zero status");
        std::process::exit(1);
    }

    // 4. Read back and replace
    let new_content = std::fs::read_to_string(&tmp_path).unwrap_or_else(|e| {
        eprintln!("Failed to read temp file: {}", e);
        std::process::exit(1);
    });

    playlist_expect_content(client.request(&LibraryRequest::PlaylistReplace {
        name: pname,
        content: new_content,
    }));
    eprintln!("Updated playlist '{}'", name.0);

    // Cleanup
    let _ = std::fs::remove_file(&tmp_path);
    Ok(())
}

fn rename(args: RenameArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let client = ctx.library();
    let from_name = args.from.name();
    let to_name = args.to.name();
    let response = client.request(&LibraryRequest::PlaylistRename {
        from: from_name,
        to: to_name,
    });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Renamed playlist '{}' to '{}'", args.from.0, args.to.0);
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

fn remove(name: Playlist, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let client = ctx.library();
    let pname = name.name();
    let response = client.request(&LibraryRequest::PlaylistRemove { name: pname });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Removed playlist '{}'", name.0);
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
