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
    let mut fake = fact_replica::test_support::FakeNode::new("e1", 0);
    fake.lines = lines;
    let id = fact_replica::NodeId::new(node).expect("a usable node id");
    fact_replica::Replica::new(nodes_dir, &id)
        .sync(
            fact_replica::UnixSecs::new(1),
            Some(&fact_replica::Host::new(host)),
            &mut |cursor, limit, page| fake.serve(cursor, limit, page),
        )
        .expect("the fake node syncs");
}

/// One line of a fact log, as a replica stores it: an assertion of the JSON `value`
/// (`{"t": .., "v": ..}`) about `entity` at `time` (RFC 3339).
pub fn asserted(entity: &str, value: &str, time: &str) -> String {
    let source = r#"{"tool":"t","version":"1","origin":{"type":"User"}}"#;
    format!(r#"["{entity}",{value},"{time}",{source},"Assert"]"#)
}

/// The text of a failed result, as the user reads it.
pub fn failure_text<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => panic!("an error"),
        Err(error) => error.to_string(),
    }
}
