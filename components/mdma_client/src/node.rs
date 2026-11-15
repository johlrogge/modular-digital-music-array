//! A node and the connections to its services.
//!
//! Every client asks a [`Node`] for the connections it needs. The node is
//! resolved lazily, the first time something needs it, so commands that never
//! talk to a node (`mdma node ...`, completions) never resolve one.

mod resolve;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use gateway_client::GatewayClient;
use library_ipc_client::ClientError;
use nng_transport::NngClientError;
use node_discovery::{NodeSpec, ResolveError, Target};

use crate::error::{connect_failure, connect_failure_text};
use crate::{source_label, LibraryBackend, PlaybackBackend, SourceClient};

pub use resolve::{
    cache_path, env_node, identify_at, ips, is_tty, load_cache, load_cache_quiet, pick_interactive,
    real_effects, request, resolve_error_message, select_error_message, Notes, NODE_ENV,
};
use resolve::{client_config, local_ipc_wanted, remote_host, resolve_startup};

pub const DEFAULT_LIBRARY_SOCKET: &str = "ipc:///run/mdma/library.sock";
pub const DEFAULT_PLAYBACK_SOCKET: &str = "ipc:///run/mdma/playback.sock";
pub const DEFAULT_SOURCES_DIR: &str = "/run/mdma/sources";

/// Why admin commands fail without a gateway.
const ADMIN_NEEDS_GATEWAY: &str =
    "admin commands need a gateway; use --node (or MDMA_NODE) to name the node";

/// Why a node could not be resolved or connected to. The texts are final: no
/// variant has a `source`, so a caller that prints the error chain prints each
/// text once.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// `MDMA_NODE` is not a valid node.
    #[error("{0}")]
    InvalidEnv(String),
    /// No node could be resolved.
    #[error("{}", resolve_error_message(.0))]
    Resolve(ResolveError),
    /// Connecting to a service failed.
    #[error("{0}")]
    Connect(String),
    /// An admin command was asked for without a gateway.
    #[error("{}", ADMIN_NEEDS_GATEWAY)]
    NeedsGateway,
    /// A remote node is needed and none is in use.
    #[error(
        "No remote node to talk to.\n\
         Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
    )]
    NoRemoteNode,
    /// The sources directory cannot be given to the clients as text.
    #[error("the sources directory is not valid UTF-8")]
    SourcesDirNotUtf8,
}

/// The options that say which node to use and where its local services are, as
/// given by the user. Each is `None` when left at its default (applied where it
/// is used), so that "given explicitly" can be told apart from "defaulted". An
/// empty value counts as not given, like `MDMA_NODE`.
// The socket addresses stay `String`: the clients take the address text
// (`ipc://<path>` or `tcp://<host>:<port>`) and no address type exists to parse it into.
#[derive(Debug, Clone, Default)]
pub struct NodeOptions {
    pub node: Option<NodeSpec>,
    pub library_socket: Option<String>,
    pub playback_socket: Option<String>,
    pub sources_dir: Option<PathBuf>,
}

pub struct Node {
    options: NodeOptions,
    local_ipc: bool,
    target: OnceLock<Target>,
}

/// Whether the local services are the node in use: sockets were given explicitly, or
/// the library socket (`ipc://<path>` or a bare path) exists here. The one rule for
/// `Node::new` and for completion, which has no `Node` and reads the environment.
pub fn local_ipc_for(library_socket: &str, sockets_given: bool) -> bool {
    let path = library_socket
        .strip_prefix("ipc://")
        .unwrap_or(library_socket);
    local_ipc_wanted(sockets_given, Path::new(path).exists())
}

/// `value`, unless it is empty (which counts as not given).
fn given(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

impl Node {
    pub fn new(options: NodeOptions) -> Self {
        let mut node = Self {
            options,
            local_ipc: false,
            target: OnceLock::new(),
        };
        node.local_ipc = local_ipc_for(node.library_socket(), node.sockets_given());
        node
    }

    /// The library socket: the one given by flag or environment, else the default.
    pub fn library_socket(&self) -> &str {
        given(&self.options.library_socket).unwrap_or(DEFAULT_LIBRARY_SOCKET)
    }

    /// The playback socket: the one given by flag or environment, else the default.
    pub fn playback_socket(&self) -> &str {
        given(&self.options.playback_socket).unwrap_or(DEFAULT_PLAYBACK_SOCKET)
    }

    /// Whether a library or playback socket was given, as opposed to defaulted.
    pub fn sockets_given(&self) -> bool {
        given(&self.options.library_socket).is_some()
            || given(&self.options.playback_socket).is_some()
    }

    /// A node that is already resolved.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_target(options: NodeOptions, target: Target) -> Self {
        let node = Self {
            options,
            local_ipc: false,
            target: OnceLock::new(),
        };
        node.target
            .set(target)
            .unwrap_or_else(|_| unreachable!("target was just created empty"));
        node
    }

    /// The node to talk to, resolved on first use. Prints a note for implicit
    /// choices and fails with advice when no node can be resolved.
    pub fn target(&self) -> Result<&Target, NodeError> {
        self.target_with(Notes::Print)
    }

    /// Like [`Node::target`], choosing whether the note is printed. Only the first
    /// resolution matters: later calls reuse the cached target.
    pub fn target_with(&self, notes: Notes) -> Result<&Target, NodeError> {
        // `OnceLock::get_or_try_init` is unstable.
        if let Some(target) = self.target.get() {
            return Ok(target);
        }
        let resolved = resolve_startup(self.options.node.as_ref(), self.local_ipc, notes)?;
        Ok(self.target.get_or_init(|| resolved))
    }

    /// The `--node` flag, as given.
    pub fn node_flag(&self) -> Option<&NodeSpec> {
        self.options.node.as_ref()
    }

    /// Whether the local services are to be reached over IPC.
    pub fn local_ipc(&self) -> bool {
        self.local_ipc
    }

    /// The effective gateway address of the resolved node: `tcp://<ip>:<port>`.
    /// `None` for local IPC, where callers fall back to direct IPC.
    pub fn gateway(&self) -> Result<Option<String>, NodeError> {
        Ok(client_config(Some(self.target()?)).gateway_addr())
    }

    /// The effective event gateway address: `tcp://<ip>:5556`.
    /// `None` for local IPC.
    pub fn event_gateway(&self) -> Result<Option<String>, NodeError> {
        Ok(client_config(Some(self.target()?)).event_addr())
    }

    /// Host and IP of the resolved node, for tools that are not NNG (scp, HTTP).
    /// `None` for local IPC.
    pub fn remote_host(&self) -> Result<Option<(String, Option<Ipv4Addr>)>, NodeError> {
        Ok(remote_host(Some(self.target()?)))
    }

    /// Require a remote node and return its hostname and, when known, its IP.
    ///
    /// Fails when the services are local, reached over IPC.
    pub fn require_node(&self) -> Result<(String, Option<Ipv4Addr>), NodeError> {
        self.remote_host()?.ok_or(NodeError::NoRemoteNode)
    }

    /// Connect to the library backend (gateway or direct).
    pub fn library(&self) -> Result<LibraryBackend, NodeError> {
        let gateway = self.gateway()?;
        LibraryBackend::connect(gateway.as_deref(), self.library_socket())
            .map_err(|e| library_connect_failed(&e, gateway.is_none()))
    }

    /// Connect to the playback backend (gateway or direct).
    pub fn playback(&self) -> Result<PlaybackBackend, NodeError> {
        let gateway = self.gateway()?;
        PlaybackBackend::connect(gateway.as_deref(), self.playback_socket())
            .map_err(|e| connect_failed("playback", &e, gateway.is_none()))
    }

    /// Connect to a source, via gateway or direct IPC, with the default request
    /// timeout or a custom one.
    pub fn source(&self, name: &str, timeout: Option<Duration>) -> Result<SourceClient, NodeError> {
        let gateway = self.gateway()?;
        let sources_dir = self.sources_dir()?;
        let connected = match timeout {
            Some(t) => SourceClient::connect_with_timeout(gateway.as_deref(), sources_dir, name, t),
            None => SourceClient::connect(gateway.as_deref(), sources_dir, name),
        };
        connected.map_err(|e| {
            let running = if gateway.is_none() {
                format!("\nIs mdma-{name} running?")
            } else {
                String::new()
            };
            let what = source_label(name);
            NodeError::Connect(format!(
                "{}{running}\nHint: 'mdma source list' shows the available sources",
                connect_text(&what, &e)
            ))
        })
    }

    /// The directory holding the source sockets (direct mode): the one given by
    /// flag or environment, else the default. The clients take it as text, so a
    /// path that is not UTF-8 is an error.
    pub fn sources_dir(&self) -> Result<&str, NodeError> {
        let dir = self
            .options
            .sources_dir
            .as_deref()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new(DEFAULT_SOURCES_DIR));
        dir.to_str().ok_or(NodeError::SourcesDirNotUtf8)
    }

    /// Connect to the admin service via gateway.
    ///
    /// Fails with a message when there is no gateway to connect to (local IPC).
    pub fn admin(&self) -> Result<GatewayClient, NodeError> {
        let gateway = self.gateway()?.ok_or(NodeError::NeedsGateway)?;
        GatewayClient::connect(&gateway).map_err(|e| connect_failed("gateway", &e, false))
    }
}

/// One layer of text for a failed connect: see [`connect_failure`].
fn connect_text(what: &str, err: &NngClientError) -> String {
    match err {
        NngClientError::Connection(e) => connect_failure(what, e),
        other => connect_failure_text(what, other),
    }
}

/// "Failed to connect to <what> at <address>: <error>", plus a hint when talking directly.
pub fn connect_failed(what: &str, err: &NngClientError, direct: bool) -> NodeError {
    let text = connect_text(what, err);
    if direct {
        NodeError::Connect(format!("{text}\nIs mdma-{what} running?"))
    } else {
        NodeError::Connect(text)
    }
}

/// Like [`connect_failed`] for the library client's error, which wraps the transport's.
pub fn library_connect_failed(err: &ClientError, direct: bool) -> NodeError {
    match err {
        ClientError::Transport(e) => connect_failed("library", e, direct),
        other => NodeError::Connect(connect_failure_text("library", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dead_node;
    use node_discovery::{RemoteTarget, Source};
    use pretty_assertions::assert_eq;
    use std::error::Error as _;

    fn remote(ip: Ipv4Addr) -> Target {
        Target::Remote(RemoteTarget {
            host: "mdma-909.local".to_string(),
            ip: Some(ip),
            port: 5555,
            source: Source::Cache,
        })
    }

    #[test]
    fn gateway_uses_resolved_target() {
        // The resolved target's IP and port give the gateway address
        let node = Node::with_target(
            NodeOptions::default(),
            remote(Ipv4Addr::new(192, 168, 0, 9)),
        );
        assert_eq!(
            node.gateway().unwrap(),
            Some("tcp://192.168.0.9:5555".to_string())
        );
        assert_eq!(
            node.event_gateway().unwrap(),
            Some("tcp://192.168.0.9:5556".to_string())
        );
    }

    #[test]
    fn require_node_fails_for_local_ipc() {
        let node = Node::with_target(NodeOptions::default(), Target::LocalIpc);
        assert_eq!(
            node.require_node().unwrap_err().to_string(),
            "No remote node to talk to.\n\
             Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
        );
    }

    #[test]
    fn require_node_gives_the_remote_host() {
        let ip = Ipv4Addr::new(192, 168, 0, 9);
        let node = Node::with_target(NodeOptions::default(), remote(ip));
        assert_eq!(
            node.require_node().unwrap(),
            ("mdma-909.local".to_string(), Some(ip))
        );
    }

    #[test]
    fn gateway_is_none_for_local_ipc() {
        // A local IPC target means no gateway
        let node = Node::with_target(NodeOptions::default(), Target::LocalIpc);
        assert_eq!(node.gateway().unwrap(), None);
        assert_eq!(node.event_gateway().unwrap(), None);
    }

    fn local_node_in(dir: &tempfile::TempDir) -> Node {
        let dir = dir.path().to_str().expect("utf-8 path");
        Node::with_target(
            NodeOptions {
                node: None,
                library_socket: Some(format!("ipc://{dir}/none.sock")),
                playback_socket: Some(format!("ipc://{dir}/none-playback.sock")),
                sources_dir: Some(PathBuf::from(dir)),
            },
            Target::LocalIpc,
        )
    }

    #[test]
    fn each_default_is_resolved_by_one_accessor_and_empty_means_unset() {
        let node = Node::with_target(NodeOptions::default(), Target::LocalIpc);
        assert_eq!(node.library_socket(), DEFAULT_LIBRARY_SOCKET);
        assert_eq!(node.playback_socket(), DEFAULT_PLAYBACK_SOCKET);
        assert_eq!(node.sources_dir().unwrap(), DEFAULT_SOURCES_DIR);
        assert!(!node.sockets_given());

        let empty = Node::with_target(
            NodeOptions {
                node: None,
                library_socket: Some(String::new()),
                playback_socket: Some(String::new()),
                sources_dir: Some(PathBuf::new()),
            },
            Target::LocalIpc,
        );
        assert_eq!(empty.library_socket(), DEFAULT_LIBRARY_SOCKET);
        assert_eq!(empty.playback_socket(), DEFAULT_PLAYBACK_SOCKET);
        assert_eq!(empty.sources_dir().unwrap(), DEFAULT_SOURCES_DIR);
        assert!(!empty.sockets_given());

        let dir = tempfile::tempdir().expect("temp dir");
        let given = local_node_in(&dir);
        assert!(given.library_socket().ends_with("/none.sock"));
        assert!(given.playback_socket().ends_with("/none-playback.sock"));
        assert_eq!(given.sources_dir().unwrap(), dir.path().to_str().unwrap());
        assert!(given.sockets_given());
    }

    #[test]
    fn library_connect_failure_over_ipc_says_what_to_start() {
        let dir = tempfile::tempdir().expect("temp dir");
        let text = local_node_in(&dir).library().err().unwrap().to_string();
        assert_eq!(
            text,
            format!(
                "Failed to connect to library at ipc://{}/none.sock: Connection refused\n\
                 Is mdma-library running?",
                dir.path().display()
            )
        );
    }

    #[test]
    fn playback_connect_failure_over_ipc_says_what_to_start() {
        let dir = tempfile::tempdir().expect("temp dir");
        let text = local_node_in(&dir).playback().err().unwrap().to_string();
        assert_eq!(
            text,
            format!(
                "Failed to connect to playback at ipc://{}/none-playback.sock: Connection refused\n\
                 Is mdma-playback running?",
                dir.path().display()
            )
        );
    }

    #[test]
    fn source_connect_failure_names_the_source() {
        let dir = tempfile::tempdir().expect("temp dir");
        let text = local_node_in(&dir)
            .source("bandcamp", None)
            .err()
            .unwrap()
            .to_string();
        assert_eq!(
            text,
            format!(
                "Failed to connect to source 'bandcamp' at ipc://{}/bandcamp.sock: \
                 Connection refused\n\
                 Is mdma-bandcamp running?\n\
                 Hint: 'mdma source list' shows the available sources",
                dir.path().display()
            )
        );
    }

    #[test]
    fn admin_needs_a_gateway_over_ipc() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            local_node_in(&dir).admin().err().unwrap().to_string(),
            "admin commands need a gateway; use --node (or MDMA_NODE) to name the node"
        );
    }

    #[test]
    fn library_connect_failure_to_a_dead_node_has_no_ipc_hint() {
        let (target, port, _guard) = dead_node();
        let node = Node::with_target(NodeOptions::default(), target);
        assert_eq!(
            node.library().err().unwrap().to_string(),
            format!("Failed to connect to library at tcp://127.0.0.1:{port}: Connection refused")
        );
    }

    #[test]
    fn source_connect_failure_to_a_dead_node_has_no_ipc_hint() {
        let (target, port, _guard) = dead_node();
        let node = Node::with_target(NodeOptions::default(), target);
        assert_eq!(
            node.source("bandcamp", None).err().unwrap().to_string(),
            format!(
                "Failed to connect to source 'bandcamp' at tcp://127.0.0.1:{port}: \
                 Connection refused\n\
                 Hint: 'mdma source list' shows the available sources"
            )
        );
    }

    #[test]
    fn admin_connect_failure_to_a_dead_node_names_the_gateway() {
        let (target, port, _guard) = dead_node();
        let node = Node::with_target(NodeOptions::default(), target);
        assert_eq!(
            node.admin().err().unwrap().to_string(),
            format!("Failed to connect to gateway at tcp://127.0.0.1:{port}: Connection refused")
        );
    }

    #[test]
    fn node_is_not_resolved_until_needed() {
        // Nothing resolves a node on construction, only on first use of the target.
        // The one deliberate use of `Node::new` in tests: it is what checks that
        // `Node::new` is lazy. Every other test uses `Node::with_target`.
        let node = Node::new(NodeOptions::default());
        assert!(node.target.get().is_none());
        let node = Node::with_target(NodeOptions::default(), Target::LocalIpc);
        assert_eq!(node.target().unwrap(), &Target::LocalIpc);
    }

    #[test]
    fn local_ipc_for_strips_the_ipc_scheme_before_looking_for_the_socket() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let existing = format!("ipc://{}", file.path().display());
        assert!(local_ipc_for(&existing, false));
        let missing = format!("ipc://{}.missing", file.path().display());
        assert!(!local_ipc_for(&missing, false));
    }

    #[test]
    fn node_error_texts_are_not_repeated_through_a_source() {
        // The message is the whole text; a `source` would print it twice in a
        // caller that walks the error chain.
        assert!(NodeError::Resolve(ResolveError::NoNodesFound)
            .source()
            .is_none());
    }
}
