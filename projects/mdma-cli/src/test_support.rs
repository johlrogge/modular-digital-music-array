//! Test helpers shared by several modules' tests.

use crate::ctx::{Ctx, FactsFlag, Globals, LibrarySocket, NodeFlag, PlaybackSocket, SourcesDir};
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

// The environment is process-wide, so one lock serves every test that sets a variable
// (`MDMA_NODE`, `EDITOR`, `COLUMNS`): the one `client-facts` takes, whose tests set
// `XDG_CACHE_HOME`. Any new test that starts a process, or takes a sync flock, must
// hold `env_lock` (#107: a forked child inherits the flock until its exec).
pub(crate) use client_facts::test_support::{env_lock, EnvVars};

/// A context for the local services (IPC), with no node to resolve. Its sockets
/// and sources directory do not exist, so no test can reach a real local service.
/// It reads facts from the node (which is not there), never from the replica of
/// whoever runs the tests.
pub(crate) fn local_ctx() -> Ctx {
    let nowhere = "/nonexistent/mdma-test";
    Ctx::with_target(
        Globals {
            facts: FactsFlag::given(Some(client_facts::FactsChoice::Node)),
            node: NodeFlag(None),
            socket: LibrarySocket(Some(format!("ipc://{nowhere}/library.sock"))),
            playback_socket: PlaybackSocket(Some(format!("ipc://{nowhere}/playback.sock"))),
            sources_dir: SourcesDir(Some(std::path::PathBuf::from(format!("{nowhere}/sources")))),
        },
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

/// Fake library and playback services over IPC, for commands that talk to both.
///
/// The library knows the tracks given and resolves a hash the way the real one
/// does: a unique prefix (with or without `sha256:`) is its track, anything else
/// is an error. (This prefix matching is a simplification of
/// `library_projection::resolve_prefix`, which the real library uses.) Playback
/// answers a queue listing with the queue given and every other command with
/// success (a removal counts every hash it was given), and keeps what it was
/// sent, as JSON. A request either fake does not understand gets an error
/// response, so a failure shows as itself rather than as a timeout.
pub(crate) struct FakeServices {
    _dir: tempfile::TempDir,
    pub(crate) library: mdma_client::LibraryBackend,
    pub(crate) playback: mdma_client::PlaybackBackend,
    sent: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl FakeServices {
    /// `tracks`: the full hashes the fake library holds.
    pub(crate) fn new(tracks: &[&str]) -> Self {
        Self::with_queue(tracks, &[])
    }

    /// `tracks`: the full hashes the fake library holds; `queue`: what the
    /// fake playback lists as its queue.
    pub(crate) fn with_queue(tracks: &[&str], queue: &[&str]) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let library_addr = format!("ipc://{}/library.sock", dir.path().display());
        let playback_addr = format!("ipc://{}/playback.sock", dir.path().display());
        let tracks: Vec<String> = tracks.iter().map(|t| t.to_string()).collect();
        let queue: Vec<ContentHash> = queue.iter().map(|h| ContentHash::new(*h)).collect();
        serve(&library_addr, move |request| {
            let error = |message: String| {
                serde_json::to_vec(&library_ipc_client::LibraryResponse::Error(
                    library_ipc_client::ProtocolError::Internal { message },
                ))
                .expect("a response")
            };
            let request: library_ipc_client::LibraryRequest = match serde_json::from_slice(request)
            {
                Ok(request) => request,
                Err(e) => return error(format!("the fake library cannot read the request: {e}")),
            };
            let hash = match request {
                library_ipc_client::LibraryRequest::GetTrack { hash } => hash,
                other => {
                    return error(format!(
                        "the fake library only answers GetTrack, got {other:?}"
                    ))
                }
            };
            let wanted = hash.as_str().trim_start_matches("sha256:");
            let found: Vec<&String> = tracks
                .iter()
                .filter(|t| t.trim_start_matches("sha256:").starts_with(wanted))
                .collect();
            let response = match found.as_slice() {
                [one] => library_ipc_client::LibraryResponse::Track(Box::new(track(one))),
                [] => library_ipc_client::LibraryResponse::Error(
                    library_ipc_client::ProtocolError::TrackNotFound {
                        hash: hash.to_string(),
                    },
                ),
                _ => library_ipc_client::LibraryResponse::Error(
                    library_ipc_client::ProtocolError::Internal {
                        message: format!("ambiguous hash prefix: {hash}"),
                    },
                ),
            };
            serde_json::to_vec(&response).expect("a response")
        });
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = sent.clone();
        serve(&playback_addr, move |request| {
            let command = serde_json::from_slice::<serde_json::Value>(request).and_then(|value| {
                serde_json::from_value::<mdma_client::Command>(value.clone())
                    .map(|command| (value, command))
            });
            let response = match command {
                Ok((value, command)) => {
                    log.lock().unwrap().push(value);
                    let data = match command {
                        mdma_client::Command::QueueRemove { hashes } => {
                            Some(mdma_client::ResponseData::Count(hashes.len()))
                        }
                        mdma_client::Command::QueueList => {
                            Some(mdma_client::ResponseData::Queue(queue.clone()))
                        }
                        _ => None,
                    };
                    mdma_client::Response::Ok { data }
                }
                Err(e) => mdma_client::Response::Err {
                    message: format!("the fake playback cannot read the command: {e}"),
                },
            };
            serde_json::to_vec(&response).expect("a response")
        });
        Self {
            library: mdma_client::LibraryBackend::connect_direct(&library_addr)
                .expect("connect to the fake library"),
            playback: mdma_client::PlaybackBackend::connect_direct(&playback_addr)
                .expect("connect to the fake playback"),
            _dir: dir,
            sent,
        }
    }

    /// The playback commands received so far, as JSON, in order.
    pub(crate) fn sent(&self) -> Vec<serde_json::Value> {
        self.sent.lock().unwrap().clone()
    }
}

/// Answer each request on `address` with `handler`'s reply, on a thread of its own.
fn serve(address: &str, handler: impl Fn(&[u8]) -> Vec<u8> + Send + 'static) {
    let socket = nng::Socket::new(nng::Protocol::Rep0).expect("a socket");
    socket.listen(address).expect("listen");
    std::thread::spawn(move || {
        while let Ok(request) = socket.recv() {
            let reply = handler(&request);
            if socket.send(nng::Message::from(&reply[..])).is_err() {
                break;
            }
        }
    });
}

/// A command as the fake playback records it.
pub(crate) fn recorded(command: &mdma_client::Command) -> serde_json::Value {
    serde_json::to_value(command).expect("a command as JSON")
}

/// A track with only its content hash set: everything optional is `None`,
/// collections are empty. Build variations with `TrackInfo { .., ..track(hash) }`.
pub(crate) fn track(hash: &str) -> TrackInfo {
    TrackInfo {
        content_hash: ContentHash::new(hash),
        title: None,
        artist: None,
        album: None,
        genre: None,
        label: None,
        styles: vec![],
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
