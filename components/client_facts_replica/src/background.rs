//! Starting the detached background sync (ADR-021 §6): nothing syncs inline.
//!
//! A refresh is a child process, `mdma replica sync --quiet`, that outlives the caller
//! and holds none of its pipes. Whether a refresh is due is the throttle's
//! ([`request_sync`](crate::request_sync)); this is the spawn it takes as its
//! `spawn` argument.

use std::io;
use std::path::Path;
use std::process::{Child, Command};

use node_discovery::NodeSpec;

use crate::detach::detach;

/// The arguments of the background sync: `replica sync --quiet`, for the node the
/// command named with `--node` if it did (the child would otherwise resolve a node of
/// its own, which may be another one).
fn sync_args(node: Option<&NodeSpec>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(node) = node {
        args.push("--node".to_string());
        args.push(node.to_string());
    }
    args.extend(["replica", "sync", "--quiet"].map(String::from));
    args
}

/// Start `mdma replica sync --quiet` detached, and do not wait for it. `mdma` is the
/// path of the `mdma` binary: a client that is not `mdma` itself (the TUI) names it.
///
/// The `Child` is the caller's to deal with. Dropping it neither waits for nor kills
/// the sync; a caller that exits soon (a command, a completion) can drop it, and init
/// adopts and reaps the child. A caller that runs long (the TUI) keeps it and polls
/// `try_wait`, so a finished sync is not a zombie until the caller exits.
///
/// A client that is under test must not call this with its own test binary as `mdma`:
/// that would run the test harness with `replica sync --quiet` as its arguments. Tests
/// inject their own spawner into [`request_sync`](crate::request_sync).
pub fn spawn_background_sync(mdma: &Path, node: Option<&NodeSpec>) -> io::Result<Child> {
    detach(Command::new(mdma).args(sync_args(node))).spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sync_child_is_told_the_node_the_command_named() {
        assert_eq!(sync_args(None), ["replica", "sync", "--quiet"]);
        let node: NodeSpec = "mdma-stage.local:5555".parse().unwrap();
        assert_eq!(
            sync_args(Some(&node)),
            [
                "--node",
                "mdma-stage.local:5555",
                "replica",
                "sync",
                "--quiet"
            ]
        );
    }
}
