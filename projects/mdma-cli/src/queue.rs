use crate::ctx::Ctx;
use crate::output::{format_track_line, handle_playback_error, render_track_table};
use crate::stdin::{hashes_arg_or_stdin, parse_hash_from_line};
use clap::Subcommand;
use color_eyre::Result;
use colored::Colorize;
use library_ipc_client::{ContentHash, TrackInfo};
use mdma_client::SourceName;
use track_formatter::short_hash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Prepend a track to the front of the queue (plays next).
    /// Hash may be omitted; if so, reads "{hash}  {display}" from stdin (dmenu output).
    Next {
        /// Content hash (full or partial). Omit to read from stdin.
        hash: Option<String>,
    },

    /// Append a track to the end of the queue.
    /// Hash may be omitted; if so, reads "{hash}  {display}" from stdin (dmenu output).
    Append {
        /// Content hash (full or partial). Omit to read from stdin.
        hash: Option<String>,
    },

    /// Show the current queue
    List,

    /// Clear the queue
    Clear,

    /// Remove track(s) from the queue by hash.
    /// Hash may be omitted; if so, reads one hash per line from stdin.
    Remove {
        /// Content hash (full or partial). Omit to read from stdin.
        hash: Option<String>,
    },

    /// Atomically replace the entire queue from stdin.
    /// Reads playlist-format lines (8–12 char hex hash as first token).
    ///
    /// Examples:
    ///   mdma search --genre=Techno | shuf | mdma queue replace
    ///   cat friday.plist | mdma queue replace
    ///   mdma queue list | mdma sort bpm -a | mdma queue replace
    Replace,

    /// Edit the queue in $EDITOR (falls back to vi).
    /// Opens the current queue as a playlist file; save to apply changes.
    Edit,
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::Next { hash } => next(OptTrackHash(hash), ctx),
        Command::Append { hash } => append(OptTrackHash(hash), ctx),
        Command::List => list(ctx),
        Command::Clear => clear(ctx),
        Command::Remove { hash } => remove(OptTrackHash(hash), ctx),
        Command::Replace => replace(ctx),
        Command::Edit => edit(ctx),
    }
}

pub(crate) fn next(hash: OptTrackHash, ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    let hashes = hashes_arg_or_stdin(hash.0);
    let count = hashes.len();
    // Prepend in reverse so the first hash ends up at the front of the queue.
    for hash in hashes.into_iter().rev() {
        let content_hash = ContentHash::new(hash);
        if let Err(e) = media_client.queue_next(content_hash, SourceName::audio()) {
            handle_playback_error(e);
        }
    }
    println!("Queued {} track(s) next", count);
    Ok(())
}

pub(crate) fn append(hash: OptTrackHash, ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    let hashes = hashes_arg_or_stdin(hash.0);
    let count = hashes.len();
    for hash in hashes {
        let content_hash = ContentHash::new(hash);
        if let Err(e) = media_client.queue_append(content_hash, SourceName::audio()) {
            handle_playback_error(e);
        }
    }
    println!("Appended {} track(s) to queue", count);
    Ok(())
}

pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let library_client = ctx.library();
    let media_client = ctx.playback();
    let hashes = match media_client.queue_list() {
        Ok(h) => h,
        Err(e) => handle_playback_error(e),
    };

    if hashes.is_empty() {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!("Queue is empty");
        }
        return Ok(());
    }

    // Resolve each hash, keeping a Result per slot so positions stay aligned.
    let resolved: Vec<Result<TrackInfo, String>> = hashes
        .iter()
        .map(|hash| library_client.get_track(hash).map_err(|e| e.to_string()))
        .collect();

    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();

    if is_tty {
        println!("{}", format!("Queue ({} tracks)", hashes.len()).bold());
        println!();

        let pos_width = hashes.len().to_string().len().max(1);
        // Collect only the resolvable tracks so we can size columns together.
        let resolvable: Vec<TrackInfo> = resolved
            .iter()
            .filter_map(|r| r.as_ref().ok().cloned())
            .collect();
        // Pre-render resolvable tracks into lines (indexed by their slot).
        let mut resolvable_lines = render_track_table(&resolvable, pos_width + 3).into_iter();

        for (i, slot) in resolved.iter().enumerate() {
            let pos = i + 1;
            let pos_str = format!("{:>width$}.", pos, width = pos_width)
                .bright_black()
                .to_string();
            match slot {
                Ok(_) => {
                    let line = resolvable_lines.next().unwrap_or_default();
                    println!("{}  {}", pos_str, line);
                }
                Err(e) => {
                    let short = short_hash(&hashes[i]);
                    println!(
                        "{}  {}  {}",
                        pos_str,
                        short.bright_black(),
                        format!("[unavailable: {}]", e).bright_red()
                    );
                }
            }
        }
    } else {
        for (i, slot) in resolved.iter().enumerate() {
            match slot {
                Ok(track) => println!("{}", format_track_line(track)),
                Err(e) => {
                    let short = short_hash(&hashes[i]);
                    println!("{}  [unavailable: {}]", short, e);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn remove(hash: OptTrackHash, ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    let hashes = hashes_arg_or_stdin(hash.0);
    let content_hashes: Vec<ContentHash> = hashes.into_iter().map(ContentHash::new).collect();

    if content_hashes.is_empty() {
        return Ok(());
    }

    match media_client.queue_remove(content_hashes) {
        Ok(removed) if removed > 0 => {
            println!("Removed {} track(s) from queue", removed);
        }
        Ok(_) => {}
        Err(e) => handle_playback_error(e),
    }
    Ok(())
}

pub(crate) fn clear(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    if let Err(e) = media_client.queue_clear() {
        handle_playback_error(e);
    }
    println!("Queue cleared");
    Ok(())
}

pub(crate) fn replace(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    let hashes = hashes_arg_or_stdin(None);
    let entries: Vec<(ContentHash, SourceName)> = hashes
        .into_iter()
        .map(|hash| (ContentHash::new(hash), SourceName::audio()))
        .collect();
    let count = entries.len();
    if let Err(e) = media_client.queue_replace(entries) {
        handle_playback_error(e);
    }
    println!("Queue replaced: {} tracks", count);
    Ok(())
}

pub(crate) fn edit(ctx: &Ctx) -> Result<()> {
    let library_client = ctx.library();
    let media_client = ctx.playback();
    // 1. Get current queue hashes.
    let hashes = match media_client.queue_list() {
        Ok(h) => h,
        Err(e) => handle_playback_error(e),
    };

    // 2. Look up each track for display info. Unresolvable entries are written as
    //    plain-hash comment lines so they survive the edit round-trip unchanged.
    // 3. Write to temp file in playlist format.
    let tmp_path = std::env::temp_dir().join("mdma_queue_edit.plist");
    let mut content = String::from(
        "# MDMA queue — reorder, delete, or add lines. Save to apply.\n\
         # Lines not starting with an 8-12 character lowercase hash followed by a space are ignored.\n\
         \n",
    );
    for hash in &hashes {
        match library_client.get_track(hash) {
            Ok(track) => {
                content.push_str(&format_track_line(&track));
            }
            Err(e) => {
                // Write the raw hash with an inline comment so the user can keep or remove it.
                // parse_hash_from_line reads only the first whitespace token, so the comment is ignored
                // when the file is parsed back — the hash will survive the round-trip.
                content.push_str(&format!("{}  # [unavailable: {}]", short_hash(hash), e));
            }
        }
        content.push('\n');
    }
    std::fs::write(&tmp_path, &content)
        .map_err(|e| {
            eprintln!("Failed to write temp file: {}", e);
            std::process::exit(1);
        })
        .unwrap();

    // 4. Launch $EDITOR (fallback to vi).
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .map_err(|e| {
            eprintln!("Failed to launch editor '{}': {}", editor, e);
            std::process::exit(1);
        })
        .unwrap();

    if !status.success() {
        eprintln!("Editor exited with non-zero status");
        std::process::exit(1);
    }

    // 5. Read back, parse hashes using the standard filter.
    use std::io::BufRead;
    let file = std::fs::File::open(&tmp_path)
        .map_err(|e| {
            eprintln!("Failed to read temp file: {}", e);
            std::process::exit(1);
        })
        .unwrap();
    let edited_hashes: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    let _ = std::fs::remove_file(&tmp_path);

    // 6. Replace queue with reordered hashes.
    let entries: Vec<(ContentHash, SourceName)> = edited_hashes
        .into_iter()
        .map(|hash| (ContentHash::new(hash), SourceName::audio()))
        .collect();
    let count = entries.len();
    if let Err(e) = media_client.queue_replace(entries) {
        handle_playback_error(e);
    }
    println!("Queue updated: {} tracks", count);
    Ok(())
}

/// Optional content hash of a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptTrackHash(pub Option<String>);
