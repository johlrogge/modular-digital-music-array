//! Detached background sync (ADR-021 §6): nothing syncs inline.
//!
//! Completion, and commands after they finish, ask for a refresh. A refresh is a
//! child process, `mdma replica sync --quiet`, that outlives the caller and holds
//! none of its pipes. The ask is throttled by the replica's `last_attempt`, which is
//! written *before* the spawn so the 30 s window (backed off to 5 min after failures)
//! holds across shells. One sync per node is enforced by the sync lock, not here.

use std::io;
use std::path::Path;
use std::process::Command;

use fact_replica::{should_attempt, Meta, NodeId, Replica, UnixSecs};
use node_discovery::NodeSpec;

use crate::detach::detach;
use crate::replica::{replica_from_environment, replicas_dir};

/// Where the attempts are recorded when there is no replica to record them in: a
/// node id never starts with a dot, so this is never mistaken for a replica.
const UNRESOLVED_FILE: &str = ".unresolved.json";

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

/// Start `mdma replica sync --quiet` detached, and do not wait for it.
///
/// The `Child` is dropped at once, which neither waits for nor kills it. The child is
/// not reaped by this process: when the caller (a completion or a command, both
/// short-lived) exits, init adopts and reaps it, and until then a child that finishes
/// first is a zombie for a few milliseconds. A double fork would avoid even that, at
/// the price of a second process per refresh; for a process that exits right after
/// spawning it is not worth it.
pub(crate) fn spawn_background_sync(node: Option<&NodeSpec>) -> io::Result<()> {
    // The test binary is not `mdma`: spawning `current_exe()` there would run the
    // test harness with `replica sync --quiet` as its arguments. Tests inject their own
    // spawner into `request_sync`, and `tests/detach.rs` checks `detach` on a stand-in.
    if cfg!(test) {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let child = detach(Command::new(exe).args(sync_args(node))).spawn()?;
    drop(child);
    Ok(())
}

/// How pressing a refresh is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Urgency {
    /// Only when the last attempt is older than the throttle interval.
    WhenStale,
    /// Whatever the interval: something was just written (the replica is behind by
    /// at least that write). The attempt is still recorded.
    Now,
}

/// Ask for a sync of `replica` (`None`: no replica is known for the node in use) and
/// run `spawn` if one is due. Returns whether `spawn` ran.
///
/// The attempt is recorded before `spawn` runs. A replica whose lock is held has a
/// sync running already, so nothing is spawned for it. If the attempt cannot be
/// recorded (a read-only cache, a full disk) nothing is spawned either, because the
/// throttle could not hold and every keystroke would start a process.
///
/// The rule that makes this safe: **never spawn while holding a `SyncLock`.** The
/// decision and the record happen under the lock (`Replica::note_attempt_if`), the
/// lock is released when that returns, and only then does `spawn` run. A child that
/// inherited a held lock would find its own sync "busy" and exit, and the lock would
/// outlive the process that took it.
pub(crate) fn request_sync(
    nodes_dir: &Path,
    replica: Option<&NodeId>,
    now: UnixSecs,
    urgency: Urgency,
    spawn: &mut dyn FnMut(),
) -> bool {
    let due = match replica {
        Some(id) => note_attempt(&Replica::new(nodes_dir, id), now, urgency),
        None => note_unresolved_attempt(nodes_dir, now),
    };
    if due {
        spawn();
    }
    due
}

/// Whether this caller won the attempt. Whether it is due is decided under the
/// replica's lock, so of several shells that all saw a stale replica only one wins.
fn note_attempt(replica: &Replica, now: UnixSecs, urgency: Urgency) -> bool {
    let due = |meta: Option<&Meta>| urgency == Urgency::Now || should_attempt(now, meta);
    replica.note_attempt_if(now, due).unwrap_or(false)
}

/// The same throttle for a node without a replica (first run, or the node in use is
/// not one we have a replica of). The attempts are kept in `nodes_dir` itself, and
/// each one counts as a failure, since no replica has appeared: the interval backs
/// off like it does for a node that cannot be reached. The urgency does not matter
/// here; there is nothing to refresh, only a first sync to start.
fn note_unresolved_attempt(nodes_dir: &Path, now: UnixSecs) -> bool {
    let path = nodes_dir.join(UNRESOLVED_FILE);
    let before: Option<Meta> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    if !should_attempt(now, before.as_ref()) {
        return false;
    }
    let marker = Meta {
        last_attempt: Some(now),
        failures: before.map_or(0, |m| m.failures.saturating_add(1)),
        ..Meta::default()
    };
    let json = serde_json::to_vec(&marker).expect("Meta serializes");
    std::fs::create_dir_all(nodes_dir).is_ok()
        && private_file::write_private_file(&path, &json).is_ok()
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
    use fact_replica::test_support::FakeNode;
    use pretty_assertions::assert_eq;

    fn secs(n: u64) -> UnixSecs {
        UnixSecs::new(n)
    }

    fn id(s: &str) -> NodeId {
        NodeId::new(s).unwrap()
    }

    /// Asks `request_sync` for a refresh of one node (or of none), counting the spawns.
    struct Caller<'a> {
        nodes_dir: &'a Path,
        replica: Option<NodeId>,
        spawns: u32,
    }

    impl<'a> Caller<'a> {
        fn new(nodes_dir: &'a Path, replica: Option<&str>) -> Self {
            Self {
                nodes_dir,
                replica: replica.map(id),
                spawns: 0,
            }
        }

        /// A read at `now`: refreshes only when stale. Whether it spawned.
        fn read(&mut self, now: u64) -> bool {
            self.ask(now, Urgency::WhenStale)
        }

        /// A command that wrote, finishing at `now`. Whether it spawned.
        fn write(&mut self, now: u64) -> bool {
            self.ask(now, Urgency::Now)
        }

        fn ask(&mut self, now: u64, urgency: Urgency) -> bool {
            let mut spawned = false;
            let mut spawn = || spawned = true;
            let replica = self.replica.as_ref();
            let due = request_sync(self.nodes_dir, replica, secs(now), urgency, &mut spawn);
            assert_eq!(due, spawned, "the answer says whether the spawner ran");
            self.spawns += u32::from(spawned);
            spawned
        }
    }

    fn synced_replica(dir: &Path, name: &str, at: u64) -> Replica {
        let replica = Replica::new(dir, &id(name));
        let node = FakeNode::new("e1", 2);
        replica
            .sync(secs(at), None, &mut |c, l, p| node.serve(c, l, p))
            .unwrap();
        replica
    }

    fn last_attempt(replica: &Replica) -> Option<UnixSecs> {
        replica.meta().unwrap().unwrap().last_attempt
    }

    // --- the throttle ------------------------------------------------------------

    #[test]
    fn repeated_requests_spawn_once_per_interval() {
        let dir = tempfile::tempdir().unwrap();
        let replica = synced_replica(dir.path(), "n", 1_000);
        let mut caller = Caller::new(dir.path(), Some("n"));

        // Within 30 s of the last attempt: nothing, however often completion asks.
        for now in [1_000, 1_001, 1_015, 1_029] {
            assert!(!caller.read(now), "{now}");
        }
        assert_eq!(caller.spawns, 0);

        // Due: spawns, and records the attempt first, so the window restarts.
        assert!(caller.read(1_030));
        assert_eq!(last_attempt(&replica), Some(secs(1_030)));
        for now in [1_030, 1_031, 1_059] {
            assert!(!caller.read(now), "{now}");
        }
        assert!(caller.read(1_060));
        assert_eq!(caller.spawns, 2);
    }

    #[test]
    fn failures_back_the_throttle_off() {
        let dir = tempfile::tempdir().unwrap();
        let replica = synced_replica(dir.path(), "n", 1_000);
        // Two failed syncs in a row: the interval is 2 * 2 * 30 = 120 s.
        let mut meta = replica.meta().unwrap().unwrap();
        meta.failures = 2;
        meta.last_attempt = Some(secs(2_000));
        let json = serde_json::to_vec(&meta).unwrap();
        std::fs::write(replica.dir().join("meta.json"), json).unwrap();
        let mut caller = Caller::new(dir.path(), Some("n"));

        assert!(!caller.read(2_119));
        assert!(caller.read(2_120));
    }

    #[test]
    fn a_write_spawns_whatever_the_interval_and_still_records_the_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let replica = synced_replica(dir.path(), "n", 1_000);
        let mut caller = Caller::new(dir.path(), Some("n"));

        assert!(caller.write(1_001));
        assert_eq!(last_attempt(&replica), Some(secs(1_001)));
        // The write's sync is under way: a read right after does not start another.
        assert!(!caller.read(1_002));
    }

    #[test]
    fn a_running_sync_is_not_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let replica = synced_replica(dir.path(), "n", 1_000);
        let _running = replica.try_lock().unwrap().unwrap();
        let mut caller = Caller::new(dir.path(), Some("n"));

        assert!(!caller.read(5_000));
        assert!(!caller.write(5_000));
        assert_eq!(
            last_attempt(&replica),
            Some(secs(1_000)),
            "the running sync records its own attempt"
        );
    }

    #[test]
    fn a_first_run_without_any_replica_is_throttled_with_a_backoff_too() {
        let dir = tempfile::tempdir().unwrap();
        let nodes = dir.path().join("nodes");
        let mut caller = Caller::new(&nodes, None);

        // Nothing exists yet: spawn, then hold off.
        assert!(caller.read(1_000));
        assert!(!caller.read(1_001));
        assert!(!caller.write(1_029), "writes wait too");
        // After 30 s, once more; no replica has appeared, so the next wait is 60 s.
        assert!(caller.read(1_030));
        assert!(!caller.read(1_089));
        assert!(caller.read(1_090));
        assert_eq!(caller.spawns, 3);
        // The record is not a replica.
        let host = fact_replica::Host::new("x");
        assert_eq!(fact_replica::id_for_host(&nodes, &host), None);
    }

    #[test]
    fn an_unwritable_cache_spawns_nothing_instead_of_spawning_every_time() {
        let dir = tempfile::tempdir().unwrap();
        // `nodes` is a file, so no replica directory can be created in it.
        let nodes = dir.path().join("nodes");
        std::fs::write(&nodes, b"").unwrap();

        let mut named = Caller::new(&nodes, Some("n"));
        let mut unnamed = Caller::new(&nodes, None);

        assert!(!named.read(1_000));
        assert!(!unnamed.read(1_000));
    }

    // --- after a command -----------------------------------------------------------

    #[test]
    fn two_requests_at_once_spawn_only_once() {
        let dir = tempfile::tempdir().unwrap();
        synced_replica(dir.path(), "n", 1_000);
        let n = id("n");
        let now = secs(2_000);
        let spawns = std::sync::atomic::AtomicU32::new(0);
        let barrier = std::sync::Barrier::new(8);

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let mut spawn = || {
                        spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    };
                    barrier.wait();
                    request_sync(dir.path(), Some(&n), now, Urgency::WhenStale, &mut spawn);
                });
            }
        });

        assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // --- after a command -----------------------------------------------------------

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

    #[test]
    fn the_child_is_told_the_node_the_command_named() {
        let spec: NodeSpec = "h.local:5555".parse().unwrap();
        assert_eq!(
            sync_args(Some(&spec)),
            ["--node", "h.local:5555", "replica", "sync", "--quiet"]
        );
        assert_eq!(sync_args(None), ["replica", "sync", "--quiet"]);
    }
}
