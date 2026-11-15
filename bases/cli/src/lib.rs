//! Process setup shared by command-line binaries.

/// Process setup for a command-line binary. Call once, first thing in `main`,
/// before any thread is spawned: restores the default SIGPIPE action (so
/// `cmd | head` ends quietly instead of panicking) and installs color-eyre's hooks.
pub fn init() {
    // Rust ignores SIGPIPE, so printing to a closed pipe (`mdma ... | head`)
    // would panic. Restore the default so the process ends quietly instead.
    #[cfg(unix)]
    // SAFETY: SIG_DFL installs no handler; the documented precondition is that
    // no other thread exists yet to race the change.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    color_eyre::install().expect("installed once");
}
