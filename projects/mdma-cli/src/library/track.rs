use crate::args::TrackArg;
use crate::ctx::Ctx;
use crate::output::display_error;
use color_eyre::Result;
use hicli::Arg;

/// Path to the new audio file on the device running the library service
// Stays a String: it is a path on the library service's device, not checked locally.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "NEW_FILE")]
pub(crate) struct NewFileArg(pub String);

/// Content hash of the old track (prefix accepted, git-style)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "OLD_HASH")]
pub(crate) struct OldHashArg(pub TrackArg);

#[derive(Debug, Clone, PartialEq, Eq, Arg)]
pub(crate) struct ReplaceArgs {
    pub(crate) old_hash: OldHashArg,
    pub(crate) new_file: NewFileArg,
}

/// Soft-delete a track (hidden from default views; file retained; recoverable)
///
/// The track is hidden from list/search results. To undo, use `mdma library track restore <hash>`.
#[hicli::command]
pub(crate) fn delete(hash: TrackArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let TrackArg(ch) = hash;
    library.track_delete(&ch).map_err(display_error)?;
    println!(
        "soft-deleted; recover with `mdma library track restore {}`",
        ch
    );
    Ok(())
}

/// Restore a soft-deleted track (retracts the Deleted fact)
#[hicli::command]
pub(crate) fn restore(hash: TrackArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let TrackArg(ch) = hash;
    library.track_restore(&ch).map_err(display_error)?;
    println!("track restored");
    Ok(())
}

/// Hard-replace an old track with a new file
///
/// Ingests the new file (path must be on the device running the library service),
/// fully retracts all facts for the old track (the old hash stops resolving - gone
/// from search and list, not merely hidden), records a Replaces provenance link on
/// the new track, and rewrites every playlist that referenced the old track.
/// Playlists that still point at the old hash self-heal on read by following the
/// replacement chain (multi-step chains are followed automatically).
///
/// Note: NEW_FILE must be a path accessible by the library service on the device.
/// Place the new file in the inbox directory or another device-local path first.
#[hicli::command]
pub(crate) fn replace(args: ReplaceArgs, ctx: &Ctx) -> Result<()> {
    let ReplaceArgs {
        old_hash: OldHashArg(TrackArg(old_hash)),
        new_file: NewFileArg(new_file),
    } = args;
    let library = ctx.library()?;
    let (new_hash, playlists) = library
        .track_replace(&old_hash, &new_file)
        .map_err(display_error)?;
    println!("{} -> {}", old_hash, new_hash.as_str());
    println!("{} playlist(s) rewritten", playlists);
    Ok(())
}

/// List blobs on disk that no live track references (GC candidate list, read-only)
///
/// Reports hard-replace leftovers (reason: "no live facts") and soft-deleted tracks
/// (reason: "deleted"). Use `mdma library track delete`/`restore` for soft-delete management.
#[hicli::command]
pub(crate) fn orphans(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::OrphanReason;

    let library = ctx.library()?;
    let orphans = library.track_orphans().map_err(display_error)?;
    if orphans.is_empty() {
        println!("no hidden tracks");
        return Ok(());
    }
    for o in &orphans {
        let hash_short = o
            .content_hash
            .as_str()
            .get(7..15)
            .unwrap_or(o.content_hash.as_str());
        let artist = o.artist.as_deref().unwrap_or("Unknown");
        let title = o.title.as_deref().unwrap_or("Unknown");
        let reason = match &o.reason {
            OrphanReason::Deleted { timestamp } => format!("deleted at {}", timestamp),
            OrphanReason::NoLiveFacts => "no live facts (hard-replace leftover)".to_string(),
        };
        println!("{}  {} - {}  [{}]", hash_short, artist, title, reason);
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use crate::test_support::{parses, usage_error};

    use pretty_assertions::assert_eq;

    use crate::args::TrackArg;
    use library_ipc_client::ContentHash;

    #[test]
    fn replace_args_parser_reads_old_hash_and_new_file() {
        use super::{NewFileArg, OldHashArg, ReplaceArgs};
        use crate::test_support::run_arg;
        assert_eq!(
            run_arg::<ReplaceArgs>(&["abc12345", "/inbox/new.flac"]),
            Ok(ReplaceArgs {
                old_hash: OldHashArg(TrackArg(ContentHash::new("abc12345"))),
                new_file: NewFileArg("/inbox/new.flac".to_string()),
            })
        );
        let err = run_arg::<ReplaceArgs>(&["nothash", "/inbox/new.flac"]).unwrap_err();
        assert!(err.contains("`OLD_HASH`"), "{err}");
        assert!(err.contains("not a content hash"), "{err}");
        assert!(run_arg::<ReplaceArgs>(&["abc12345"]).is_err());
    }

    #[test]
    fn replace_help_says_the_first_positional_is_the_old_hash() {
        let help = crate::test_support::help(&["library", "track", "replace", "--help"]);
        assert!(help.contains("OLD_HASH"), "{help}");
        assert!(help.contains("Content hash of the old track"), "{help}");
        assert!(help.contains("NEW_FILE"), "{help}");
    }

    // ── track lifecycle parsing ───────────────────────────────────────────────

    #[test]
    fn track_delete_parses_correctly() {
        assert_eq!(
            parses(&["library", "track", "delete", "abc12345"]),
            ["library", "track", "delete"]
        );
    }

    #[test]
    fn track_restore_parses_correctly() {
        assert_eq!(
            parses(&["library", "track", "restore", "abc12345"]),
            ["library", "track", "restore"]
        );
    }

    #[test]
    fn track_replace_parses_correctly() {
        assert_eq!(
            parses(&[
                "library",
                "track",
                "replace",
                "abc12345",
                "/music/inbox/new.flac",
            ]),
            ["library", "track", "replace"]
        );
    }

    #[test]
    fn track_hash_args_must_be_valid_hashes() {
        for (verb, extra) in [
            ("delete", None),
            ("restore", None),
            ("replace", Some("/music/inbox/new.flac")),
        ] {
            for bad in ["abc1234", "ABCD1234", "sha256:abc"] {
                let mut argv = vec!["library", "track", verb, bad];
                argv.extend(extra);
                usage_error(&argv);
            }
        }
    }

    #[test]
    fn track_orphans_parses_correctly() {
        assert_eq!(
            parses(&["library", "track", "orphans"]),
            ["library", "track", "orphans"]
        );
    }
}
