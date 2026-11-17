//! Helpers for tests, here and in the clients, behind the `test-support` feature.

/// Held by every test that sets an environment variable (`MDMA_NODE`, `EDITOR`,
/// `COLUMNS`, `XDG_CACHE_HOME`): the environment is process-wide and tests run in
/// parallel.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Environment variables set for the length of a test and put back when it ends,
/// even if it panics. Hold [`env_lock`] first (declare it first, so it drops last).
#[derive(Default)]
pub struct EnvVars(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvVars {
    pub fn set(&mut self, name: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        self.0.push((name, std::env::var_os(name)));
        std::env::set_var(name, value);
    }
}

impl EnvVars {
    /// Remove `name` from the environment until this is dropped.
    pub fn unset(&mut self, name: &'static str) {
        self.0.push((name, std::env::var_os(name)));
        std::env::remove_var(name);
    }
}

impl Drop for EnvVars {
    fn drop(&mut self) {
        for (name, original) in self.0.drain(..).rev() {
            match original {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// An environment that reads nothing of the user's: the cache and the config are
/// under `dir` and no node is named (`MDMA_NODE` is empty). Hold [`env_lock`] first.
pub fn offline_env(dir: &std::path::Path) -> EnvVars {
    let mut env = EnvVars::default();
    env.set("XDG_CACHE_HOME", dir.join("cache"));
    env.set("XDG_CONFIG_HOME", dir.join("config"));
    env.set("MDMA_NODE", "");
    env
}

/// Where the replicas are kept when [`offline_env`] is set for `dir`.
pub fn nodes_dir(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("cache").join("mdma").join("nodes")
}

/// Sync a replica of node `node`, reached as `host`, holding `lines`, into `nodes_dir`.
pub fn replica_with_lines(nodes_dir: &std::path::Path, node: &str, host: &str, lines: Vec<String>) {
    replica_with_lines_at(nodes_dir, node, host, lines, 1);
}

/// [`replica_with_lines`], synced at `synced_at` (seconds since the epoch). Synced
/// just now, the throttle of the background sync declines, so a command run against
/// it spawns no detached sync.
pub fn replica_with_lines_at(
    nodes_dir: &std::path::Path,
    node: &str,
    host: &str,
    lines: Vec<String>,
    synced_at: u64,
) {
    let mut fake = fact_replica::test_support::FakeNode::new("e1", 0);
    fake.lines = lines;
    let id = fact_replica::NodeId::new(node).expect("a usable node id");
    fact_replica::Replica::new(nodes_dir, &id)
        .sync(
            fact_replica::UnixSecs::new(synced_at),
            Some(&fact_replica::Host::new(host)),
            &mut |cursor, limit, page| fake.serve(cursor, limit, page),
        )
        .expect("the fake node syncs");
}

/// Record, through the real throttle, a fresh attempt at a first sync (one for no
/// replica) in the cache [`offline_env`] sets for `dir`, as a client that has just asked
/// for one would. Until the throttle's back-off has passed, a client run against that
/// cache asks for no sync, so no scenario spawns a detached `mdma replica sync` that
/// outlives it. Asking again is harmless: the second ask declines.
pub fn decline_first_sync(dir: &std::path::Path) {
    let mut spawn = || {};
    crate::request_sync(
        &nodes_dir(dir),
        None,
        fact_replica::UnixSecs::now(),
        crate::Urgency::Now,
        &mut spawn,
    );
}

/// One line of a fact log, as a replica stores it: an assertion of the JSON `value`
/// (`{"t": .., "v": ..}`) about `entity` at `time` (RFC 3339).
pub fn asserted(entity: &str, value: &str, time: &str) -> String {
    let source = r#"{"tool":"t","version":"1","origin":{"type":"User"}}"#;
    format!(r#"["{entity}",{value},"{time}",{source},"Assert"]"#)
}

/// The host the replica of a [`World`] was synced from, and the node it names.
pub const WORLD_HOST: &str = "mdma-stage2.local";

/// The cache and config in a temp dir, and the replica of some lines synced into it
/// (none when there are none), for tests of the code that reads facts without a node.
/// Holds [`env_lock`] for as long as it lives. Fields drop in order: the environment
/// is put back before the lock is released.
pub struct World {
    _env: EnvVars,
    dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl World {
    /// A world whose replica (of node `stage`, synced from [`WORLD_HOST`]) holds
    /// `lines`; with `None` there is no replica.
    pub fn new(lines: Option<Vec<String>>) -> Self {
        let lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let env = offline_env(dir.path());
        let world = World {
            _env: env,
            dir,
            _lock: lock,
        };
        if let Some(lines) = lines {
            world.sync(lines);
        }
        world
    }

    /// Sync `lines` into the replica, as a sync from the node would.
    pub fn sync(&self, lines: Vec<String>) {
        replica_with_lines(&nodes_dir(self.dir.path()), "stage", WORLD_HOST, lines);
    }

    /// The temp dir everything is under.
    pub fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Options that name [`WORLD_HOST`] and put the library and playback sockets under
    /// [`World::dir`] as `library.sock` and `playback.sock`.
    pub fn options(&self) -> mdma_client::NodeOptions {
        let socket = |name: &str| format!("ipc://{}/{name}.sock", self.dir.path().display());
        mdma_client::NodeOptions {
            node: Some(WORLD_HOST.parse().expect("a node name")),
            library_socket: Some(socket("library")),
            playback_socket: Some(socket("playback")),
            ..mdma_client::NodeOptions::default()
        }
    }

    /// A node with no service behind it (unless a test starts one at its sockets):
    /// using it fails.
    pub fn dead_node(&self) -> mdma_client::Node {
        mdma_client::Node::with_target(self.options(), node_discovery::Target::LocalIpc)
    }

    /// A node whose pick prompt the user cancelled.
    pub fn cancelled_node(&self) -> mdma_client::Node {
        let mut effects = mdma_client::node::NodeEffects::real();
        effects.resolve = Box::new(|_, _, _, _| {
            Err(mdma_client::NodeError::Resolve(
                node_discovery::ResolveError::Cancelled,
            ))
        });
        mdma_client::Node::needed_by(mdma_client::NeededBy::new("mdma-tui"), self.options())
            .with_effects(effects)
    }
}

/// The text of a failed result, as the user reads it.
pub fn failure_text<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => panic!("an error"),
        Err(error) => error.to_string(),
    }
}
