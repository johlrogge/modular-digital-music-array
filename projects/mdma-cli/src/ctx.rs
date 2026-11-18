//! Command context: the global options, and the node resolved from them.
//!
//! Every command receives a [`Ctx`] and asks it for the connections it needs.
//! The node is resolved lazily, the first time something needs it, so commands
//! that never talk to a node (`mdma node ...`, completions) never resolve one.
//! The context holds an `mdma_client::Node` ([`Ctx::node`]) and where the reads of
//! facts come from (`--facts`, ADR-030 §2.3); this module parses the options.

use std::path::{Path, PathBuf};

use client_facts::{Facts, FactsChoice, FactsError, FactsWord, UnixSecs};
use hicli::Arg;
use mdma_client::node::DEFAULT_LIBRARY_SOCKET;
use mdma_client::{NeededBy, Node, NodeOptions};
use node_discovery::NodeSpec;

use crate::node;

/// What a command runs against: the node, resolved on first use, and the source its
/// reads of facts come from.
pub(crate) struct Ctx {
    node: Node,
    facts: FactsChoice,
}

impl Ctx {
    /// A context whose node errors say `needed_by` needed it.
    pub(crate) fn needed_by(needed_by: NeededBy, globals: Globals) -> Self {
        let facts = globals.facts.choice().unwrap_or_default();
        Self {
            node: Node::needed_by(needed_by, globals.into()),
            facts,
        }
    }

    /// A context for a node already resolved to `target`.
    #[cfg(test)]
    pub(crate) fn with_target(globals: Globals, target: node_discovery::Target) -> Self {
        let facts = globals.facts.choice().unwrap_or_default();
        Self {
            node: Node::with_target(globals.into(), target),
            facts,
        }
    }

    /// The source `--facts` (or `MDMA_FACTS`) chose; the replica unless told otherwise.
    #[cfg(test)]
    pub(crate) fn facts_choice(&self) -> FactsChoice {
        self.facts
    }

    /// The node, resolved on first use. For everything that is not a read of facts
    /// (writes, queue, playback, playlists, sources, admin) and for reads that become
    /// writes (ADR-030 §5).
    pub(crate) fn node(&self) -> &Node {
        &self.node
    }

    /// The facts to read from. The replica is folded now; the node is not touched
    /// until a read needs it.
    fn facts(&self) -> Result<Facts<&Node>, FactsError> {
        Facts::open(&self.node, self.facts)
    }

    /// Open the facts, run `read` on them and, once the replica has answered, say on
    /// stderr where the answer came from: for a refusal too, since a stale replica is
    /// when its age matters (ADR-030 §3). No note when the facts did not open. The one
    /// way a command reads facts, so none forgets the note, and stdout carries only
    /// the answer.
    pub(crate) fn read<T>(
        &self,
        read: impl FnOnce(&Facts<&Node>) -> color_eyre::Result<T>,
    ) -> color_eyre::Result<T> {
        let facts = self.facts()?;
        let outcome = read(&facts);
        note_source(&facts);
        outcome
    }
}

/// The note of where `facts` read from (ADR-030 §3), on stderr: exactly once per
/// command, for the replica only (the node says nothing).
fn note_source(facts: &Facts<&Node>) {
    if let Some(note) = facts.source_note(UnixSecs::now()) {
        eprintln!("{note}");
    }
}

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

/// Where reads of facts come from: `replica` (the local copy, the default) or `node`.
/// Also read from MDMA_FACTS; the flag wins.
// The variable is optional and never required. `None` is left to `FactsChoice`'s
// default (the replica), applied in `Ctx`.
#[derive(Debug, Clone, Arg)]
#[arg(long = "facts", metavar = "SOURCE", env = "MDMA_FACTS", complete = complete_facts)]
pub struct FactsFlag(pub Option<FactsWord>);

impl FactsFlag {
    /// The flag as given: `choice` is what it names, `None` when it was left out.
    pub fn given(choice: Option<FactsChoice>) -> Self {
        Self(choice.map(FactsWord::given))
    }

    /// What the flag or the variable named; `None` when it is left out or empty.
    pub fn choice(&self) -> Option<FactsChoice> {
        self.0.and_then(FactsWord::choice)
    }
}

/// The words `--facts` takes, for completion.
fn complete_facts(typed: &str) -> Vec<hicli::Candidate> {
    [
        ("replica", "the local replica of the node's facts (default)"),
        ("node", "ask the node"),
    ]
    .into_iter()
    .filter(|(word, _)| word.starts_with(typed))
    .map(|(word, about)| (word.to_string(), Some(about.to_string())))
    .collect()
}

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
    pub facts: FactsFlag,
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
            failure_text(local_ctx().node().admin().map_err(Into::into)),
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

    // --- --facts ---------------------------------------------------------------

    /// The choice a command line (and the environment as it is) parses to.
    fn facts_of(args: &[&str]) -> Option<FactsChoice> {
        crate::test_support::parse_cli(args)
            .expect("valid args")
            .0
            .facts
            .choice()
    }

    // MDMA_FACTS is never set in this process: the environment is shared with every
    // test that parses a command line, and one bad value would fail them all.
    // `tests/facts_source.rs` runs the real binary with the variable set instead.
    #[test]
    fn facts_is_a_global_that_is_replica_or_node() {
        assert_eq!(
            facts_of(&["--facts=node", "search"]),
            Some(FactsChoice::Node)
        );
        assert_eq!(
            facts_of(&["search", "--facts", "replica"]),
            Some(FactsChoice::Replica),
            "a global may follow the command"
        );
        assert_eq!(
            facts_of(&["--facts=", "search"]),
            None,
            "empty is not given"
        );
        let refused = crate::test_support::usage_error(&["--facts=cache", "search"]);
        assert!(
            refused.contains("unknown facts source 'cache'; use replica or node"),
            "{refused}"
        );
    }

    #[test]
    fn the_choice_defaults_to_the_replica() {
        let ctx = Ctx::with_target(
            Globals {
                node: NodeFlag(None),
                socket: LibrarySocket(None),
                playback_socket: PlaybackSocket(None),
                sources_dir: SourcesDir(None),
                facts: FactsFlag(None),
            },
            node_discovery::Target::LocalIpc,
        );
        assert_eq!(ctx.facts_choice(), FactsChoice::Replica);
    }

    #[test]
    fn globals_become_node_options_unchanged() {
        let options: NodeOptions = Globals {
            facts: FactsFlag(None),
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
