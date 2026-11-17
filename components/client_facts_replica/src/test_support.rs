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

/// One line of a fact log, as a replica stores it: an assertion of the JSON `value`
/// (`{"t": .., "v": ..}`) about `entity` at `time` (RFC 3339).
pub fn asserted(entity: &str, value: &str, time: &str) -> String {
    let source = r#"{"tool":"t","version":"1","origin":{"type":"User"}}"#;
    format!(r#"["{entity}",{value},"{time}",{source},"Assert"]"#)
}

/// The text of a failed result, as the user reads it.
pub fn failure_text<T>(result: color_eyre::Result<T>) -> String {
    result.err().expect("an error").to_string()
}
