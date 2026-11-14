//! Test helpers shared by several modules' tests.

use crate::ctx::{Ctx, Globals};
use bpaf::{ParseFailure, Parser as _};
use library_ipc_client::{ContentHash, TrackInfo};

/// Parse `args` (without the program name) as an `mdma` command line.
pub(crate) fn parse_cli(args: &[&str]) -> Result<(Globals, hicli::Invocation<Ctx>), ParseFailure> {
    crate::app().parse(args.iter().copied())
}

/// Parse `args` (without the program name), keeping only the command.
pub(crate) fn parse(args: &[&str]) -> Result<hicli::Invocation<Ctx>, ParseFailure> {
    parse_cli(args).map(|(_, invocation)| invocation)
}

/// Parse `args`, which must be a valid command line; the path of the command it reaches.
pub(crate) fn parses(args: &[&str]) -> Vec<&'static str> {
    match parse(args) {
        Ok(invocation) => invocation.path().to_vec(),
        Err(e) => panic!("{args:?} should parse: {e:?}"),
    }
}

/// Parse `args`, which must be rejected as a usage error (exit code 2); the message.
pub(crate) fn usage_error(args: &[&str]) -> String {
    match parse(args) {
        Ok(invocation) => panic!("{args:?} should be rejected, got {:?}", invocation.path()),
        Err(failure) => {
            assert_eq!(hicli::exit_code_for(&failure), 2, "{args:?}");
            flat(&failure.unwrap_stderr())
        }
    }
}

/// Parse `args`, which must ask for help (exit code 0); the help text.
pub(crate) fn help(args: &[&str]) -> String {
    match parse(args) {
        Ok(invocation) => panic!("{args:?} should show help, got {:?}", invocation.path()),
        Err(failure) => {
            assert_eq!(hicli::exit_code_for(&failure), 0, "{args:?}");
            failure.unwrap_stdout()
        }
    }
}

/// Run the [`hicli::Arg`] parser of `T` over `args`; an error is bpaf's message
/// (which wraps long lines, so its whitespace is collapsed to single spaces).
pub(crate) fn run_arg<T: hicli::Arg>(args: &[&str]) -> Result<T, String> {
    <T as hicli::Arg>::parser()
        .to_options()
        .run_inner(args)
        .map_err(|failure| flat(&failure.unwrap_stderr()))
}

/// Held by every test that sets an environment variable (`MDMA_NODE`, `EDITOR`,
/// `COLUMNS`): the environment is process-wide and tests run in parallel.
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Global options as given by none of the flags or variables.
pub(crate) fn globals() -> Globals {
    Globals {
        node: None,
        socket: None,
        playback_socket: None,
        sources_dir: "/run/mdma/sources".to_string(),
    }
}

/// A context for the local services (IPC), with no node to resolve. Its sockets
/// and sources directory do not exist, so no test can reach a real local service.
pub(crate) fn local_ctx() -> Ctx {
    let nowhere = "/nonexistent/mdma-test";
    Ctx::with_target(
        Globals {
            node: None,
            socket: Some(format!("ipc://{nowhere}/library.sock")),
            playback_socket: Some(format!("ipc://{nowhere}/playback.sock")),
            sources_dir: format!("{nowhere}/sources"),
        },
        node_discovery::Target::LocalIpc,
    )
}

/// Keeps a dead node's port reserved: bound but never listening, so dials are
/// always refused and no other test can be handed the same port.
pub(crate) struct PortGuard(#[allow(dead_code)] std::os::fd::OwnedFd);

/// A node on this machine that refuses connections. The target, its port, and
/// the guard that must be held for as long as the node should stay dead.
pub(crate) fn dead_node() -> (node_discovery::Target, u16, PortGuard) {
    use std::os::fd::FromRawFd;
    // std has no bound-but-not-listening TCP socket, so go through libc.
    // SAFETY: plain socket/bind/getsockname calls on a descriptor we own and
    // immediately wrap in an OwnedFd; `addr` outlives every call using it.
    let (fd, port) = unsafe {
        let raw = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        assert!(raw >= 0, "create a socket");
        let fd = std::os::fd::OwnedFd::from_raw_fd(raw);
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
        let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let addr_ptr = std::ptr::addr_of_mut!(addr).cast::<libc::sockaddr>();
        let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
        assert_eq!(libc::bind(raw, addr_ptr, len), 0, "bind a free port");
        let mut out_len = len;
        assert_eq!(
            libc::getsockname(raw, addr_ptr, &mut out_len),
            0,
            "read the bound port"
        );
        (fd, u16::from_be(addr.sin_port))
    };
    let target = node_discovery::Target::Remote(node_discovery::RemoteTarget {
        host: "localhost".to_string(),
        ip: Some(std::net::Ipv4Addr::LOCALHOST),
        port,
        source: node_discovery::Source::Cache,
    });
    (target, port, PortGuard(fd))
}

/// The text hicli would print (after its `Error: ` prefix) for a failed command result.
pub(crate) fn failure_text<T>(r: color_eyre::Result<T>) -> String {
    hicli::Failure::from(r.err().expect("an error"))
        .message()
        .to_owned()
}

/// `message` with its whitespace collapsed to single spaces.
fn flat(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip ANSI escape codes from a string.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(next) = chars.next() {
                if next == '[' {
                    for seq_char in chars.by_ref() {
                        if seq_char.is_ascii_alphabetic() || seq_char == '~' {
                            break;
                        }
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A track with only its content hash set: everything optional is `None`,
/// collections are empty. Build variations with `TrackInfo { .., ..track(hash) }`.
pub(crate) fn track(hash: &str) -> TrackInfo {
    TrackInfo {
        content_hash: ContentHash::new(hash),
        title: None,
        artist: None,
        album: None,
        duration: None,
        bpm: None,
        key: None,
        blob_path: None,
        cover_art_path: None,
        track_number: None,
        disc_number: None,
        added: None,
        started: None,
        stopped: None,
        memory_cues: vec![],
        beat_grid: None,
        role: None,
        energy: None,
    }
}
