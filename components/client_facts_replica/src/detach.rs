//! Detaching a child process from the one that starts it.
//!
//! In a file of its own so that `tests/detach.rs` can include it and run a real child
//! in a test binary with no other tests. Spawning while other test threads hold locked
//! files lets the child keep a copy of their descriptors until it execs, which makes
//! their locks look taken for that moment.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Make `command` a detached process: all three standard streams on `/dev/null`, so
/// it holds no pipe of the shell it was started from (an inherited pipe would keep
/// a `$(...)` or a completion waiting for it), and a process group of its own, so a
/// Ctrl-C in the terminal's foreground group does not reach it.
///
/// A process group, not a session: it needs no `unsafe`, and std can then start the
/// child with `posix_spawn`. A terminal hangup is sent to the session's foreground
/// group, which the child is no longer part of.
pub(crate) fn detach(command: &mut Command) -> &mut Command {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
}
