use crate::ctx::Ctx;
use clap::Subcommand;
use color_eyre::Result;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Compute identity hashes for tracks that don't have them yet.
    ///
    /// New tracks get an AudioHash when they are ingested; this covers the tracks
    /// already in the library. It runs as a low-priority background job in the
    /// library service and can be re-run: tracks that already have the hash are
    /// skipped. By default the command follows the job until it finishes.
    Backfill(BackfillArgs),

    /// Show progress of the identity-hash backfill
    Status,

    /// Stop a running identity-hash backfill (progress is kept; run backfill again to resume)
    Cancel,
}

#[derive(clap::Args, Debug, PartialEq)]
pub(crate) struct BackfillArgs {
    /// Also compute PcmHash (decodes every lossless track; slow)
    #[arg(long)]
    pcm: bool,
    /// Start the job and return immediately
    #[arg(long)]
    no_wait: bool,
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::Backfill(args) => backfill(args, ctx),
        Command::Status => status(ctx),
        Command::Cancel => cancel(ctx),
    }
}

const IDENTITY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Consecutive failed status polls before `identity backfill` stops following.
const IDENTITY_POLL_MAX_FAILURES: u32 = 5;

fn format_identity_status(status: &library_ipc_client::IdentityBackfillStatus) -> String {
    use library_ipc_client::IdentityBackfillStatus as S;
    let (label, p) = match status {
        S::Idle => {
            return "No identity backfill has run since the library service started".to_string()
        }
        S::Running(p) => ("Running", p),
        S::Done(p) => ("Done", p),
        S::Cancelled(p) => ("Cancelled", p),
    };
    let hashes = if p.include_pcm {
        "AudioHash + PcmHash"
    } else {
        "AudioHash"
    };
    let mut line = format!("{label} ({hashes}): {}/{} tracks", p.done, p.total);
    if p.failed > 0 {
        line.push_str(&format!(", {} failed", p.failed));
    }
    if p.skipped > 0 {
        line.push_str(&format!(", {} skipped", p.skipped));
    }
    if let Some(err) = &p.last_error {
        line.push_str(&format!(" (last error: {err})"));
    }
    line
}

/// What to tell the user when a backfill is already running. The running job keeps
/// its own mode, so say so when it differs from what was just asked for.
fn identity_already_running_message(running_pcm: bool, requested_pcm: bool) -> String {
    let mode = |pcm| {
        if pcm {
            "AudioHash + PcmHash"
        } else {
            "AudioHash"
        }
    };
    let mut msg = "An identity backfill is already running; not starting another.".to_string();
    if running_pcm != requested_pcm {
        msg.push_str(&format!(
            " The running job computes {}, not the requested {}.",
            mode(running_pcm),
            mode(requested_pcm)
        ));
    }
    msg
}

pub(crate) fn backfill(args: BackfillArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::IdentityBackfillStart;
    let BackfillArgs { pcm, no_wait } = args;
    let library = ctx.library();
    match library.identity_backfill(pcm) {
        Ok(IdentityBackfillStart::Started(p)) => {
            println!("Identity backfill started: {} tracks to process", p.total)
        }
        Ok(IdentityBackfillStart::AlreadyRunning(p)) => {
            println!("{}", identity_already_running_message(p.include_pcm, pcm))
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
    if no_wait {
        println!("Check progress with 'mdma library identity status'");
        return Ok(());
    }

    let mut last_printed = String::new();
    let mut failures = 0;
    loop {
        match library.identity_backfill_status() {
            Ok(s) => {
                failures = 0;
                let line = format_identity_status(&s);
                if line != last_printed {
                    println!("{}", line);
                    last_printed = line;
                }
                if !matches!(s, library_ipc_client::IdentityBackfillStatus::Running(_)) {
                    return Ok(());
                }
            }
            Err(e) => {
                failures += 1;
                if failures >= IDENTITY_POLL_MAX_FAILURES {
                    eprintln!(
                        "Lost contact with the library service ({e}); \
                         check later with 'mdma library identity status'"
                    );
                    std::process::exit(1);
                }
            }
        }
        std::thread::sleep(IDENTITY_POLL_INTERVAL);
    }
}

pub(crate) fn status(ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    match library.identity_backfill_status() {
        Ok(s) => {
            println!("{}", format_identity_status(&s));
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn cancel(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::IdentityBackfillStatus;
    let library = ctx.library();
    match library.identity_backfill_cancel() {
        Ok(IdentityBackfillStatus::Running(_)) => {
            println!(
                "Cancelling after the current track; progress is kept. \
                 Run 'mdma library identity backfill' again to resume."
            );
            Ok(())
        }
        Ok(s) => {
            println!("Nothing to cancel. {}", format_identity_status(&s));
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;

    use crate::Cli;

    use pretty_assertions::assert_eq;

    use crate::library;
    use crate::Commands;

    // ── identity clap parsing ─────────────────────────────────────────────────

    #[test]
    fn identity_backfill_parses_flags() {
        for (args, pcm, no_wait) in [
            (
                vec!["mdma", "library", "identity", "backfill"],
                false,
                false,
            ),
            (
                vec!["mdma", "library", "identity", "backfill", "--pcm"],
                true,
                false,
            ),
            (
                vec![
                    "mdma",
                    "library",
                    "identity",
                    "backfill",
                    "--pcm",
                    "--no-wait",
                ],
                true,
                true,
            ),
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            match cli.command {
                Commands::Library {
                    command:
                        library::Command::Identity {
                            command: Command::Backfill(parsed),
                        },
                } => assert_eq!(parsed, BackfillArgs { pcm, no_wait }),
                other => panic!("unexpected command: {:?}", other),
            }
        }
    }

    #[test]
    fn identity_status_parses() {
        let cli = Cli::try_parse_from(["mdma", "library", "identity", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Library {
                command: library::Command::Identity {
                    command: Command::Status
                }
            }
        ));
    }

    #[test]
    fn already_running_message_names_the_mode_only_when_it_differs() {
        let same = identity_already_running_message(true, true);
        assert!(!same.contains("requested"), "{same}");
        let differs = identity_already_running_message(false, true);
        assert!(
            differs.contains("computes AudioHash, not the requested AudioHash + PcmHash"),
            "{differs}"
        );
    }

    #[test]
    fn identity_cancel_parses() {
        let cli = Cli::try_parse_from(["mdma", "library", "identity", "cancel"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Library {
                command: library::Command::Identity {
                    command: Command::Cancel
                }
            }
        ));
    }

    #[test]
    fn identity_status_formats_progress_failures_and_skips() {
        use library_ipc_client::{IdentityBackfillProgress, IdentityBackfillStatus};
        let p = IdentityBackfillProgress {
            include_pcm: true,
            total: 10,
            done: 10,
            failed: 2,
            skipped: 1,
            last_error: Some("boom".into()),
        };
        assert_eq!(
            format_identity_status(&IdentityBackfillStatus::Done(p.clone())),
            "Done (AudioHash + PcmHash): 10/10 tracks, 2 failed, 1 skipped (last error: boom)"
        );
        assert!(
            format_identity_status(&IdentityBackfillStatus::Cancelled(p)).starts_with("Cancelled")
        );
        assert!(format_identity_status(&IdentityBackfillStatus::Idle).starts_with("No identity"));
    }
}
