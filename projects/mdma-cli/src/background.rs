//! Detached background sync (ADR-021 §6): nothing syncs inline.
//!
//! Completion, and commands after they finish, ask for a refresh. A refresh is a
//! child process, `mdma replica sync --quiet`, that outlives the caller and holds
//! none of its pipes. Whether a refresh is due is the throttle's (`client-facts`'
//! [`request_sync`]); this module is the spawn and the rule for which commands ask.

use std::io;
use std::path::PathBuf;

use client_facts::{replica_from_environment, request_sync, UnixSecs, Urgency};
use node_discovery::NodeSpec;

/// The directory the replicas live in. Under test it is `None` unless the test set
/// `XDG_CACHE_HOME`, so no test can read or write the real cache of whoever runs it.
pub(crate) fn replicas_dir() -> Option<PathBuf> {
    if cfg!(test) && std::env::var_os("XDG_CACHE_HOME").is_none_or(|dir| dir.is_empty()) {
        return None;
    }
    client_facts::replicas_dir()
}

/// Start `mdma replica sync --quiet` detached (see
/// [`client_facts::spawn_background_sync`]), and do not wait for it.
pub(crate) fn spawn_background_sync(node: Option<&NodeSpec>) -> io::Result<()> {
    // The test binary is not `mdma`: spawning `current_exe()` there would run the
    // test harness with `replica sync --quiet` as its arguments. Tests inject their own
    // spawner into `request_sync`.
    if cfg!(test) {
        return Ok(());
    }
    // Dropped: the CLI exits soon, and init adopts and reaps the child.
    client_facts::spawn_background_sync(&std::env::current_exe()?, node).map(|_child| ())
}

/// What to do after the command at `path` has run; `None` for the commands that
/// must not start a sync:
///
/// - `replica ...`: `sync` is the refresh itself (it would start itself), and `reset`
///   would be undone at once;
/// - `node ...` and `generate-completions`: no node is involved.
fn refresh_after(path: &[&str]) -> Option<Urgency> {
    match path {
        ["replica", ..] | ["node", ..] | ["generate-completions"] => None,
        path if writes(path) => Some(Urgency::Now),
        _ => Some(Urgency::WhenStale),
    }
}

/// Whether the command at `path` changes what the replica holds or what completion
/// offers (facts, playlists, sources), so the replica is behind once it is done.
/// Reads are everything else: a command missing here costs one sync late, at the
/// next stale check, never a wrong answer. `library` lists its write commands
/// one by one; `library dedup` counts as one because `--apply` does not change the
/// command path.
fn writes(path: &[&str]) -> bool {
    match path {
        ["mark" | "unmark" | "upload", ..] => true,
        ["fact", "add" | "retract"] => true,
        ["library", "reindex-covers"] => true,
        ["library", "track", "delete" | "restore" | "replace"] => true,
        ["library", "identity", "backfill"] => true,
        ["library", "dedup", ..] => true,
        ["library", "archive", "empty"] => true,
        ["inbox", "ingest" | "ingest-all" | "delete"] => true,
        ["playlist", "new" | "append" | "replace" | "edit" | "remove" | "rename"] => true,
        ["rekordbox", "import"] => true,
        ["source", sub, ..] => *sub != "list",
        _ => false,
    }
}

/// The command a command line runs, and the node it named with `--node`.
pub(crate) struct Ran {
    pub(crate) path: Vec<&'static str>,
    pub(crate) node: Option<NodeSpec>,
}

/// The command a command line reaches, if it runs one. `None` for a usage error,
/// `--help`, `--version` and a completion request (the completion functions ask for
/// their own refresh, and parsing a completion twice would run them twice).
pub(crate) fn parse_command(args: &[String]) -> Option<Ran> {
    if args.iter().any(|a| a.starts_with("--bpaf-complete")) {
        return None;
    }
    let (globals, invocation) = crate::app().parse(args.iter().map(String::as_str)).ok()?;
    Some(Ran {
        path: invocation.path().to_vec(),
        node: globals.node.0,
    })
}

/// Once a command has run (anything but a usage error): start a background sync if
/// the replica is behind (a write) or stale. Never waits, never syncs inline. Called
/// once, from `main`, for every command, so no command has to remember to.
///
/// The replica is the one of `--node` when the command named a node (no replica of
/// that name: the unresolved throttle, and no fallback to another node), else the
/// one completion finds offline (`MDMA_NODE`, the remembered node, localhost on the
/// node itself). The child is told the same `--node`.
pub(crate) fn after_command(ran: &Ran) {
    let Some(urgency) = refresh_after(&ran.path) else {
        return;
    };
    let Some(nodes_dir) = replicas_dir() else {
        return;
    };
    let node = ran.node.as_ref();
    let id = replica_from_environment(&nodes_dir, node.map(NodeSpec::host));
    // A failed spawn is not the command's failure; the next attempt tries again.
    let mut spawn = || {
        let _ = spawn_background_sync(node);
    };
    let now = UnixSecs::now();
    request_sync(&nodes_dir, id.as_ref(), now, urgency, &mut spawn);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// What `after_command` would do for a command line (words separated by spaces),
    /// parsed like `main` does.
    fn urgency_of(line: &str) -> Option<Urgency> {
        let args: Vec<String> = line.split_whitespace().map(String::from).collect();
        let ran = parse_command(&args).unwrap_or_else(|| panic!("{line:?} should run"));
        refresh_after(&ran.path)
    }

    #[test]
    fn commands_refresh_when_stale_and_writes_at_once() {
        let reads = [
            "search",
            "list",
            "marked",
            "playlist get -",
            "playlist list",
            "source list",
            "queue list",
            "playback now",
            "library track orphans",
            "library identity status",
            "library identity cancel",
            "library archive",
            "status",
            "get abcd1234",
        ];
        for read in reads {
            assert_eq!(urgency_of(read), Some(Urgency::WhenStale), "{read}");
        }
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let zip = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(zip.path(), b"PK\x03\x04").unwrap();
        let zip = zip.path().display();
        let writes = [
            "mark banger".to_string(),
            "unmark banger".to_string(),
            "bookmark banger".to_string(),
            "fact add --field year --value 2020 abcd1234".to_string(),
            "fact retract --field year --value 2020 abcd1234".to_string(),
            "library reindex-covers".to_string(),
            "library track delete abcd1234".to_string(),
            "library track restore abcd1234".to_string(),
            "library track replace abcd1234 /tmp/new.flac".to_string(),
            "library identity backfill --pcm".to_string(),
            "library dedup".to_string(),
            "library dedup merge abcd1234 bcde2345".to_string(),
            "library archive empty -y".to_string(),
            "inbox ingest a.mp3".to_string(),
            "inbox ingest-all".to_string(),
            "inbox delete a.mp3".to_string(),
            "playlist new set -".to_string(),
            "playlist append set -".to_string(),
            "playlist replace set -".to_string(),
            "playlist edit set".to_string(),
            "playlist remove set".to_string(),
            "playlist rename old new".to_string(),
            format!("rekordbox import {manifest}"),
            "source bandcamp sync".to_string(),
            format!("upload --ssh-key {manifest} {zip}"),
        ];
        for write in &writes {
            assert_eq!(urgency_of(write), Some(Urgency::Now), "{write}");
        }
    }

    #[test]
    fn the_sync_itself_and_node_commands_never_start_a_sync() {
        let none = [
            "replica sync --quiet",
            "replica reset",
            "replica status",
            "node select host",
            "node list",
            "generate-completions bash",
        ];
        for line in none {
            assert_eq!(urgency_of(line), None, "{line}");
        }
    }

    fn parsed(words: &[&str]) -> Option<Ran> {
        let args: Vec<String> = words.iter().map(ToString::to_string).collect();
        parse_command(&args)
    }

    #[test]
    fn the_command_is_known_only_for_a_command_that_runs() {
        let path_of = |words: &[&str]| parsed(words).map(|ran| ran.path);
        assert_eq!(path_of(&["mark", "banger"]), Some(vec!["mark"]));
        let alias = path_of(&["bookmark", "banger"]);
        assert_eq!(alias, Some(vec!["mark"]), "an alias is the command");
        assert_eq!(path_of(&["get", "xyz"]), None, "a usage error");
        assert_eq!(path_of(&["--help"]), None);
        let completion = path_of(&["--bpaf-complete-rev=1", "mark", ""]);
        assert_eq!(completion, None, "a completion request");
    }

    #[test]
    fn the_node_flag_is_carried_along_and_only_when_given() {
        let flagged = parsed(&["--node", "h.local:5555", "replica", "sync"]).unwrap();
        assert_eq!(flagged.path, ["replica", "sync"]);
        assert_eq!(flagged.node.unwrap().to_string(), "h.local:5555");
        let after = parsed(&["mark", "banger", "--node", "h.local"]).unwrap();
        assert_eq!(after.node.unwrap().to_string(), "h.local");
        assert_eq!(parsed(&["mark", "banger"]).unwrap().node, None);
    }
}
