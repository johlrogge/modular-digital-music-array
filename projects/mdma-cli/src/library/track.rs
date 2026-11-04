use crate::args::TrackArg;
use crate::ctx::Ctx;
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::ContentHash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Soft-delete a track (hidden from default views; file retained; recoverable).
    ///
    /// The track is hidden from list/search results. To undo, use `mdma library track restore <hash>`.
    Delete {
        /// Content hash of the track (prefix accepted, git-style).
        hash: String,
    },

    /// Restore a soft-deleted track (retracts the Deleted fact).
    Restore {
        /// Content hash of the track (prefix accepted, git-style).
        hash: String,
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
        old_hash: String,
        /// Path to the new audio file on the device running the library service.
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
        Command::Delete { hash } => delete(TrackArg(ContentHash::new(hash)), ctx),
        Command::Restore { hash } => restore(TrackArg(ContentHash::new(hash)), ctx),
        Command::Replace { old_hash, new_file } => replace(
            ReplaceArgs {
                old_hash: TrackArg(ContentHash::new(old_hash)),
                new_file,
            },
            ctx,
        ),
        Command::Orphans => orphans(ctx),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplaceArgs {
    pub(crate) old_hash: TrackArg,
    /// Path on the device running the library service, so not a local `PathBuf`.
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

    use clap::Parser;

    use crate::Cli;

    use pretty_assertions::assert_eq;

    use super::Command;
    use crate::library;
    use crate::Commands;

    // ── track lifecycle clap parsing ──────────────────────────────────────────

    #[test]
    fn track_delete_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "library", "track", "delete", "abc123"]);
        assert!(
            result.is_ok(),
            "clap rejected library track delete: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Delete { hash },
                    },
            } => {
                assert_eq!(hash, "abc123");
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_restore_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "library", "track", "restore", "abc123"]);
        assert!(
            result.is_ok(),
            "clap rejected library track restore: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Restore { hash },
                    },
            } => {
                assert_eq!(hash, "abc123");
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_replace_parses_correctly() {
        let result = Cli::try_parse_from([
            "mdma",
            "library",
            "track",
            "replace",
            "abc123",
            "/music/inbox/new.flac",
        ]);
        assert!(
            result.is_ok(),
            "clap rejected library track replace: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    library::Command::Track {
                        command: Command::Replace { old_hash, new_file },
                    },
            } => {
                assert_eq!(old_hash, "abc123");
                assert_eq!(new_file, "/music/inbox/new.flac");
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_orphans_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "library", "track", "orphans"]);
        assert!(
            result.is_ok(),
            "clap rejected library track orphans: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
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
