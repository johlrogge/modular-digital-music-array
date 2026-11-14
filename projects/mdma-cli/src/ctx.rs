//! Command context: the global options and the node resolved from them.
//!
//! Every command receives a [`Ctx`] and asks it for the connections it needs.
//! The node is resolved lazily, the first time something needs it, so commands
//! that never talk to a node (`mdma node ...`, completions) never resolve one.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use color_eyre::eyre::eyre;
use color_eyre::Result;
use gateway_client::GatewayClient;
use hicli::Arg;
use library_ipc_client::ClientError;
use mdma_client::{LibraryBackend, PlaybackBackend, SourceClient};
use nng_transport::NngClientError;
use node_discovery::{NodeSpec, Target};

use crate::node;
use crate::output::{connect_failure, connect_failure_text, source_label};

pub const DEFAULT_LIBRARY_SOCKET: &str = "ipc:///run/mdma/library.sock";
pub const DEFAULT_PLAYBACK_SOCKET: &str = "ipc:///run/mdma/playback.sock";
pub const DEFAULT_SOURCES_DIR: &str = "/run/mdma/sources";

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

/// Why admin commands fail without a gateway (without hicli's `Error: ` prefix).
const ADMIN_NEEDS_GATEWAY: &str =
    "admin commands need a gateway; use --node (or MDMA_NODE) to name the node";

pub struct Ctx {
    globals: Globals,
    local_ipc: bool,
    target: OnceLock<Target>,
}

/// `value`, unless it is empty (which counts as not given).
fn given(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

impl Ctx {
    pub fn new(globals: Globals) -> Self {
        let mut ctx = Self {
            globals,
            local_ipc: false,
            target: OnceLock::new(),
        };
        let library_socket = ctx.library_socket();
        let library_socket = library_socket
            .strip_prefix("ipc://")
            .unwrap_or(library_socket);
        ctx.local_ipc =
            node::local_ipc_wanted(ctx.sockets_given(), Path::new(library_socket).exists());
        ctx
    }

    /// The library socket: the one given by flag or environment, else the default.
    pub fn library_socket(&self) -> &str {
        given(&self.globals.socket.0).unwrap_or(DEFAULT_LIBRARY_SOCKET)
    }

    /// The playback socket: the one given by flag or environment, else the default.
    pub fn playback_socket(&self) -> &str {
        given(&self.globals.playback_socket.0).unwrap_or(DEFAULT_PLAYBACK_SOCKET)
    }

    /// Whether a library or playback socket was given, as opposed to defaulted.
    pub fn sockets_given(&self) -> bool {
        given(&self.globals.socket.0).is_some() || given(&self.globals.playback_socket.0).is_some()
    }

    /// A context whose node is already resolved.
    #[cfg(test)]
    pub fn with_target(globals: Globals, target: Target) -> Self {
        let ctx = Self {
            globals,
            local_ipc: false,
            target: OnceLock::new(),
        };
        ctx.target
            .set(target)
            .unwrap_or_else(|_| unreachable!("target was just created empty"));
        ctx
    }

    /// The node to talk to, resolved on first use. Prints a note for implicit
    /// choices and fails with advice when no node can be resolved.
    pub fn target(&self) -> Result<&Target> {
        // `OnceLock::get_or_try_init` is unstable.
        if let Some(target) = self.target.get() {
            return Ok(target);
        }
        let resolved = node::resolve_startup(self.globals.node.0.as_ref(), self.local_ipc)?;
        Ok(self.target.get_or_init(|| resolved))
    }

    /// The `--node` flag, as given.
    pub fn node_flag(&self) -> Option<&NodeSpec> {
        self.globals.node.0.as_ref()
    }

    /// Whether the local services are to be reached over IPC.
    pub fn local_ipc(&self) -> bool {
        self.local_ipc
    }

    /// The effective gateway address of the resolved node: `tcp://<ip>:<port>`.
    /// `None` for local IPC, where callers fall back to direct IPC.
    pub fn gateway(&self) -> Result<Option<String>> {
        Ok(node::client_config(Some(self.target()?)).gateway_addr())
    }

    /// The effective event gateway address: `tcp://<ip>:5556`.
    /// `None` for local IPC.
    pub fn event_gateway(&self) -> Result<Option<String>> {
        Ok(node::client_config(Some(self.target()?)).event_addr())
    }

    /// Host and IP of the resolved node, for tools that are not NNG (scp, HTTP).
    /// `None` for local IPC.
    pub fn remote_host(&self) -> Result<Option<(String, Option<Ipv4Addr>)>> {
        Ok(node::remote_host(Some(self.target()?)))
    }

    /// Require a remote node and return its hostname and, when known, its IP.
    ///
    /// Fails when the CLI talks to local services over IPC.
    pub fn require_node(&self) -> Result<(String, Option<Ipv4Addr>)> {
        self.remote_host()?.ok_or_else(|| {
            eyre!(
                "No remote node to talk to.\n\
                 Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
            )
        })
    }

    /// Connect to the library backend (gateway or direct).
    pub fn library(&self) -> Result<LibraryBackend> {
        let gateway = self.gateway()?;
        LibraryBackend::connect(gateway.as_deref(), self.library_socket())
            .map_err(|e| library_connect_failed(&e, gateway.is_none()))
    }

    /// Connect to the playback backend (gateway or direct).
    pub fn playback(&self) -> Result<PlaybackBackend> {
        let gateway = self.gateway()?;
        PlaybackBackend::connect(gateway.as_deref(), self.playback_socket())
            .map_err(|e| connect_failed("playback", &e, gateway.is_none()))
    }

    /// Connect to a source, via gateway or direct IPC, with the default request
    /// timeout or a custom one.
    pub fn source(&self, name: &str, timeout: Option<Duration>) -> Result<SourceClient> {
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
            eyre!(
                "{}{running}\nHint: 'mdma source list' shows the available sources",
                connect_text(&what, &e)
            )
        })
    }

    /// The directory holding the source sockets (direct mode): the one given by
    /// flag or environment, else the default. The clients take it as text, so a
    /// path that is not UTF-8 is an error.
    pub fn sources_dir(&self) -> Result<&str> {
        let dir = self
            .globals
            .sources_dir
            .0
            .as_deref()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new(DEFAULT_SOURCES_DIR));
        dir.to_str()
            .ok_or_else(|| eyre!("the sources directory is not valid UTF-8"))
    }

    /// Connect to the admin service via gateway.
    ///
    /// Fails with a message when there is no gateway to connect to (local IPC).
    pub fn admin(&self) -> Result<GatewayClient> {
        let gateway = self.gateway()?.ok_or_else(|| eyre!(ADMIN_NEEDS_GATEWAY))?;
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
fn connect_failed(what: &str, err: &NngClientError, direct: bool) -> color_eyre::Report {
    let text = connect_text(what, err);
    if direct {
        eyre!("{text}\nIs mdma-{what} running?")
    } else {
        eyre!("{text}")
    }
}

/// Like [`connect_failed`] for the library client's error, which wraps the transport's.
fn library_connect_failed(err: &ClientError, direct: bool) -> color_eyre::Report {
    match err {
        ClientError::Transport(e) => connect_failed("library", e, direct),
        other => eyre!("{}", connect_failure_text("library", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dead_node, failure_text, globals};
    use pretty_assertions::assert_eq;

    fn remote(ip: Ipv4Addr) -> Target {
        Target::Remote(node_discovery::RemoteTarget {
            host: "mdma-909.local".to_string(),
            ip: Some(ip),
            port: 5555,
            source: node_discovery::Source::Cache,
        })
    }

    #[test]
    fn gateway_uses_resolved_target() {
        // The resolved target's IP and port give the gateway address
        let ctx = Ctx::with_target(globals(), remote(Ipv4Addr::new(192, 168, 0, 9)));
        assert_eq!(
            ctx.gateway().unwrap(),
            Some("tcp://192.168.0.9:5555".to_string())
        );
        assert_eq!(
            ctx.event_gateway().unwrap(),
            Some("tcp://192.168.0.9:5556".to_string())
        );
    }

    #[test]
    fn require_node_fails_for_local_ipc() {
        let ctx = Ctx::with_target(globals(), Target::LocalIpc);
        assert_eq!(
            failure_text(ctx.require_node()),
            "No remote node to talk to.\n\
             Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
        );
    }

    #[test]
    fn require_node_gives_the_remote_host() {
        let ip = Ipv4Addr::new(192, 168, 0, 9);
        let ctx = Ctx::with_target(globals(), remote(ip));
        assert_eq!(
            ctx.require_node().unwrap(),
            ("mdma-909.local".to_string(), Some(ip))
        );
    }

    #[test]
    fn gateway_is_none_for_local_ipc() {
        // A local IPC target means no gateway
        let ctx = Ctx::with_target(globals(), Target::LocalIpc);
        assert_eq!(ctx.gateway().unwrap(), None);
        assert_eq!(ctx.event_gateway().unwrap(), None);
    }

    fn local_ctx_in(dir: &tempfile::TempDir) -> Ctx {
        let dir = dir.path().to_str().expect("utf-8 path");
        Ctx::with_target(
            Globals {
                node: NodeFlag(None),
                socket: LibrarySocket(Some(format!("ipc://{dir}/none.sock"))),
                playback_socket: PlaybackSocket(Some(format!("ipc://{dir}/none-playback.sock"))),
                sources_dir: SourcesDir(Some(PathBuf::from(dir))),
            },
            Target::LocalIpc,
        )
    }

    #[test]
    fn each_default_is_resolved_by_one_accessor_and_empty_means_unset() {
        let ctx = Ctx::with_target(globals(), Target::LocalIpc);
        assert_eq!(ctx.library_socket(), DEFAULT_LIBRARY_SOCKET);
        assert_eq!(ctx.playback_socket(), DEFAULT_PLAYBACK_SOCKET);
        assert_eq!(ctx.sources_dir().unwrap(), DEFAULT_SOURCES_DIR);
        assert!(!ctx.sockets_given());

        let empty = Ctx::with_target(
            Globals {
                node: NodeFlag(None),
                socket: LibrarySocket(Some(String::new())),
                playback_socket: PlaybackSocket(Some(String::new())),
                sources_dir: SourcesDir(Some(PathBuf::new())),
            },
            Target::LocalIpc,
        );
        assert_eq!(empty.library_socket(), DEFAULT_LIBRARY_SOCKET);
        assert_eq!(empty.playback_socket(), DEFAULT_PLAYBACK_SOCKET);
        assert_eq!(empty.sources_dir().unwrap(), DEFAULT_SOURCES_DIR);
        assert!(!empty.sockets_given());

        let dir = tempfile::tempdir().expect("temp dir");
        let given = local_ctx_in(&dir);
        assert!(given.library_socket().ends_with("/none.sock"));
        assert!(given.playback_socket().ends_with("/none-playback.sock"));
        assert_eq!(given.sources_dir().unwrap(), dir.path().to_str().unwrap());
        assert!(given.sockets_given());
    }

    #[test]
    fn library_connect_failure_over_ipc_says_what_to_start() {
        let dir = tempfile::tempdir().expect("temp dir");
        let text = failure_text(local_ctx_in(&dir).library());
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
        let text = failure_text(local_ctx_in(&dir).playback());
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
        let text = failure_text(local_ctx_in(&dir).source("bandcamp", None));
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
            failure_text(local_ctx_in(&dir).admin()),
            "admin commands need a gateway; use --node (or MDMA_NODE) to name the node"
        );
    }

    #[test]
    fn library_connect_failure_to_a_dead_node_has_no_ipc_hint() {
        let (target, port, _guard) = dead_node();
        let ctx = Ctx::with_target(globals(), target);
        assert_eq!(
            failure_text(ctx.library()),
            format!("Failed to connect to library at tcp://127.0.0.1:{port}: Connection refused")
        );
    }

    #[test]
    fn source_connect_failure_to_a_dead_node_has_no_ipc_hint() {
        let (target, port, _guard) = dead_node();
        let ctx = Ctx::with_target(globals(), target);
        assert_eq!(
            failure_text(ctx.source("bandcamp", None)),
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
        let ctx = Ctx::with_target(globals(), target);
        assert_eq!(
            failure_text(ctx.admin()),
            format!("Failed to connect to gateway at tcp://127.0.0.1:{port}: Connection refused")
        );
    }

    #[test]
    fn node_is_not_resolved_until_needed() {
        // Replaces the old "node commands do not need a resolved node": nothing
        // resolves a node on construction, only on first use of the target.
        // The one deliberate use of `Ctx::new` in tests: it is what checks that
        // `Ctx::new` is lazy. Every other test uses `Ctx::with_target`.
        let ctx = Ctx::new(globals());
        assert!(ctx.target.get().is_none());
        let ctx = Ctx::with_target(globals(), Target::LocalIpc);
        assert_eq!(ctx.target().unwrap(), &Target::LocalIpc);
    }
}
