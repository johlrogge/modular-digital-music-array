use crate::args::{HashInput, StdinDash};
use crate::ctx::Ctx;
use crate::output::{display_error, format_track_line, render_track_table};
use crate::stdin::{hashes_in, hashes_of_input, require_stdin_hashes};
use color_eyre::eyre::eyre;
use color_eyre::Result;
use colored::Colorize;
use library_ipc_client::{CanonicalHash, ContentHash, TrackInfo};
use mdma_client::{LibraryBackend, PlaybackBackend, SourceName};
use track_formatter::short_hash;

/// The canonical hash of each of `hashes`, resolved by the library.
///
/// Playback holds only full hashes (ADR-026), so every hash bound for it goes
/// through here first. All are resolved before the caller sends anything: the
/// first that is unknown or ambiguous fails the batch, naming it.
pub(crate) fn resolve_for_playback(
    library: &LibraryBackend,
    hashes: &[ContentHash],
) -> Result<Vec<CanonicalHash>> {
    library.resolve_for_playback(hashes).map_err(display_error)
}

/// Queue `hashes` to play next, keeping their order. Returns how many.
fn queue_next_resolved(
    library: &LibraryBackend,
    playback: &PlaybackBackend,
    hashes: &[ContentHash],
) -> Result<usize> {
    let resolved = resolve_for_playback(library, hashes)?;
    // Prepend in reverse so the first hash ends up at the front of the queue.
    for content_hash in resolved.iter().rev() {
        playback
            .queue_next(content_hash.into(), SourceName::audio())
            .map_err(display_error)?;
    }
    Ok(resolved.len())
}

/// Queue `hashes` at the end. Returns how many.
fn queue_append_resolved(
    library: &LibraryBackend,
    playback: &PlaybackBackend,
    hashes: &[ContentHash],
) -> Result<usize> {
    let resolved = resolve_for_playback(library, hashes)?;
    for content_hash in &resolved {
        playback
            .queue_append(content_hash.into(), SourceName::audio())
            .map_err(display_error)?;
    }
    Ok(resolved.len())
}

/// Remove `hashes` from the queue, however they are spelled. Returns how many went.
fn queue_remove_resolved(
    library: &LibraryBackend,
    playback: &PlaybackBackend,
    hashes: &[ContentHash],
) -> Result<usize> {
    let resolved = resolve_for_playback(library, hashes)?;
    playback
        .queue_remove(resolved.into_iter().map(ContentHash::from).collect())
        .map_err(display_error)
}

/// Make `hashes` the whole queue. Returns how many.
fn queue_replace_resolved(
    library: &LibraryBackend,
    playback: &PlaybackBackend,
    hashes: &[ContentHash],
) -> Result<usize> {
    let entries: Vec<(ContentHash, SourceName)> = resolve_for_playback(library, hashes)?
        .into_iter()
        .map(|hash| (hash.into(), SourceName::audio()))
        .collect();
    let count = entries.len();
    playback.queue_replace(entries).map_err(display_error)?;
    Ok(count)
}

/// Prepend a track to the front of the queue (plays next)
///
/// Give a hash, or `-` to read "{hash}  {display}" lines from stdin (dmenu output).
///
/// # Examples
///
/// ```sh
/// mdma queue next abcd1234
/// mdma search --artist CBL | dmenu | mdma queue next -
/// ```
#[hicli::command]
pub(crate) fn next(hash: HashInput, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let playback = ctx.playback()?;
    let hashes = hashes_of_input(hash)?;
    let count = queue_next_resolved(&library, &playback, &hashes)?;
    println!("Queued {} track(s) next", count);
    Ok(())
}

/// Append a track to the end of the queue
///
/// Give a hash, or `-` to read "{hash}  {display}" lines from stdin (dmenu output).
///
/// # Examples
///
/// ```sh
/// mdma queue append abcd1234
/// mdma search --genre=Techno | mdma queue append -
/// ```
#[hicli::command]
pub(crate) fn append(hash: HashInput, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let playback = ctx.playback()?;
    let hashes = hashes_of_input(hash)?;
    let count = queue_append_resolved(&library, &playback, &hashes)?;
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
/// Give a hash, or `-` to read one hash per line from stdin.
///
/// # Examples
///
/// ```sh
/// mdma queue remove abcd1234
/// mdma queue list | dmenu | mdma queue remove -
/// ```
#[hicli::command]
pub(crate) fn remove(hash: HashInput, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let playback = ctx.playback()?;
    let content_hashes = hashes_of_input(hash)?;

    let removed = queue_remove_resolved(&library, &playback, &content_hashes)?;
    if removed > 0 {
        println!("Removed {} track(s) from queue", removed);
    }
    Ok(())
}

/// Atomically replace the entire queue from stdin
///
/// Give `-` to read playlist-format lines (8-12 char hex hash as first token).
///
/// # Examples
///
/// ```sh
/// mdma search --genre=Techno | shuf | mdma queue replace -
/// cat friday.plist | mdma queue replace -
/// mdma queue list | mdma sort bpm -a - | mdma queue replace -
/// ```
#[hicli::command]
pub(crate) fn replace(_stdin: StdinDash, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let playback = ctx.playback()?;
    let hashes = require_stdin_hashes()?;
    let count = queue_replace_resolved(&library, &playback, &hashes)?;
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
    let count = edit_queue(&library, &playback, |content| {
        crate::editor::edit_text("mdma-queue-", content)
    })?;
    println!("Queue updated: {} tracks", count);
    Ok(())
}

/// Show the queue as playlist text to `edit`, and make what comes back the queue.
/// Returns how many tracks the queue holds. When the new queue is rejected the
/// edited text is kept in a file whose path is in the error.
fn edit_queue(
    library: &LibraryBackend,
    playback: &PlaybackBackend,
    edit: impl FnOnce(&str) -> Result<String>,
) -> Result<usize> {
    // 1. Get current queue hashes.
    let hashes = playback.queue_list().map_err(display_error)?;

    // 2. Look up each track for display info. Unavailable entries are written
    //    with their full hash and a comment, so they survive the round-trip.
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
                // The full hash with an inline comment, so the user can keep or remove it.
                // The first whitespace token is the hash and the comment is ignored when
                // the text is parsed back, so the entry survives the round-trip. (The
                // short form would not: nothing could resolve it once the track is gone.)
                content.push_str(&format!("{hash}  # [unavailable: {e}]"));
            }
        }
        content.push('\n');
    }
    // 4. Let the user edit it (the real one launches $EDITOR, falling back to vi).
    let edited = edit(&content)?;

    // 5. Parse hashes using the standard filter.
    let edited_hashes = hashes_in(&edited);

    // 6. Replace queue with reordered hashes (the lines carry short hashes).
    queue_replace_resolved(library, playback, &edited_hashes).map_err(|e| {
        match crate::editor::keep_text("mdma-queue-", &edited) {
            Some(path) => eyre!("{e}; your edits are kept in {}", path.display()),
            None => e,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{parse, recorded, usage_error, FakeServices};
    use mdma_client::Command;
    use pretty_assertions::assert_eq;

    const A: &str = "sha256:9fb4105e0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5";
    const B: &str = "sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
    const C: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn services() -> FakeServices {
        FakeServices::new(&[A, B, C])
    }

    fn hashes(spelled: &[&str]) -> Vec<ContentHash> {
        spelled.iter().map(|h| ContentHash::new(*h)).collect()
    }

    fn append(hash: &str) -> serde_json::Value {
        recorded(&Command::QueueAppend {
            hash: ContentHash::new(hash),
            source: SourceName::audio(),
        })
    }

    #[test]
    fn append_sends_the_full_hash_for_a_short_one() {
        let fake = services();
        let count =
            queue_append_resolved(&fake.library, &fake.playback, &hashes(&["9fb4105e"])).unwrap();
        assert_eq!(count, 1);
        assert_eq!(fake.sent(), vec![append(A)]);
    }

    #[test]
    fn next_sends_full_hashes_so_the_first_ends_up_in_front() {
        let fake = services();
        queue_next_resolved(
            &fake.library,
            &fake.playback,
            &hashes(&["9fb4105e0a1b", "12345678"]),
        )
        .unwrap();
        let next = |hash: &str| {
            recorded(&Command::QueueNext {
                hash: ContentHash::new(hash),
                source: SourceName::audio(),
            })
        };
        assert_eq!(fake.sent(), vec![next(B), next(A)]);
    }

    #[test]
    fn an_unresolvable_hash_in_a_batch_sends_no_playback_command() {
        let fake = services();
        let err = queue_append_resolved(
            &fake.library,
            &fake.playback,
            &hashes(&["9fb4105e", "deadbeef", "12345678"]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("deadbeef"), "{err}");
        assert_eq!(fake.sent(), Vec::<serde_json::Value>::new());
    }

    #[test]
    fn an_ambiguous_hash_sends_nothing_and_is_named() {
        let fake = FakeServices::new(&[
            A,
            "sha256:9fb4105effffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        ]);
        let err = queue_replace_resolved(&fake.library, &fake.playback, &hashes(&["9fb4105e"]))
            .unwrap_err();
        assert!(err.to_string().contains("9fb4105e"), "{err}");
        assert_eq!(fake.sent(), Vec::<serde_json::Value>::new());
    }

    #[test]
    fn remove_sends_the_full_hash_for_a_short_one() {
        let fake = services();
        let removed =
            queue_remove_resolved(&fake.library, &fake.playback, &hashes(&["9fb4105e"])).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            fake.sent(),
            vec![recorded(&Command::QueueRemove {
                hashes: vec![ContentHash::new(A)]
            })]
        );
    }

    #[test]
    fn replace_and_edit_lines_of_twelve_chars_send_full_hashes() {
        let fake = services();
        // What `replace -` and `edit` hand over: the first token of each line.
        let lines = "9fb4105e0a1b  Artist - Title  [3:45]\n1234567890ab  # [unavailable: x]\n";
        let count = queue_replace_resolved(
            &fake.library,
            &fake.playback,
            &crate::stdin::hashes_in(lines),
        )
        .unwrap();
        assert_eq!(count, 2);
        let entry = |hash: &str| (ContentHash::new(hash), SourceName::audio());
        assert_eq!(
            fake.sent(),
            vec![recorded(&Command::QueueReplace {
                entries: vec![entry(A), entry(B)]
            })]
        );
    }

    const GONE: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    #[test]
    fn edit_round_trip_keeps_an_unavailable_entry_by_its_full_hash() {
        let fake = FakeServices::with_queue(&[A, B], &[A, GONE, B]);
        let mut shown = String::new();
        let count = edit_queue(&fake.library, &fake.playback, |text| {
            shown = text.to_string();
            Ok(text.to_string())
        })
        .unwrap();
        assert_eq!(count, 3);
        assert!(
            shown.contains(&format!("{GONE}  # [unavailable:")),
            "{shown}"
        );
        let entry = |hash: &str| (ContentHash::new(hash), SourceName::audio());
        assert_eq!(
            fake.sent(),
            vec![
                recorded(&Command::QueueList),
                recorded(&Command::QueueReplace {
                    entries: vec![entry(A), entry(GONE), entry(B)]
                })
            ]
        );
    }

    #[test]
    fn a_rejected_edit_names_the_file_that_keeps_it() {
        let fake = FakeServices::with_queue(&[A], &[A]);
        let err = edit_queue(&fake.library, &fake.playback, |_| {
            Ok("deadbeef  my careful work\n".to_string())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("deadbeef"), "{err}");
        let (_, path) = err.split_once("your edits are kept in ").expect(&err);
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "deadbeef  my careful work\n"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn queue_hash_arguments_are_validated_at_parse_time() {
        for sub in ["next", "append", "remove"] {
            let err = usage_error(&["queue", sub, "nothex"]);
            assert!(err.contains("not a content hash"), "{sub}");
            usage_error(&["queue", sub, "ABCD1234"]);
            assert!(parse(&["queue", sub, "abcd1234"]).is_ok());
            assert!(parse(&["queue", sub, "sha256:abcd1234"]).is_ok());
            // Stdin is read only when asked for with `-`.
            assert!(parse(&["queue", sub, "-"]).is_ok(), "{sub} -");
            // Omitted: nothing to act on (stdin is not read unasked).
            let err = usage_error(&["queue", sub]);
            let hint = "`-` reads hashes from stdin";
            assert!(err.contains(hint), "{sub}: {err}");
            // A hash and `-` do not mix.
            usage_error(&["queue", sub, "-", "abcd1234"]);
            usage_error(&["queue", sub, "abcd1234", "-"]);
        }
    }
}
