//! Test helpers shared by several modules' tests.

use crate::ctx::{Ctx, Globals, LibrarySocket, NodeFlag, PlaybackSocket, SourcesDir};
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

/// Environment variables set for the length of a test and put back when it ends,
/// even if it panics. Hold [`env_lock`] first (declare it first, so it drops last).
#[derive(Default)]
pub(crate) struct EnvVars(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvVars {
    pub(crate) fn set(&mut self, name: &'static str, value: impl AsRef<std::ffi::OsStr>) {
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

/// Global options as given by none of the flags or variables.
pub(crate) fn globals() -> Globals {
    Globals {
        node: NodeFlag(None),
        socket: LibrarySocket(None),
        playback_socket: PlaybackSocket(None),
        sources_dir: SourcesDir(None),
    }
}

/// A context for the local services (IPC), with no node to resolve. Its sockets
/// and sources directory do not exist, so no test can reach a real local service.
pub(crate) fn local_ctx() -> Ctx {
    let nowhere = "/nonexistent/mdma-test";
    Ctx::with_target(
        Globals {
            node: NodeFlag(None),
            socket: LibrarySocket(Some(format!("ipc://{nowhere}/library.sock"))),
            playback_socket: PlaybackSocket(Some(format!("ipc://{nowhere}/playback.sock"))),
            sources_dir: SourcesDir(Some(std::path::PathBuf::from(format!("{nowhere}/sources")))),
        }
        .into(),
        node_discovery::Target::LocalIpc,
    )
}

pub(crate) use mdma_client::test_support::dead_node;

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

/// One line of a fact log, as a replica stores it: an assertion of the JSON `value`
/// (`{"t": .., "v": ..}`) about `entity` at `time` (RFC 3339).
pub(crate) fn asserted(entity: &str, value: &str, time: &str) -> String {
    let source = r#"{"tool":"t","version":"1","origin":{"type":"User"}}"#;
    format!(r#"["{entity}",{value},"{time}",{source},"Assert"]"#)
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
