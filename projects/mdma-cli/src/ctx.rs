//! Command context: the global options, and the node resolved from them.
//!
//! Every command receives a [`Ctx`] and asks it for the connections it needs.
//! The node is resolved lazily, the first time something needs it, so commands
//! that never talk to a node (`mdma node ...`, completions) never resolve one.
//! The context itself is `mdma_client::Node`; this module parses the options.

use std::path::{Path, PathBuf};

use hicli::Arg;
use mdma_client::node::DEFAULT_LIBRARY_SOCKET;
use mdma_client::{NeededBy, NodeOptions};
use node_discovery::NodeSpec;

use crate::node;

pub(crate) type Ctx = mdma_client::Node;

/// What needed the node when the command at `path` ran: `mdma` and the canonical
/// command path, e.g. `mdma queue append`. A line that did not parse as a command
/// names only the program.
pub(crate) fn needed_by(path: Option<&[&str]>) -> NeededBy {
    let words = path.unwrap_or_default().iter().copied();
    NeededBy::new(
        std::iter::once("mdma")
            .chain(words)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Node hostname (e.g. mdma-909.local), optionally with a port. Also read from
/// MDMA_NODE. Without either, the remembered or discovered node is used.
// No `env`: MDMA_NODE is read separately (see `node`), so the flag and the
// variable can be told apart and an empty variable counts as unset.
#[derive(Debug, Clone, Arg)]
#[arg(long = "node", metavar = "NODE", complete = node::complete_nodes)]
pub struct NodeFlag(pub Option<NodeSpec>);

/// `ipc://` addresses of the sockets in `dir` that start with `typed`, sorted.
fn ipc_sockets_in(dir: &Path, typed: &str) -> Vec<hicli::Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
        .map(|path| format!("ipc://{}", path.display()))
        .filter(|address| address.starts_with(typed))
        .collect();
    found.sort();
    found.into_iter().map(|address| (address, None)).collect()
}

/// The local services' sockets (`/run/mdma/*.sock`), for `--socket` and `--playback-socket`.
fn complete_ipc_sockets(typed: &str) -> Vec<hicli::Candidate> {
    let dir = DEFAULT_LIBRARY_SOCKET
        .strip_prefix("ipc://")
        .map(Path::new)
        .and_then(Path::parent);
    dir.map(|dir| ipc_sockets_in(dir, typed))
        .unwrap_or_default()
}

// The socket addresses stay `String`: the clients take the address text
// (`ipc://<path>` or `tcp://<host>:<port>`) and no address type exists to parse it into.

/// Library IPC socket address (direct mode, ignored when --node is set; default: ipc:///run/mdma/library.sock)
#[derive(Debug, Clone, Arg)]
#[arg(
    long = "socket",
    metavar = "SOCKET",
    env = "MDMA_LIBRARY_SOCKET",
    complete = complete_ipc_sockets
)]
pub struct LibrarySocket(pub Option<String>);

/// Playback server socket address (direct mode, ignored when --node is set; default: ipc:///run/mdma/playback.sock)
#[derive(Debug, Clone, Arg)]
#[arg(
    long = "playback-socket",
    metavar = "PLAYBACK_SOCKET",
    env = "MDMA_PLAYBACK_SOCKET",
    complete = complete_ipc_sockets
)]
pub struct PlaybackSocket(pub Option<String>);

/// Sources directory for direct mode, containing *.sock files (default: /run/mdma/sources)
#[derive(Debug, Clone, Arg)]
#[arg(
    long = "sources-dir",
    metavar = "SOURCES_DIR",
    env = "MDMA_SOURCES_DIR",
    complete = dirs
)]
pub struct SourcesDir(pub Option<PathBuf>);

// The global options, as given on the command line or in the environment.
//
// Each is `None` when left at its default (applied where it is used), so that
// "given explicitly" can be told apart from "defaulted". An empty value (a
// flag or a variable set to nothing) counts as not given, like `MDMA_NODE`.
#[derive(Debug, Clone, Arg)]
pub struct Globals {
    pub node: NodeFlag,
    pub socket: LibrarySocket,
    pub playback_socket: PlaybackSocket,
    pub sources_dir: SourcesDir,
}

impl From<Globals> for NodeOptions {
    fn from(globals: Globals) -> Self {
        Self {
            node: globals.node.0,
            library_socket: globals.socket.0,
            playback_socket: globals.playback_socket.0,
            sources_dir: globals.sources_dir.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{failure_text, local_ctx};
    use pretty_assertions::assert_eq;

    #[test]
    fn admin_needs_a_gateway_over_ipc() {
        assert_eq!(
            failure_text(local_ctx().admin().map_err(Into::into)),
            "admin commands need a gateway; use --node (or MDMA_NODE) to name the node"
        );
    }

    #[test]
    fn the_label_is_the_command_as_typed() {
        assert_eq!(
            needed_by(Some(&["queue", "append"])),
            NeededBy::new("mdma queue append")
        );
        assert_eq!(needed_by(Some(&["ping"])), NeededBy::new("mdma ping"));
        assert_eq!(needed_by(None), NeededBy::new("mdma"));
    }

    #[test]
    fn globals_become_node_options_unchanged() {
        let options: NodeOptions = Globals {
            node: NodeFlag(None),
            socket: LibrarySocket(Some("ipc:///tmp/lib.sock".into())),
            playback_socket: PlaybackSocket(None),
            sources_dir: SourcesDir(Some(PathBuf::from("/tmp/sources"))),
        }
        .into();
        assert_eq!(
            options.library_socket.as_deref(),
            Some("ipc:///tmp/lib.sock")
        );
        assert_eq!(options.playback_socket, None);
        assert_eq!(options.sources_dir, Some(PathBuf::from("/tmp/sources")));
        assert!(options.node.is_none());
    }
}
