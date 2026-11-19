use crate::args::NoWait;
use crate::ctx::Ctx;
use crate::output::{display_error, lost_contact};
use color_eyre::Result;
use hicli::Arg;
use library_ipc_client::IdentityBackfillScope;

/// Also compute PcmHash (decodes every lossless track; slow)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "pcm")]
pub(crate) struct Pcm(pub bool);

/// Also add the exact duration (DurationMs, read from the tags) to tracks that lack it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "duration")]
pub(crate) struct WithDuration(pub bool);

#[derive(Debug, PartialEq, Arg)]
pub(crate) struct BackfillArgs {
    pcm: Pcm,
    duration: WithDuration,
    /// Start the job and return immediately
    no_wait: NoWait,
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
    let mut line = format!("{label} ({}): {}/{} tracks", p.scope, p.done, p.total);
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
fn identity_already_running_message(
    running: IdentityBackfillScope,
    requested: IdentityBackfillScope,
) -> String {
    let mut msg = "An identity backfill is already running; not starting another.".to_string();
    if running != requested {
        msg.push_str(&format!(
            " The running job computes {running}, not the requested {requested}."
        ));
    }
    msg
}

/// A node that predates `--duration` ignores it and answers with a scope without
/// it. The job it started is still useful, but the user asked for more: fail loudly.
fn check_duration_honoured(
    requested: IdentityBackfillScope,
    started: IdentityBackfillScope,
) -> Result<()> {
    if requested.include_duration && !started.include_duration {
        return Err(color_eyre::eyre::eyre!(
            "this node's library service predates --duration; only {started} is being \
             backfilled — upgrade mdma-library"
        ));
    }
    Ok(())
}

/// The failure for a backfill whose status could not be polled any more.
fn lost_contact_with_library(cause: &dyn std::fmt::Display) -> color_eyre::Report {
    lost_contact("the library service", cause, "mdma library identity status")
}

/// Compute identity hashes (and with --duration the exact length) for tracks that lack them
///
/// New tracks get an AudioHash and their exact duration when they are ingested;
/// this covers the tracks already in the library. It runs as a low-priority
/// background job in the library service and can be re-run: tracks that already
/// have the facts are skipped. By default the command follows the job until it
/// finishes.
#[hicli::command]
pub(crate) fn backfill(args: BackfillArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::IdentityBackfillStart;
    let BackfillArgs {
        pcm: Pcm(pcm),
        duration: WithDuration(duration),
        no_wait: NoWait(no_wait),
    } = args;
    let library = ctx.node().library()?;
    let requested = IdentityBackfillScope {
        include_pcm: pcm,
        include_duration: duration,
    };
    match library
        .identity_backfill(requested)
        .map_err(display_error)?
    {
        IdentityBackfillStart::Started(p) => {
            println!("Identity backfill started: {} tracks to process", p.total);
            check_duration_honoured(requested, p.scope)?;
        }
        IdentityBackfillStart::AlreadyRunning(p) => {
            println!("{}", identity_already_running_message(p.scope, requested))
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
                    return Err(lost_contact_with_library(&e));
                }
            }
        }
        std::thread::sleep(IDENTITY_POLL_INTERVAL);
    }
}

/// Show progress of the identity-hash backfill
#[hicli::command]
pub(crate) fn status(ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let status = library.identity_backfill_status().map_err(display_error)?;
    println!("{}", format_identity_status(&status));
    Ok(())
}

/// Stop a running identity-hash backfill (progress is kept; run backfill again to resume)
#[hicli::command]
pub(crate) fn cancel(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::IdentityBackfillStatus;
    let library = ctx.node().library()?;
    match library.identity_backfill_cancel().map_err(display_error)? {
        IdentityBackfillStatus::Running(_) => println!(
            "Cancelling after the current track; progress is kept. \
             Run 'mdma library identity backfill' again to resume."
        ),
        s => println!("Nothing to cancel. {}", format_identity_status(&s)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{failure_text, parses, run_arg};

    use pretty_assertions::assert_eq;

    #[test]
    fn lost_contact_says_why_and_how_to_check() {
        assert_eq!(
            failure_text::<()>(Err(lost_contact_with_library(&"timed out"))),
            "Lost contact with the library service (timed out); \
             check later with 'mdma library identity status'"
        );
    }

    // ── identity parsing ──────────────────────────────────────────────────────

    #[test]
    fn identity_backfill_parses_flags() {
        for (args, pcm, duration, no_wait) in [
            (vec![], false, false, false),
            (vec!["--pcm"], true, false, false),
            (vec!["--duration"], false, true, false),
            (vec!["--pcm", "--duration", "--no-wait"], true, true, true),
        ] {
            assert_eq!(
                run_arg::<BackfillArgs>(&args),
                Ok(BackfillArgs {
                    pcm: Pcm(pcm),
                    duration: WithDuration(duration),
                    no_wait: NoWait(no_wait)
                }),
                "{args:?}"
            );
            let mut argv = vec!["library", "identity", "backfill"];
            argv.extend(&args);
            assert_eq!(parses(&argv), ["library", "identity", "backfill"]);
        }
    }

    #[test]
    fn identity_status_parses() {
        assert_eq!(
            parses(&["library", "identity", "status"]),
            ["library", "identity", "status"]
        );
    }

    #[test]
    fn already_running_message_names_the_mode_only_when_it_differs() {
        let scope = |include_pcm, include_duration| IdentityBackfillScope {
            include_pcm,
            include_duration,
        };
        let same = identity_already_running_message(scope(true, false), scope(true, false));
        assert!(!same.contains("requested"), "{same}");
        let differs = identity_already_running_message(scope(false, false), scope(true, false));
        assert!(
            differs.contains("computes AudioHash, not the requested AudioHash + PcmHash"),
            "{differs}"
        );
        let duration = identity_already_running_message(scope(false, false), scope(false, true));
        assert!(
            duration.contains("computes AudioHash, not the requested AudioHash + DurationMs"),
            "{duration}"
        );
    }

    #[test]
    fn an_older_node_that_ignores_duration_is_a_loud_failure() {
        let asked = IdentityBackfillScope {
            include_pcm: false,
            include_duration: true,
        };
        assert_eq!(
            failure_text::<()>(check_duration_honoured(
                asked,
                IdentityBackfillScope::default()
            )),
            "this node's library service predates --duration; only AudioHash is being \
             backfilled — upgrade mdma-library"
        );
        assert!(check_duration_honoured(asked, asked).is_ok());
        assert!(check_duration_honoured(
            IdentityBackfillScope::default(),
            IdentityBackfillScope::default()
        )
        .is_ok());
    }

    #[test]
    fn identity_cancel_parses() {
        assert_eq!(
            parses(&["library", "identity", "cancel"]),
            ["library", "identity", "cancel"]
        );
    }

    #[test]
    fn identity_status_formats_progress_failures_and_skips() {
        use library_ipc_client::{IdentityBackfillProgress, IdentityBackfillStatus};
        let p = IdentityBackfillProgress {
            scope: IdentityBackfillScope::ALL,
            total: 10,
            done: 10,
            failed: 2,
            skipped: 1,
            last_error: Some("boom".into()),
        };
        assert_eq!(
            format_identity_status(&IdentityBackfillStatus::Done(p.clone())),
            "Done (AudioHash + PcmHash + DurationMs): 10/10 tracks, 2 failed, 1 skipped (last error: boom)"
        );
        assert!(
            format_identity_status(&IdentityBackfillStatus::Cancelled(p)).starts_with("Cancelled")
        );
        assert!(format_identity_status(&IdentityBackfillStatus::Idle).starts_with("No identity"));
    }
}
