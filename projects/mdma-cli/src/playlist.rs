use crate::args::parse_playlist_name;
use crate::ctx::Ctx;
use crate::stdin::{parse_hash_from_line, read_stdin_to_string};
use clap::Subcommand;
use color_eyre::Result;
use colored::Colorize;
use library_ipc_client::ContentHash;
use mdma_client::LibraryBackend;

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
    let lib = ctx.library();
    match &command {
        Command::List => handle_playlist_list(&lib),
        Command::Get { name } => handle_playlist_get(&lib, name),
        Command::Contains { all, at_least, no } => {
            handle_playlist_contains(&lib, *all, *at_least, *no)
        }
        Command::New { name } => handle_playlist_new(&lib, name),
        Command::Append { name } => handle_playlist_append(&lib, name),
        Command::Replace { name } => handle_playlist_replace(&lib, name),
        Command::Edit { name } => handle_playlist_edit(&lib, name),
        Command::Remove { name } => handle_playlist_remove(&lib, name),
        Command::Rename { from, to } => handle_playlist_rename(&lib, from, to),
    }
}

// =============================================================================
// Playlist Helpers
// =============================================================================

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

// =============================================================================
// Playlist Command Handlers
// =============================================================================

fn handle_playlist_list(client: &LibraryBackend) -> Result<()> {
    use library_ipc_client::{DurationSeconds, LibraryRequest, LibraryResponse};

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

fn handle_playlist_get(client: &LibraryBackend, name: &Option<String>) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    match name {
        Some(n) => {
            let pname = parse_playlist_name(n);
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

/// Resolve the match threshold and invert flag from the `contains` filter flags.
/// Assumes mutual exclusivity and `at_least > 0` have already been validated.
/// Returns `(threshold, invert)`.
fn resolve_contains_threshold(
    all: bool,
    at_least: Option<usize>,
    no: bool,
    input_len: usize,
) -> (usize, bool) {
    if no {
        (1, true)
    } else if all {
        (input_len, false)
    } else if let Some(n) = at_least {
        (n, false)
    } else {
        (1, false)
    }
}

fn handle_playlist_contains(
    client: &LibraryBackend,
    all: bool,
    at_least: Option<usize>,
    no: bool,
) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    use std::collections::HashSet;
    use std::io::BufRead;

    // Validate mutually exclusive flags
    let flag_count = usize::from(all) + usize::from(at_least.is_some()) + usize::from(no);
    if flag_count > 1 {
        eprintln!("Error: --all, --at-least, and --no are mutually exclusive.");
        std::process::exit(1);
    }

    // Validate --at-least value
    if let Some(0) = at_least {
        eprintln!("Error: --at-least requires a value of at least 1.");
        std::process::exit(1);
    }

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
    let (threshold, invert) = resolve_contains_threshold(all, at_least, no, input_hashes.len());

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

fn handle_playlist_new(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let pname = parse_playlist_name(name);
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistNew {
        name: pname,
        content,
    }));
    eprintln!("Created playlist '{}'", name);
    Ok(())
}

fn handle_playlist_append(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let pname = parse_playlist_name(name);
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistAppend {
        name: pname,
        content,
    }));
    eprintln!("Appended to playlist '{}'", name);
    Ok(())
}

fn handle_playlist_replace(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let pname = parse_playlist_name(name);
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistReplace {
        name: pname,
        content,
    }));
    eprintln!("Replaced playlist '{}'", name);
    Ok(())
}

fn handle_playlist_edit(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse, ProtocolError};

    let pname = parse_playlist_name(name);

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
    let tmp_path = std::env::temp_dir().join(format!("mdma_playlist_{}.plist", name));
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
    eprintln!("Updated playlist '{}'", name);

    // Cleanup
    let _ = std::fs::remove_file(&tmp_path);
    Ok(())
}

fn handle_playlist_rename(client: &LibraryBackend, from: &str, to: &str) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let from_name = parse_playlist_name(from);
    let to_name = parse_playlist_name(to);
    let response = client.request(&LibraryRequest::PlaylistRename {
        from: from_name,
        to: to_name,
    });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Renamed playlist '{}' to '{}'", from, to);
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

fn handle_playlist_remove(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let pname = parse_playlist_name(name);
    let response = client.request(&LibraryRequest::PlaylistRemove { name: pname });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Removed playlist '{}'", name);
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

// Adopted by a later chunk (per-command arg types); unused until then.
#[allow(dead_code)]
/// Name of an MDMA playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Playlist(pub String);

// Adopted by a later chunk (per-command arg types); unused until then.
#[allow(dead_code)]
/// Optional name of an MDMA playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptPlaylist(pub Option<String>);

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    #[test]
    fn contains_threshold_default_is_one_not_inverted() {
        assert_eq!(
            resolve_contains_threshold(false, None, false, 3),
            (1, false)
        );
    }

    #[test]
    fn contains_threshold_all_uses_input_len() {
        assert_eq!(resolve_contains_threshold(true, None, false, 5), (5, false));
    }

    #[test]
    fn contains_threshold_at_least_uses_given_value() {
        assert_eq!(
            resolve_contains_threshold(false, Some(3), false, 7),
            (3, false)
        );
    }

    #[test]
    fn contains_threshold_no_flag_inverts() {
        assert_eq!(resolve_contains_threshold(false, None, true, 4), (1, true));
    }
}
