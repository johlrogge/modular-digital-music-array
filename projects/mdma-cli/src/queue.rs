use crate::args::OptTrackArg;
use crate::ctx::Ctx;
use crate::output::{display_error, format_track_line, render_track_table};
use crate::stdin::{hashes_arg_or_stdin, hashes_in, require_stdin_hashes};
use color_eyre::Result;
use colored::Colorize;
use library_ipc_client::{ContentHash, TrackInfo};
use mdma_client::SourceName;
use track_formatter::short_hash;

/// Prepend a track to the front of the queue (plays next)
///
/// Hash may be omitted; if so, reads "{hash}  {display}" from stdin (dmenu output).
#[hicli::command]
pub(crate) fn next(hash: OptTrackArg, ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    let hashes = hashes_arg_or_stdin(hash)?;
    let count = hashes.len();
    // Prepend in reverse so the first hash ends up at the front of the queue.
    for content_hash in hashes.into_iter().rev() {
        playback
            .queue_next(content_hash, SourceName::audio())
            .map_err(display_error)?;
    }
    println!("Queued {} track(s) next", count);
    Ok(())
}

/// Append a track to the end of the queue
///
/// Hash may be omitted; if so, reads "{hash}  {display}" from stdin (dmenu output).
#[hicli::command]
pub(crate) fn append(hash: OptTrackArg, ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    let hashes = hashes_arg_or_stdin(hash)?;
    let count = hashes.len();
    for content_hash in hashes {
        playback
            .queue_append(content_hash, SourceName::audio())
            .map_err(display_error)?;
    }
    println!("Appended {} track(s) to queue", count);
    Ok(())
}

/// Show the current queue
#[hicli::command]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let playback = ctx.playback()?;
    let hashes = playback.queue_list().map_err(display_error)?;

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
        .map(|hash| library.get_track(hash).map_err(|e| e.to_string()))
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

/// Clear the queue
#[hicli::command]
pub(crate) fn clear(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    playback.queue_clear().map_err(display_error)?;
    println!("Queue cleared");
    Ok(())
}

/// Remove track(s) from the queue by hash
///
/// Hash may be omitted; if so, reads one hash per line from stdin.
#[hicli::command]
pub(crate) fn remove(hash: OptTrackArg, ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    let content_hashes = hashes_arg_or_stdin(hash)?;

    let removed = playback
        .queue_remove(content_hashes)
        .map_err(display_error)?;
    if removed > 0 {
        println!("Removed {} track(s) from queue", removed);
    }
    Ok(())
}

/// Atomically replace the entire queue from stdin
///
/// Reads playlist-format lines (8-12 char hex hash as first token).
///
/// Examples:
///   mdma search --genre=Techno | shuf | mdma queue replace
///   cat friday.plist | mdma queue replace
///   mdma queue list | mdma sort bpm -a | mdma queue replace
#[hicli::command]
pub(crate) fn replace(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    let hashes = require_stdin_hashes()?;
    let entries: Vec<(ContentHash, SourceName)> = hashes
        .into_iter()
        .map(|hash| (hash, SourceName::audio()))
        .collect();
    let count = entries.len();
    playback.queue_replace(entries).map_err(display_error)?;
    println!("Queue replaced: {} tracks", count);
    Ok(())
}

/// Edit the queue in $EDITOR (falls back to vi)
///
/// Opens the current queue as a playlist file; save to apply changes.
#[hicli::command]
pub(crate) fn edit(ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let playback = ctx.playback()?;
    // 1. Get current queue hashes.
    let hashes = playback.queue_list().map_err(display_error)?;

    // 2. Look up each track for display info. Unresolvable entries are written as
    //    plain-hash comment lines so they survive the edit round-trip unchanged.
    // 3. Write to temp file in playlist format.
    let mut content = String::from(
        "# MDMA queue — reorder, delete, or add lines. Save to apply.\n\
         # Lines not starting with an 8-12 character lowercase hash followed by a space are ignored.\n\
         \n",
    );
    for hash in &hashes {
        match library.get_track(hash) {
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
    // 4. Launch $EDITOR (fallback to vi) and read back what was saved.
    let edited = crate::editor::edit_text("mdma-queue-", &content)?;

    // 5. Parse hashes using the standard filter.
    let edited_hashes = hashes_in(&edited);

    // 6. Replace queue with reordered hashes.
    let entries: Vec<(ContentHash, SourceName)> = edited_hashes
        .into_iter()
        .map(|hash| (hash, SourceName::audio()))
        .collect();
    let count = entries.len();
    playback.queue_replace(entries).map_err(display_error)?;
    println!("Queue updated: {} tracks", count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::test_support::{parse, usage_error};

    #[test]
    fn queue_hash_arguments_are_validated_at_parse_time() {
        for sub in ["next", "append", "remove"] {
            let err = usage_error(&["queue", sub, "nothex"]);
            assert!(err.contains("not a content hash"), "{sub}");
            usage_error(&["queue", sub, "ABCD1234"]);
            assert!(parse(&["queue", sub, "abcd1234"]).is_ok());
            assert!(parse(&["queue", sub, "sha256:abcd1234"]).is_ok());
            // Omitted: read from stdin.
            assert!(parse(&["queue", sub]).is_ok());
        }
    }
}
