use crate::args::TrackArg;
use crate::ctx::Ctx;
use bpaf::Bpaf;
use clap::Subcommand;
use color_eyre::Result;
use hicli::Arg;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Soft-delete a track (hidden from default views; file retained; recoverable).
    ///
    /// The track is hidden from list/search results. To undo, use `mdma library track restore <hash>`.
    Delete {
        /// Content hash of the track (prefix accepted, git-style).
        hash: TrackArg,
    },

    /// Restore a soft-deleted track (retracts the Deleted fact).
    Restore {
        /// Content hash of the track (prefix accepted, git-style).
        hash: TrackArg,
    },

    /// Hard-replace an old track with a new file.
    ///
    /// Ingests the new file (path must be on the device running the library service),
    /// fully retracts all facts for the old track (the old hash stops resolving — gone
    /// from search and list, not merely hidden), records a Replaces provenance link on
    /// the new track, and rewrites every playlist that referenced the old track.
    /// Playlists that still point at the old hash self-heal on read by following the
    /// replacement chain (multi-step chains are followed automatically).
    ///
    /// Note: <new-file> must be a path accessible by the library service on the device.
    /// Place the new file in the inbox directory or another device-local path first.
    Replace {
        /// Content hash of the old track (prefix accepted, git-style).
        old_hash: TrackArg,
        /// Path to the new audio file on the device running the library service.
        // A path on the service's device, so not checked here and not an ExistingFileArg.
        new_file: String,
    },

    /// List blobs on disk that no live track references (GC candidate list, read-only).
    ///
    /// Reports hard-replace leftovers (reason: "no live facts") and soft-deleted tracks
    /// (reason: "deleted"). Use `mdma library track delete`/`restore` for soft-delete management.
    Orphans,
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::Delete { hash } => delete(hash, ctx),
        Command::Restore { hash } => restore(hash, ctx),
        Command::Replace { old_hash, new_file } => replace(ReplaceArgs { old_hash, new_file }, ctx),
        Command::Orphans => orphans(ctx),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Bpaf, Arg)]
pub(crate) struct ReplaceArgs {
    /// Content hash of the old track (prefix accepted, git-style).
    #[bpaf(positional("OLD_HASH"))]
    pub(crate) old_hash: TrackArg,
    // Stays a String: it is a path on the library service's device, not checked locally.
    /// Path to the new audio file on the device running the library service.
    #[bpaf(positional("NEW_FILE"))]
    pub(crate) new_file: String,
}

pub(crate) fn delete(hash: TrackArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    let TrackArg(ch) = hash;
    match library.track_delete(&ch) {
        Ok(()) => {
            println!(
                "soft-deleted; recover with `mdma library track restore {}`",
                ch
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn restore(hash: TrackArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    let TrackArg(ch) = hash;
    match library.track_restore(&ch) {
        Ok(()) => {
            println!("track restored");
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn replace(args: ReplaceArgs, ctx: &Ctx) -> Result<()> {
    let ReplaceArgs {
        old_hash: TrackArg(old_hash),
        new_file,
    } = args;
    let library = ctx.library();
    match library.track_replace(&old_hash, &new_file) {
        Ok((new_hash, playlists)) => {
            println!("{} -> {}", old_hash, new_hash.as_str());
            println!("{} playlist(s) rewritten", playlists);
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn orphans(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::OrphanReason;

    let library = ctx.library();
    match library.track_orphans() {
        Ok(orphans) => {
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
                    OrphanReason::NoLiveFacts => {
                        "no live facts (hard-replace leftover)".to_string()
                    }
                };
                println!("{}  {} - {}  [{}]", hash_short, artist, title, reason);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::test_support::{parses, usage_error};

    use pretty_assertions::assert_eq;

    use super::Command;
    use crate::args::TrackArg;
    use crate::library;
    use crate::Commands;
    use library_ipc_client::ContentHash;

    #[test]
    fn replace_args_parser_reads_old_hash_and_new_file() {
        use super::ReplaceArgs;
        use crate::test_support::run_arg;
        assert_eq!(
            run_arg::<ReplaceArgs>(&["abc12345", "/inbox/new.flac"]),
            Ok(ReplaceArgs {
                old_hash: TrackArg(ContentHash::new("abc12345")),
                new_file: "/inbox/new.flac".to_string(),
            })
        );
        let err = run_arg::<ReplaceArgs>(&["nothash", "/inbox/new.flac"]).unwrap_err();
        assert!(err.contains("is not a content hash"), "{err}");
        assert!(run_arg::<ReplaceArgs>(&["abc12345"]).is_err());
    }

    // ── track lifecycle clap parsing ──────────────────────────────────────────

    #[test]
    fn track_delete_parses_correctly() {
        match parses(&["library", "track", "delete", "abc12345"]) {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Delete { hash },
                    },
            } => {
                assert_eq!(hash, TrackArg(ContentHash::new("abc12345")));
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_restore_parses_correctly() {
        match parses(&["library", "track", "restore", "abc12345"]) {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Restore { hash },
                    },
            } => {
                assert_eq!(hash, TrackArg(ContentHash::new("abc12345")));
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_replace_parses_correctly() {
        match parses(&[
            "library",
            "track",
            "replace",
            "abc12345",
            "/music/inbox/new.flac",
        ]) {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Replace { old_hash, new_file },
                    },
            } => {
                assert_eq!(old_hash, TrackArg(ContentHash::new("abc12345")));
                assert_eq!(new_file, "/music/inbox/new.flac");
            }
            other => panic!("unexpected command: {:?}", other),
        }
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
        match parses(&["library", "track", "orphans"]) {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Orphans,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }
}
