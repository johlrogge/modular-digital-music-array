#![cfg(unix)]

#[test]
fn init_restores_default_sigpipe() {
    // SAFETY: single test in this process; no other thread touches signals.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    cli_base::init();
    // SAFETY: as above; reads the previous action while setting the default.
    let prev = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    assert_eq!(prev, libc::SIG_DFL);
}
