//! Command context: the global options and the node resolved from them.
//!
//! Every command receives a [`Ctx`] and asks it for the connections it needs.
//! The node is resolved lazily, the first time something needs it, so commands
//! that never talk to a node (`mdma node ...`, completions) never resolve one.

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use bpaf::Bpaf;
use gateway_client::GatewayClient;
use mdma_client::{LibraryBackend, PlaybackBackend, SourceClient};
use node_discovery::{NodeSpec, Target};

use crate::node;

pub const DEFAULT_LIBRARY_SOCKET: &str = "ipc:///run/mdma/library.sock";
pub const DEFAULT_PLAYBACK_SOCKET: &str = "ipc:///run/mdma/playback.sock";

// The global options, as given on the command line or in the environment.
//
// `socket` and `playback_socket` are `None` when left at their defaults, so
// that "given explicitly" can be told apart from "defaulted".
#[derive(Debug, Clone, Bpaf)]
pub struct Globals {
    /// Node hostname (e.g. mdma-909.local), optionally with a port. Also read from
    /// MDMA_NODE. Without either, the remembered or discovered node is used.
    #[bpaf(long, argument("NODE"))]
    pub node: Option<NodeSpec>,
    /// Library IPC socket address (direct mode, ignored when --node is set).
    /// Default: ipc:///run/mdma/library.sock
    #[bpaf(long, env("MDMA_LIBRARY_SOCKET"), argument("SOCKET"))]
    pub socket: Option<String>,
    /// Playback server socket address (direct mode, ignored when --node is set).
    /// Default: ipc:///run/mdma/playback.sock
    #[bpaf(long, env("MDMA_PLAYBACK_SOCKET"), argument("PLAYBACK_SOCKET"))]
    pub playback_socket: Option<String>,
    /// Sources directory for direct mode (contains *.sock files)
    #[bpaf(
        long,
        env("MDMA_SOURCES_DIR"),
        argument("SOURCES_DIR"),
        fallback("/run/mdma/sources".to_string()),
        display_fallback
    )]
    pub sources_dir: String,
}

pub struct Ctx {
    globals: Globals,
    local_ipc: bool,
    target: OnceLock<Target>,
}

impl Ctx {
    pub fn new(globals: Globals) -> Self {
        let library_socket = globals.socket.as_deref().unwrap_or(DEFAULT_LIBRARY_SOCKET);
        let library_socket = library_socket
            .strip_prefix("ipc://")
            .unwrap_or(library_socket);
        let sockets_given = globals.socket.is_some() || globals.playback_socket.is_some();
        let local_ipc = node::local_ipc_wanted(sockets_given, Path::new(library_socket).exists());
        Self {
            globals,
            local_ipc,
            target: OnceLock::new(),
        }
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
    /// choices and exits with advice on failure.
    pub fn target(&self) -> &Target {
        self.target
            .get_or_init(|| node::resolve_startup(self.globals.node.as_ref(), self.local_ipc))
    }

    /// The `--node` flag, as given.
    pub fn node_flag(&self) -> Option<&NodeSpec> {
        self.globals.node.as_ref()
    }

    /// Whether the local services are to be reached over IPC.
    pub fn local_ipc(&self) -> bool {
        self.local_ipc
    }

    /// The effective gateway address of the resolved node: `tcp://<ip>:<port>`.
    /// `None` for local IPC, where callers fall back to direct IPC.
    pub fn gateway(&self) -> Option<String> {
        node::client_config(Some(self.target())).gateway_addr()
    }

    /// The effective event gateway address: `tcp://<ip>:5556`.
    /// `None` for local IPC.
    pub fn event_gateway(&self) -> Option<String> {
        node::client_config(Some(self.target())).event_addr()
    }

    /// Host and IP of the resolved node, for tools that are not NNG (scp, HTTP).
    /// `None` for local IPC.
    pub fn remote_host(&self) -> Option<(String, Option<Ipv4Addr>)> {
        node::remote_host(Some(self.target()))
    }

    /// Require a remote node and return its hostname and, when known, its IP.
    ///
    /// Prints an error and exits when the CLI talks to local services over IPC.
    pub fn require_node(&self) -> (String, Option<Ipv4Addr>) {
        match self.remote_host() {
            Some(remote) => remote,
            None => {
                eprintln!("No remote node to talk to.");
                eprintln!(
                    "Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
                );
                std::process::exit(1);
            }
        }
    }

    /// Connect to the library backend (gateway or direct).
    pub fn library(&self) -> LibraryBackend {
        let gateway = self.gateway();
        let socket = self
            .globals
            .socket
            .as_deref()
            .unwrap_or(DEFAULT_LIBRARY_SOCKET);
        match LibraryBackend::connect(gateway.as_deref(), socket) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to connect to library: {}", e);
                if gateway.is_none() {
                    eprintln!("Is mdma-library running?");
                }
                std::process::exit(1);
            }
        }
    }

    /// Connect to the playback backend (gateway or direct).
    pub fn playback(&self) -> PlaybackBackend {
        let gateway = self.gateway();
        let socket = self
            .globals
            .playback_socket
            .as_deref()
            .unwrap_or(DEFAULT_PLAYBACK_SOCKET);
        match PlaybackBackend::connect(gateway.as_deref(), socket) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to connect to playback: {}", e);
                if gateway.is_none() {
                    eprintln!("Is mdma-playback running?");
                }
                std::process::exit(1);
            }
        }
    }

    /// Connect to a source, via gateway or direct IPC, with the default request
    /// timeout or a custom one.
    pub fn source(&self, name: &str, timeout: Option<Duration>) -> SourceClient {
        let gateway = self.gateway();
        let connected = match timeout {
            Some(t) => SourceClient::connect_with_timeout(
                gateway.as_deref(),
                &self.globals.sources_dir,
                name,
                t,
            ),
            None => SourceClient::connect(gateway.as_deref(), &self.globals.sources_dir, name),
        };
        match connected {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{}", e);
                if gateway.is_none() {
                    eprintln!("Is mdma-{} running?", name);
                }
                eprintln!("Hint: 'mdma source list' shows the available sources");
                std::process::exit(1);
            }
        }
    }

    /// The directory holding the source sockets (direct mode).
    pub fn sources_dir(&self) -> &str {
        &self.globals.sources_dir
    }

    /// Connect to the admin service via gateway.
    ///
    /// Exits with a message when there is no gateway to connect to (local IPC).
    pub fn admin(&self) -> GatewayClient {
        let gateway = self.gateway().unwrap_or_else(|| {
            eprintln!(
                "error: admin commands need a gateway; use --node (or MDMA_NODE) to name the node"
            );
            std::process::exit(1);
        });
        match GatewayClient::connect(&gateway) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to connect to gateway: {}", e);
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn globals() -> Globals {
        Globals {
            node: None,
            socket: None,
            playback_socket: None,
            sources_dir: "/run/mdma/sources".to_string(),
        }
    }

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
        assert_eq!(ctx.gateway(), Some("tcp://192.168.0.9:5555".to_string()));
        assert_eq!(
            ctx.event_gateway(),
            Some("tcp://192.168.0.9:5556".to_string())
        );
    }

    #[test]
    fn gateway_is_none_for_local_ipc() {
        // A local IPC target means no gateway
        let ctx = Ctx::with_target(globals(), Target::LocalIpc);
        assert_eq!(ctx.gateway(), None);
        assert_eq!(ctx.event_gateway(), None);
    }

    #[test]
    fn node_is_not_resolved_until_needed() {
        // Replaces the old "node commands do not need a resolved node": nothing
        // resolves a node on construction, only on first use of the target.
        let ctx = Ctx::new(globals());
        assert!(ctx.target.get().is_none());
        let ctx = Ctx::with_target(globals(), Target::LocalIpc);
        assert_eq!(ctx.target(), &Target::LocalIpc);
    }
}
