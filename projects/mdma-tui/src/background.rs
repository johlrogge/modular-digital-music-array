//! Asking for a background sync of the replica (ADR-021 §6, ADR-030 §4.5).
//!
//! The TUI never syncs inline. At start and on `:refresh` it asks, and the throttle
//! of `client-facts` decides whether a refresh is due; if it is, `mdma replica sync
//! --quiet` runs detached. The TUI is not `mdma`, so it has to find that binary: next
//! to itself first (a release ships them together), then on `PATH`.

use crate::link::Link;
use client_facts::{replica_from_environment, request_sync, UnixSecs, Urgency};
use mdma_client::NodeSpec;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Child;

/// What the status line says when a sync was due and there is no `mdma` to run.
const MDMA_NOT_FOUND: &str = "mdma not found next to mdma-tui or on PATH: no background sync";

/// Whether `path` is a file somebody may run.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The `mdma` binary: in `exe_dir`, else in a directory of `path`. A file that cannot
/// be run does not count, nor does an empty or relative `PATH` entry (which would
/// find whatever is in the directory the TUI happens to be started in).
fn find_mdma(exe_dir: Option<&Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    let beside = exe_dir.map(Path::to_path_buf).into_iter();
    let on_path = path.into_iter().flat_map(std::env::split_paths);
    beside
        .chain(on_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("mdma"))
        .find(|candidate| is_executable(candidate))
}

/// [`find_mdma`] for this process.
fn mdma_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    find_mdma(
        exe.as_deref().and_then(Path::parent),
        std::env::var_os("PATH").as_deref(),
    )
}

/// The directory the replicas live in. Under test it is `None` unless the test set
/// `XDG_CACHE_HOME`, so no test can read or write the real cache of whoever runs it.
fn replicas_dir() -> Option<PathBuf> {
    if cfg!(test) && std::env::var_os("XDG_CACHE_HOME").is_none_or(|dir| dir.is_empty()) {
        return None;
    }
    client_facts::replicas_dir()
}

/// Start `mdma replica sync --quiet` detached, for `node` if the TUI was given one.
/// The child is the caller's to reap.
fn spawn(mdma: &Path, node: Option<&NodeSpec>) -> Option<Child> {
    // The test binary has no `mdma` beside it, and no test should start a sync.
    if cfg!(test) {
        return None;
    }
    // A failed spawn is not the TUI's failure; the next ask tries again.
    client_facts::spawn_background_sync(mdma, node).ok()
}

/// Ask for a sync of the replica in use (the one of `--node`, else the one found
/// offline); `spawn` runs if the throttle says one is due, and gives the child it
/// started. Also gives the text for the status line when one was due and `find`
/// found no `mdma` to run.
fn request_with(
    nodes_dir: &Path,
    node: Option<&NodeSpec>,
    urgency: Urgency,
    find: impl FnOnce() -> Option<PathBuf>,
    spawn: impl FnOnce(&Path) -> Option<Child>,
) -> (Option<Child>, Option<String>) {
    let id = replica_from_environment(nodes_dir, node.map(NodeSpec::host));
    // `request_sync` takes a `FnMut` that runs at most once: hand it the two once.
    let mut once = Some((find, spawn));
    let mut started = None;
    let mut missing = None;
    let mut due = || {
        if let Some((find, spawn)) = once.take() {
            match find() {
                Some(mdma) => started = spawn(&mdma),
                None => missing = Some(MDMA_NOT_FOUND.to_string()),
            }
        }
    };
    request_sync(nodes_dir, id.as_ref(), UnixSecs::now(), urgency, &mut due);
    (started, missing)
}

/// Ask for a background sync: `Urgency::WhenStale` at start, `Urgency::Now` on
/// `:refresh`. Gives the child that was started, if one was, and the text for the
/// status line when `mdma` cannot be found.
pub fn request(link: &Link, urgency: Urgency) -> (Option<Child>, Option<String>) {
    let Some(nodes_dir) = replicas_dir() else {
        return (None, None);
    };
    let node = link.node_flag();
    request_with(&nodes_dir, node, urgency, mdma_binary, |mdma| {
        spawn(mdma, node)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{catalog, World};
    use std::cell::RefCell;

    fn touch_with_mode(dir: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        touch_with_mode(dir, name, 0o755)
    }

    #[test]
    fn mdma_is_found_beside_the_tui_first_then_on_the_path() {
        let beside = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let on_path = std::env::join_paths([elsewhere.path()]).unwrap();

        assert_eq!(find_mdma(Some(beside.path()), Some(&on_path)), None);

        let far = touch(elsewhere.path(), "mdma");
        assert_eq!(find_mdma(Some(beside.path()), Some(&on_path)), Some(far));

        let near = touch(beside.path(), "mdma");
        assert_eq!(find_mdma(Some(beside.path()), Some(&on_path)), Some(near));
        assert_eq!(find_mdma(None, None), None);
    }

    #[test]
    fn a_file_that_cannot_be_run_is_not_mdma() {
        let beside = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let on_path = std::env::join_paths([elsewhere.path()]).unwrap();
        touch_with_mode(beside.path(), "mdma", 0o644);
        std::fs::create_dir(elsewhere.path().join("mdma")).unwrap();

        assert_eq!(find_mdma(Some(beside.path()), Some(&on_path)), None);
    }

    /// An empty or relative `PATH` entry means the current directory, which is not
    /// where a binary is looked for.
    #[test]
    fn empty_and_relative_path_entries_are_skipped() {
        // Skipped by the text alone: the working directory plays no part.
        let on_path = std::ffi::OsString::from(":.:./bin:bin");
        assert_eq!(find_mdma(None, Some(&on_path)), None);

        let absolute = tempfile::tempdir().unwrap();
        let wanted = touch(absolute.path(), "mdma");
        let on_path = std::env::join_paths([
            Path::new(""),
            Path::new("bin"),
            Path::new("."),
            absolute.path(),
        ])
        .unwrap();

        assert_eq!(find_mdma(None, Some(&on_path)), Some(wanted));
    }

    /// A stale replica is due a sync: with an `mdma` it is spawned, and without one
    /// the status line is told.
    #[test]
    fn a_due_sync_spawns_mdma_or_says_it_cannot_be_found() {
        let world = World::new(Some(catalog()));
        let nodes_dir = client_facts::replicas_dir().unwrap();
        let node: NodeSpec = client_facts::test_support::WORLD_HOST.parse().unwrap();
        let spawned = RefCell::new(None);

        let record = |mdma: &Path| {
            *spawned.borrow_mut() = Some(mdma.to_path_buf());
            None
        };

        let (_, found) = request_with(
            &nodes_dir,
            Some(&node),
            Urgency::Now,
            || Some(PathBuf::from("/opt/mdma")),
            record,
        );
        assert_eq!(found, None);
        assert_eq!(spawned.take(), Some(PathBuf::from("/opt/mdma")));

        let (_, missing) = request_with(&nodes_dir, Some(&node), Urgency::Now, || None, record);
        assert_eq!(missing.as_deref(), Some(MDMA_NOT_FOUND));
        assert_eq!(spawned.take(), None);
        drop(world);
    }

    /// Nothing due, nothing looked for: a fresh attempt is throttled, so a missing
    /// `mdma` is not reported then.
    #[test]
    fn a_throttled_ask_neither_spawns_nor_complains() {
        let world = World::new(Some(catalog()));
        let nodes_dir = client_facts::replicas_dir().unwrap();
        let node: NodeSpec = client_facts::test_support::WORLD_HOST.parse().unwrap();
        let ask = || {
            request_with(
                &nodes_dir,
                Some(&node),
                Urgency::WhenStale,
                || None,
                |_| None,
            )
            .1
        };

        assert_eq!(ask().as_deref(), Some(MDMA_NOT_FOUND), "stale: due");
        assert_eq!(ask(), None, "an attempt was just recorded");
        drop(world);
    }
}
