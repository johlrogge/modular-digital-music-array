//! A reader that closes the pipe (`mdma ... | head`) must end mdma quietly by
//! SIGPIPE, not with a panic.
#![cfg(unix)]

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

#[test]
fn writing_to_a_closed_pipe_ends_mdma_by_sigpipe() {
    // The read end is closed before mdma starts, so its first write fails.
    let (reader, writer) = std::io::pipe().expect("created a pipe");
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_mdma"))
        .args(["generate-completions", "bash"])
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("ran mdma");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGPIPE),
        "status: {:?}, stderr: {stderr}",
        output.status
    );
}
