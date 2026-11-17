//! The throttle of the background sync (ADR-021 §6).
//!
//! Completion, and commands after they finish, ask for a refresh. The ask is
//! throttled by the replica's `last_attempt`, which is written *before* the spawn so
//! the 30 s window (backed off to 5 min after failures) holds across shells. One sync
//! per node is enforced by the sync lock, not here. Starting the sync is the
//! client's business: it is the `spawn` the caller passes in.

use std::path::Path;

use fact_replica::{should_attempt, Meta, NodeId, Replica, UnixSecs};

/// Where the attempts are recorded when there is no replica to record them in: a
/// node id never starts with a dot, so this is never mistaken for a replica.
const UNRESOLVED_FILE: &str = ".unresolved.json";

/// How pressing a refresh is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
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
pub fn request_sync(
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

/// The clock is injected (`now`), so these tests are deterministic as long as nothing
/// else in the test binary starts a process. The sync lock is an `flock`: a child
/// forked while it is held keeps it alive until the child's `exec`, so a lock that was
/// dropped can still read as busy for a moment, and a throttle that is due reads as
/// "a sync is running" (#107). Keep tests that spawn processes out of this crate.
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
}
