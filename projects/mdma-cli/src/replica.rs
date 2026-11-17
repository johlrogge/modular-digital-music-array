//! `mdma replica`: the client-side copy of a node's fact log (ADR-021).
//!
//! The replica itself (which one, its sync, the probes and the snapshot) is the
//! `client-facts` component's; this module is the commands.

use color_eyre::Result;
use hicli::Arg;

use crate::ctx::Ctx;

/// Print nothing unless something fails
#[derive(Debug, Clone, Arg)]
#[arg(long = "quiet")]
pub(crate) struct Quiet(pub bool);

/// Bring the replica up to date with the node
///
/// Reads the node's fact log into the local replica, then probes the playlist and
/// source names. Meant to run detached; a sync already running for the node makes
/// this one exit at once.
#[hicli::command(hide)]
pub(crate) fn sync(quiet: Quiet, ctx: &Ctx) -> Result<()> {
    let (id, report) = client_facts::sync(ctx, quiet.0)?;
    let Some(report) = report else {
        if !quiet.0 {
            println!("Another sync of {id} is running.");
        }
        return Ok(());
    };
    if !quiet.0 {
        let how = if report.reset { ", refetched" } else { "" };
        println!(
            "Synced {id}: {} new lines, {} in total{how}",
            report.added, report.total
        );
    }
    Ok(())
}

/// Show the replica of the current node
///
/// Which node is decided offline: `--node`; on the node itself, the replica synced
/// as localhost; else `MDMA_NODE`; else the remembered node (when `MDMA_NODE` is
/// unset or names it).
#[hicli::command]
pub(crate) fn status(ctx: &Ctx) -> Result<()> {
    println!("{}", client_facts::status(ctx)?);
    Ok(())
}

/// Delete the replica of the current node
///
/// Nothing is lost: the next sync rebuilds it from the node. Name another node
/// with `--node`. A node that has no replica is not an error: a second reset
/// says there is nothing to reset.
#[hicli::command]
pub(crate) fn reset(ctx: &Ctx) -> Result<()> {
    println!("{}", client_facts::reset(ctx)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::test_support::{help, parses};

    #[test]
    fn the_commands_parse() {
        assert_eq!(parses(&["replica", "sync", "--quiet"]), ["replica", "sync"]);
        assert_eq!(parses(&["replica", "sync"]), ["replica", "sync"]);
        assert_eq!(parses(&["replica", "status"]), ["replica", "status"]);
        assert_eq!(parses(&["replica", "reset"]), ["replica", "reset"]);
        assert_eq!(
            parses(&["replica", "reset", "--node", "mdma-stage.local"]),
            ["replica", "reset"]
        );
    }

    #[test]
    fn sync_is_hidden_from_help() {
        let help = help(&["replica", "--help"]);
        // A command is listed on a line that starts with its name.
        let lists = |name: &str| help.lines().any(|l| l.trim_start().starts_with(name));
        assert!(lists("status") && lists("reset"), "{help}");
        assert!(!lists("sync"), "{help}");
    }
}
