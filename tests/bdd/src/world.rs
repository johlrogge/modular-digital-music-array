//! Cucumber World — holds clients, last results, and test state.

use crate::harness::{self, SeedTrack, TestEnv};
use mdma_client::{ContentHash, LibraryBackend, PlaybackBackend, TrackInfo};

/// The cucumber World. Each scenario gets a fresh instance.
#[derive(cucumber::World)]
#[world(init = Self::new)]
pub struct MdmaWorld {
    /// Test environment (services + temp dirs). None until Background seeds it.
    env: Option<TestEnv>,

    /// Last search results for assertions.
    pub last_search_results: Vec<TrackInfo>,

    /// Last queue listing for assertions.
    pub last_queue: Vec<ContentHash>,

    /// Last error message, if any.
    pub last_error: Option<String>,

    /// Tracks seeded in Background, before the env is booted.
    pub pending_tracks: Vec<SeedTrack>,

    // --- CLI subprocess state ---
    /// Captured stdout from the last CLI invocation.
    pub last_cli_stdout: String,

    /// Captured stderr from the last CLI invocation.
    pub last_cli_stderr: String,

    /// Exit code from the last CLI invocation.
    pub last_cli_exit_code: Option<i32>,
}

impl std::fmt::Debug for MdmaWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdmaWorld")
            .field("has_env", &self.env.is_some())
            .field("search_results", &self.last_search_results.len())
            .field("queue_len", &self.last_queue.len())
            .field("last_error", &self.last_error)
            .field("cli_exit_code", &self.last_cli_exit_code)
            .finish()
    }
}

impl MdmaWorld {
    fn new() -> Self {
        Self {
            env: None,
            last_search_results: vec![],
            last_queue: vec![],
            last_error: None,
            pending_tracks: vec![],
            last_cli_stdout: String::new(),
            last_cli_stderr: String::new(),
            last_cli_exit_code: None,
        }
    }

    /// Boot the test environment with the tracks accumulated in Background steps.
    pub fn ensure_env(&mut self) {
        if self.env.is_none() {
            let tracks = std::mem::take(&mut self.pending_tracks);
            self.env = Some(harness::boot_test_env(&tracks));
        }
    }

    /// Get a reference to the library client. Panics if env not booted.
    pub fn library(&mut self) -> &LibraryBackend {
        self.ensure_env();
        &self.env.as_ref().unwrap().library
    }

    /// Get a reference to the playback client. Panics if env not booted.
    pub fn playback(&mut self) -> &PlaybackBackend {
        self.ensure_env();
        &self.env.as_ref().unwrap().playback
    }

    /// Library IPC address for CLI env var. Panics if env not booted.
    pub fn library_addr(&self) -> &str {
        &self.env.as_ref().expect("env not booted").library_addr
    }

    /// Playback IPC address for CLI env var. Panics if env not booted.
    pub fn playback_addr(&self) -> &str {
        &self.env.as_ref().expect("env not booted").playback_addr
    }

    /// Build a `Command` with the common CLI setup:
    /// binary path, `--socket` / `--playback-socket` flags, user-provided args,
    /// and removal of env vars that would interfere with the test harness.
    fn base_command(&self, args: &[&str]) -> std::process::Command {
        let binary = cli_binary_path();
        let mut cmd = std::process::Command::new(&binary);
        cmd.arg("--socket")
            .arg(self.library_addr())
            .arg("--playback-socket")
            .arg(self.playback_addr())
            .env_remove("MDMA_GATEWAY")
            .env_remove("MDMA_NODE")
            .env_remove("MDMA_LIBRARY_SOCKET")
            .env_remove("MDMA_PLAYBACK_SOCKET");
        cmd.args(args);
        cmd
    }

    /// Configure stdin on `cmd` based on whether `stdin_data` will be provided.
    ///
    /// When `stdin_data` is `Some`, stdin is set to `Stdio::piped()` so the
    /// caller can write data to it later; returns `None`.
    ///
    /// When `stdin_data` is `None`, a PTY slave is attached so the child sees
    /// a real terminal for stdin (preventing an empty stdin intersection filter).
    /// Returns `Some(master)` so the caller can keep the master fd alive until
    /// the child exits.
    fn configure_stdin(
        cmd: &mut std::process::Command,
        stdin_data: &Option<String>,
    ) -> Option<std::fs::File> {
        if stdin_data.is_some() {
            cmd.stdin(std::process::Stdio::piped());
            None
        } else {
            // Create a PTY so the CLI subprocess sees a real terminal for stdin.
            // This prevents search from applying an empty stdin intersection filter
            // (which triggers when stdin is not a terminal).
            let (master, slave) = open_pty();
            cmd.stdin(std::process::Stdio::from(slave));
            Some(master)
        }
    }

    /// Write `stdin_data` to the child's stdin and close it so the child sees EOF.
    /// Does nothing when `stdin_data` is `None`.
    fn write_stdin_and_close(child: &mut std::process::Child, stdin_data: &Option<String>) {
        if let Some(data) = stdin_data {
            use std::io::Write;
            let stdin = child.stdin.as_mut().expect("stdin was piped");
            stdin.write_all(data.as_bytes()).expect("write to stdin");
            drop(child.stdin.take()); // close stdin so child sees EOF
        }
    }

    /// Run the `mdma` CLI binary as a subprocess, capturing stdout/stderr/exit code.
    ///
    /// Environment variables point the CLI at the test harness IPC sockets.
    /// When `stdin_data` is Some, it is piped to the process's stdin.
    /// Stdout is captured via `Stdio::piped()`, so the CLI uses pipe-mode output.
    pub fn run_cli(&mut self, args: &[&str], stdin_data: Option<&str>) {
        self.ensure_env();

        let stdin_data = stdin_data.map(str::to_owned);

        let mut cmd = self.base_command(args);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Keep PTY master alive until after child exits (when stdin_data is None).
        let _stdin_master = Self::configure_stdin(&mut cmd, &stdin_data);

        let binary = cli_binary_path();
        let mut child = cmd.spawn().unwrap_or_else(|e| {
            panic!("Failed to spawn mdma binary at {:?}: {}", binary, e);
        });

        Self::write_stdin_and_close(&mut child, &stdin_data);

        let output = child.wait_with_output().expect("wait_with_output");
        self.last_cli_stdout = String::from_utf8_lossy(&output.stdout).to_string();
        self.last_cli_stderr = String::from_utf8_lossy(&output.stderr).to_string();
        self.last_cli_exit_code = output.status.code();
    }

    /// Run the `mdma` CLI binary with stdout connected to a PTY (TTY mode).
    ///
    /// This makes `stdout.is_terminal()` return true in the child process,
    /// triggering corsett table rendering with ANSI colors. The `columns`
    /// parameter sets the `$COLUMNS` env var to control `terminal_width()`.
    ///
    /// When `stdin_data` is Some, it is piped to stdin. When None, a separate
    /// PTY is used for stdin (preventing the empty hash filter).
    pub fn run_cli_tty(&mut self, args: &[&str], columns: usize, stdin_data: Option<&str>) {
        use std::os::fd::AsRawFd;
        self.ensure_env();

        let stdin_data = stdin_data.map(str::to_owned);

        let mut cmd = self.base_command(args);
        cmd.env("COLUMNS", columns.to_string())
            .stderr(std::process::Stdio::piped());

        // PTY for stdout — enables is_terminal() in the child
        let (stdout_master, stdout_slave) = open_pty();
        cmd.stdout(std::process::Stdio::from(stdout_slave));

        // Keep PTY master alive until after child exits (when stdin_data is None).
        let _stdin_master = Self::configure_stdin(&mut cmd, &stdin_data);

        let binary = cli_binary_path();
        let mut child = cmd.spawn().unwrap_or_else(|e| {
            panic!("Failed to spawn mdma binary at {:?}: {}", binary, e);
        });

        // Drop the Command to close the stdout slave fd in the parent.
        // Command::spawn() takes &mut self so the Command (and its Stdio fds)
        // stays alive after spawn. The child has its own copy of the slave fd
        // from fork(). If we don't close the parent's copy, the PTY master
        // will never get EIO when the child exits (because the slave refcount
        // stays >0), and the reader thread hangs forever.
        drop(cmd);

        Self::write_stdin_and_close(&mut child, &stdin_data);

        // Reader thread drains PTY master to avoid blocking the child.
        // PTY buffer is small (~4KB); child blocks on write if we don't read.
        let master_fd = stdout_master.as_raw_fd();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let mut master_file = &stdout_master; // borrow to keep alive in thread
            loop {
                match std::io::Read::read(&mut master_file, &mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    // EIO (errno 5) = slave side closed (child exited) — normal PTY EOF
                    Err(e) if e.raw_os_error() == Some(5) => break,
                    Err(e) => panic!("PTY read error (fd {}): {}", master_fd, e),
                }
            }
            buf
        });

        let status = child.wait().expect("wait for child");
        let stdout_bytes = reader.join().expect("join reader thread");

        self.last_cli_stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
        // Collect stderr from the piped handle
        if let Some(mut stderr) = child.stderr.take() {
            let mut stderr_buf = Vec::new();
            std::io::Read::read_to_end(&mut stderr, &mut stderr_buf).ok();
            self.last_cli_stderr = String::from_utf8_lossy(&stderr_buf).to_string();
        } else {
            self.last_cli_stderr.clear();
        }
        self.last_cli_exit_code = status.code();
    }
}

/// Create a pseudo-terminal pair. Returns (master, slave) as owned Files.
/// The slave end is used as stdin for child processes so `isatty()` returns true.
fn open_pty() -> (std::fs::File, std::fs::File) {
    use std::os::fd::FromRawFd;

    extern "C" {
        fn openpty(
            master: *mut std::ffi::c_int,
            slave: *mut std::ffi::c_int,
            name: *mut std::ffi::c_char,
            termp: *const std::ffi::c_void,
            winp: *const std::ffi::c_void,
        ) -> std::ffi::c_int;
    }

    let mut master_fd: std::ffi::c_int = -1;
    let mut slave_fd: std::ffi::c_int = -1;

    let ret = unsafe {
        openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert!(
        ret == 0,
        "openpty() failed: {}",
        std::io::Error::last_os_error()
    );

    unsafe {
        (
            std::fs::File::from_raw_fd(master_fd),
            std::fs::File::from_raw_fd(slave_fd),
        )
    }
}

/// Locate the `mdma` binary in the target/debug directory.
fn cli_binary_path() -> std::path::PathBuf {
    // The test binary is in target/debug/deps/cucumber-<hash>.
    // The mdma binary is at target/debug/mdma.
    let test_exe = std::env::current_exe().expect("current_exe");
    let target_debug = test_exe
        .parent() // deps/
        .and_then(|p| p.parent()) // debug/
        .expect("could not find target/debug from test exe");
    let binary = target_debug.join("mdma");
    assert!(
        binary.exists(),
        "mdma binary not found at {:?} — run `cargo build` first",
        binary
    );
    binary
}
