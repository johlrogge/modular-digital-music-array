//! A node and the connections to its services.
//!
//! Every client asks a [`Node`] for the connections it needs. The node is
//! resolved lazily, the first time something needs it, so commands that never
//! talk to a node (`mdma node ...`, completions) never resolve one.

mod resolve;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use gateway_client::{GatewayClient, IdentifyError, NodeIdentity};
use library_ipc_client::ClientError;
use nng_transport::NngClientError;
use node_discovery::{NodeSpec, RemoteTarget, ResolveError, Target};

use crate::error::{connect_failure, connect_failure_text};
use crate::{source_label, LibraryBackend, PlaybackBackend, SourceClient};

pub use resolve::{
    cache_path, env_node, identify_at, ips, is_tty, load_cache, load_cache_quiet, pick_interactive,
    real_effects, request, resolve_error_message, select_error_message, Interactive, Notes,
    Resolved, NODE_ENV,
};
use resolve::{client_config, local_ipc_wanted, probe_node, remote_host, resolve_startup};

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
    /// `needed_by` could not get the node, for the reason `cause`. See
    /// [`Node::labelled`].
    #[error("{needed_by} needs the node: {cause}")]
    Needed {
        needed_by: NeededBy,
        cause: Box<NodeError>,
    },
}

impl NodeError {
    /// Whether the user cancelled the pick prompt: they chose no node, for the whole
    /// process. Looks through the label ([`NodeError::Needed`]).
    pub fn is_cancelled(&self) -> bool {
        match self {
            NodeError::Resolve(error) => *error == ResolveError::Cancelled,
            NodeError::Needed { cause, .. } => cause.is_cancelled(),
            _ => false,
        }
    }
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

/// What needed the node, as the user would name it ("mdma queue append",
/// "mdma-tui"). It is set once, by whoever creates the [`Node`], and names the
/// cause in [`NodeError::Needed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeededBy(String);

impl NeededBy {
    pub fn new(label: impl Into<String>) -> Self {
        Self(label.into())
    }
}

impl std::fmt::Display for NeededBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a node's gateway answered "who are you". Any of these means the node is
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The node's identity.
    Identity(NodeIdentity),
    /// The gateway is too old to say.
    Unsupported(String),
    /// The gateway answered with an error.
    Failed(String),
}

impl Answer {
    /// The answer in `result`; a failure of the transport is the error.
    pub fn from_identify(
        result: Result<NodeIdentity, IdentifyError>,
    ) -> Result<Self, gateway_client::ClientError> {
        use gateway_client::ClientError;
        match result {
            Ok(identity) => Ok(Self::Identity(identity)),
            Err(IdentifyError::Unsupported(message)) => Ok(Self::Unsupported(message)),
            Err(IdentifyError::Client(ClientError::Service(message))) => Ok(Self::Failed(message)),
            Err(IdentifyError::Client(error)) => Err(error),
        }
    }
}

/// After a failed check, attempts fail at once for this long.
const RETRY_AFTER: Duration = Duration::from_secs(10);

/// What a node does that reaches outside the process, injected so tests need
/// neither a network nor a real clock.
pub struct NodeEffects {
    pub now: Box<dyn Fn() -> Instant + Send + Sync>,
    pub resolve: ResolveFn,
    pub probe: Probe,
}

/// Resolve the node: the `--node` flag, whether the local services are in use,
/// whether a note may be printed, and whether the user may be asked to pick.
pub type ResolveFn = Box<
    dyn Fn(Option<&NodeSpec>, bool, Notes, Interactive) -> Result<Resolved, NodeError>
        + Send
        + Sync,
>;

/// Ask a remote node who it is, bounded in time. The error is the text for the
/// user.
pub type Probe = Box<dyn Fn(&RemoteTarget) -> Result<Answer, String> + Send + Sync>;

impl NodeEffects {
    pub fn real() -> Self {
        Self {
            now: Box::new(Instant::now),
            resolve: Box::new(resolve_startup),
            probe: Box::new(probe_node),
        }
    }
}

/// The last failure of something tried, and when. It is kept for
/// [`RETRY_AFTER`]: until then asking again fails with it, without trying.
#[derive(Clone)]
struct Failure<E> {
    at: Instant,
    error: E,
}

impl<E: Clone> Failure<E> {
    fn standing(&self, now: Instant) -> Option<E> {
        (now.saturating_duration_since(self.at) < RETRY_AFTER).then(|| self.error.clone())
    }
}

/// What this process knows about resolving the node.
#[derive(Default)]
struct Resolution {
    /// Resolution has been tried: later tries are not interactive and print nothing.
    attempted: bool,
    failure: Option<Failure<ResolveError>>,
}

/// What this process knows about whether the remote node answers.
enum Reach {
    Untried,
    Reached(Answer),
    Failed(Failure<String>),
}

pub struct Node {
    needed_by: NeededBy,
    options: NodeOptions,
    local_ipc: bool,
    target: OnceLock<Target>,
    effects: NodeEffects,
    resolution: Mutex<Resolution>,
    reach: Mutex<Reach>,
    // Each connection is made on first use and kept after the first success. A
    // failure is not kept. Handing out a connection hands out a clone that
    // shares the cached socket, which serialises requests. On a remote node the
    // library, playback and admin connections are one gateway socket. A
    // connection that a request found dead is dropped (see `Node::live`).
    library: Mutex<Option<LibraryBackend>>,
    playback: Mutex<Option<PlaybackBackend>>,
    gateway_client: Mutex<Option<GatewayClient>>,
    /// Why each group of connections was last dropped as dead, and when: until
    /// [`RETRY_AFTER`] has passed, asking for a connection of the group fails with it.
    dropped: Mutex<[Option<Failure<String>>; 3]>,
}

/// A connection that can go dead on its own. On a remote node they share one socket.
#[derive(Clone, Copy)]
enum Group {
    Library,
    Playback,
    Gateway,
}

impl Group {
    const ALL: [Group; 3] = [Group::Library, Group::Playback, Group::Gateway];
}

/// The connection in `slot`, connecting with `connect` when there is none yet.
fn cached<T: Clone>(
    slot: &Mutex<Option<T>>,
    connect: impl FnOnce() -> Result<T, NodeError>,
) -> Result<T, NodeError> {
    if let Some(connection) = locked(slot).as_ref() {
        return Ok(connection.clone());
    }
    let connection = connect()?;
    Ok(locked(slot).get_or_insert(connection).clone())
}

/// The guard of `mutex`; what a panic left behind is still a valid state.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the local services are the node in use: sockets were given explicitly, or
/// the library socket (`ipc://<path>` or a bare path) exists here. The one rule for
/// `Node::needed_by` and for completion, which has no `Node` and reads the environment.
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
    /// A node whose errors say that `needed_by` needed it. Nothing connects, and
    /// no node is resolved, until something asks for a connection.
    pub fn needed_by(needed_by: NeededBy, options: NodeOptions) -> Self {
        let mut node = Self::unresolved(needed_by, options);
        node.local_ipc = local_ipc_for(node.library_socket(), node.sockets_given());
        node
    }

    fn unresolved(needed_by: NeededBy, options: NodeOptions) -> Self {
        Self {
            needed_by,
            options,
            local_ipc: false,
            target: OnceLock::new(),
            effects: NodeEffects::real(),
            resolution: Mutex::new(Resolution::default()),
            reach: Mutex::new(Reach::Untried),
            library: Mutex::new(None),
            playback: Mutex::new(None),
            gateway_client: Mutex::new(None),
            dropped: Mutex::new([None, None, None]),
        }
    }

    /// Replace what the node does outside the process.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_effects(mut self, effects: NodeEffects) -> Self {
        self.effects = effects;
        self
    }

    /// The one place that says what needed the node: `error`, if it is about
    /// resolving or reaching the node (`Resolve`, `Connect`), wrapped in the label.
    /// Idempotent, so every method that can fail that way may apply it, and so may
    /// a caller that connects on its own. Errors about the arguments (`MDMA_NODE`,
    /// the sources directory), "no remote node" and "needs a gateway" are not about
    /// reaching the node and pass unchanged.
    pub fn labelled(&self, error: NodeError) -> NodeError {
        match error {
            NodeError::Resolve(_) | NodeError::Connect(_) => NodeError::Needed {
                needed_by: self.needed_by.clone(),
                cause: Box::new(error),
            },
            other => other,
        }
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

    /// A node that is already resolved, needed by "mdma".
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_target(options: NodeOptions, target: Target) -> Self {
        let node = Self::unresolved(NeededBy::new("mdma"), options);
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
    /// resolution matters: later calls reuse the cached target. A failure to
    /// resolve is kept like a failure to reach the node: for [`RETRY_AFTER`],
    /// asking again fails with it and neither browses nor prompts.
    pub fn target_with(&self, notes: Notes) -> Result<&Target, NodeError> {
        // `OnceLock::get_or_try_init` is unstable.
        if let Some(target) = self.target.get() {
            return Ok(target);
        }
        let mut resolution = locked(&self.resolution);
        if let Some(failure) = &resolution.failure {
            // Cancelling the prompt is the user's choice of no node, for the whole
            // process; any other failure is tried again after the window.
            let cancelled = failure.error == ResolveError::Cancelled;
            if cancelled || failure.standing((self.effects.now)()).is_some() {
                return Err(self.labelled(NodeError::Resolve(failure.error.clone())));
            }
        }
        // Only the first resolution may ask the user or print a note.
        let (notes, interactive) = if resolution.attempted {
            (Notes::Silent, Interactive::No)
        } else {
            (notes, Interactive::Yes)
        };
        resolution.attempted = true;
        match (self.effects.resolve)(
            self.options.node.as_ref(),
            self.local_ipc,
            notes,
            interactive,
        ) {
            Ok(resolved) => {
                resolution.failure = None;
                if let Some(answer) = resolved.answer {
                    *locked(&self.reach) = Reach::Reached(answer);
                }
                Ok(self.target.get_or_init(|| resolved.target))
            }
            Err(NodeError::Resolve(error)) => {
                resolution.failure = Some(Failure {
                    at: (self.effects.now)(),
                    error: error.clone(),
                });
                Err(self.labelled(NodeError::Resolve(error)))
            }
            Err(other) => Err(self.labelled(other)),
        }
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
    ///
    /// A remote node must answer first: see [`Node::reach`].
    pub fn gateway(&self) -> Result<Option<String>, NodeError> {
        let target = self.target()?;
        self.reach(target)?;
        Ok(client_config(Some(target)).gateway_addr())
    }

    /// The effective event gateway address: `tcp://<ip>:5556`.
    /// `None` for local IPC. Events are an address to subscribe to, not a
    /// connection the node keeps, and a subscriber dials without waiting and
    /// reconnects by itself, so the address is not checked for reachability.
    pub fn event_gateway(&self) -> Result<Option<String>, NodeError> {
        Ok(client_config(Some(self.target()?)).event_addr())
    }

    /// [`Node::event_gateway`] if the node is resolved already; never resolves, so it
    /// is cheap to ask on every tick of a UI.
    pub fn event_address(&self) -> Option<String> {
        client_config(self.target.get()).event_addr()
    }

    /// The one reachability check, for every connection. A remote node is reached
    /// when it has answered "who are you": while the resolver verified it, or to
    /// the probe here, which connects and asks, at most twice
    /// `IDENTIFY_TIMEOUT` (to send and to receive) plus name resolution. The outcome is kept for
    /// the process. After a failure, further attempts fail at once for
    /// [`RETRY_AFTER`], then the check is tried again. Local IPC needs no check.
    fn reach(&self, target: &Target) -> Result<(), NodeError> {
        let Target::Remote(remote) = target else {
            return Ok(());
        };
        let mut reach = locked(&self.reach);
        match &*reach {
            Reach::Reached(_) => return Ok(()),
            Reach::Failed(failure) => {
                if let Some(text) = failure.standing((self.effects.now)()) {
                    return Err(self.labelled(NodeError::Connect(text)));
                }
            }
            Reach::Untried => {}
        }
        match (self.effects.probe)(remote) {
            Ok(answer) => {
                *reach = Reach::Reached(answer);
                Ok(())
            }
            Err(text) => {
                *reach = Reach::Failed(Failure {
                    at: (self.effects.now)(),
                    error: text.clone(),
                });
                Err(self.labelled(NodeError::Connect(text)))
            }
        }
    }

    /// Sticky reach (ADR-030 §4.4): the transport records, on the socket every clone
    /// of a connection shares, that a request found the node unreachable (a timeout,
    /// a refused, reset or closed connection). The cached connections are asked here,
    /// before one is handed out.
    ///
    /// A connection that went dead is dropped together with those that share its
    /// socket: on a remote node that is all of them (they are one gateway socket), on
    /// local IPC only itself, so a hung library leaves playback usable. For
    /// [`RETRY_AFTER`] asking for a connection of a dropped group fails with the
    /// transport's text, without dialling. A remote node must then answer the
    /// reachability check again before it is used.
    fn live(&self, group: Group) -> Result<(), NodeError> {
        let now = (self.effects.now)();
        let remote = matches!(self.target.get(), Some(Target::Remote(_)));
        let seen = [
            (
                Group::Library,
                locked(&self.library)
                    .as_ref()
                    .and_then(LibraryBackend::take_unreachable),
            ),
            (
                Group::Playback,
                locked(&self.playback)
                    .as_ref()
                    .and_then(PlaybackBackend::take_unreachable),
            ),
            (
                Group::Gateway,
                locked(&self.gateway_client)
                    .as_ref()
                    .and_then(GatewayClient::take_unreachable),
            ),
        ];
        for (dead, text) in seen {
            if let Some(text) = text {
                self.drop_dead(
                    dead,
                    remote,
                    Failure {
                        at: now,
                        error: text,
                    },
                );
            }
        }
        match locked(&self.dropped)[group as usize]
            .as_ref()
            .and_then(|failure| failure.standing(now))
        {
            Some(text) => Err(self.labelled(NodeError::Connect(text))),
            None => Ok(()),
        }
    }

    /// Drop the connection of `dead` and everything that shares its socket.
    fn drop_dead(&self, dead: Group, remote: bool, failure: Failure<String>) {
        let groups: &[Group] = if remote {
            &Group::ALL
        } else {
            std::slice::from_ref(&dead)
        };
        for group in groups {
            match group {
                Group::Library => *locked(&self.library) = None,
                Group::Playback => *locked(&self.playback) = None,
                Group::Gateway => *locked(&self.gateway_client) = None,
            }
            locked(&self.dropped)[*group as usize] = Some(failure.clone());
        }
        if remote {
            *locked(&self.reach) = Reach::Failed(failure);
        }
    }

    /// What the remote node said when asked who it is, if it has been asked. It
    /// has not for local IPC, which needs no check, nor before the first
    /// connection.
    pub fn identity(&self) -> Option<Answer> {
        match &*locked(&self.reach) {
            Reach::Reached(answer) => Some(answer.clone()),
            Reach::Untried | Reach::Failed(_) => None,
        }
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

    /// The gateway connection of a remote node, shared by every service.
    /// `what` names the service that asked, for the failure text.
    fn gateway_connection(&self, what: &str, address: &str) -> Result<GatewayClient, NodeError> {
        cached(&self.gateway_client, || {
            GatewayClient::connect(address)
                .map_err(|e| self.labelled(connect_failed(what, &e, false)))
        })
    }

    /// Connect to the library backend (gateway or direct).
    /// Later calls reuse the first successful connection.
    pub fn library(&self) -> Result<LibraryBackend, NodeError> {
        self.live(Group::Library)?;
        cached(&self.library, || match self.gateway()? {
            Some(address) => Ok(LibraryBackend::Gateway(
                self.gateway_connection("library", &address)?,
            )),
            None => LibraryBackend::connect_direct(self.library_socket())
                .map_err(|e| self.labelled(library_connect_failed(&e, true))),
        })
    }

    /// Connect to the playback backend (gateway or direct). Later calls reuse
    /// the first successful connection.
    pub fn playback(&self) -> Result<PlaybackBackend, NodeError> {
        self.live(Group::Playback)?;
        cached(&self.playback, || match self.gateway()? {
            Some(address) => Ok(PlaybackBackend::Gateway(
                self.gateway_connection("playback", &address)?,
            )),
            None => PlaybackBackend::connect_direct(self.playback_socket())
                .map_err(|e| self.labelled(connect_failed("playback", &e, true))),
        })
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
            self.labelled(NodeError::Connect(format!(
                "{}{running}\nHint: 'mdma source list' shows the available sources",
                connect_text(&what, &e)
            )))
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
    /// Later calls reuse the first successful connection.
    pub fn admin(&self) -> Result<GatewayClient, NodeError> {
        self.live(Group::Gateway)?;
        let Some(address) = self.gateway()? else {
            return Err(NodeError::NeedsGateway);
        };
        self.gateway_connection("gateway", &address)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn remote(ip: Ipv4Addr) -> Target {
        Target::Remote(RemoteTarget {
            host: "mdma-909.local".to_string(),
            ip: Some(ip),
            port: 5555,
            source: Source::Cache,
        })
    }

    /// Effects that count what is asked of them, answer as the test says and read
    /// a clock the test moves by hand.
    struct Fake {
        probes: Arc<AtomicUsize>,
        /// How resolution was asked for, each time.
        resolutions: Arc<Mutex<Vec<(Notes, Interactive)>>>,
        /// What resolution fails with.
        resolve_error: Arc<Mutex<ResolveError>>,
        clock: Arc<Mutex<Instant>>,
        /// `None` is a node that answers; `Some` is the text it fails with.
        probe_failure: Arc<Mutex<Option<&'static str>>>,
    }

    impl Fake {
        fn effects(probe_failure: Option<&'static str>) -> (NodeEffects, Self) {
            let fake = Self {
                probes: Arc::new(AtomicUsize::new(0)),
                resolutions: Arc::new(Mutex::new(Vec::new())),
                resolve_error: Arc::new(Mutex::new(ResolveError::NoNodesFound)),
                clock: Arc::new(Mutex::new(Instant::now())),
                probe_failure: Arc::new(Mutex::new(probe_failure)),
            };
            let clock = Arc::clone(&fake.clock);
            let probes = Arc::clone(&fake.probes);
            let failure = Arc::clone(&fake.probe_failure);
            let resolutions = Arc::clone(&fake.resolutions);
            let resolve_error = Arc::clone(&fake.resolve_error);
            let effects = NodeEffects {
                now: Box::new(move || *clock.lock().unwrap()),
                resolve: Box::new(move |_, _, notes, interactive| {
                    resolutions.lock().unwrap().push((notes, interactive));
                    Err(NodeError::Resolve(resolve_error.lock().unwrap().clone()))
                }),
                probe: Box::new(move |_| {
                    probes.fetch_add(1, Ordering::SeqCst);
                    match *failure.lock().unwrap() {
                        None => Ok(Answer::Failed("not asked".to_string())),
                        Some(text) => Err(text.to_string()),
                    }
                }),
            };
            (effects, fake)
        }

        fn probes(&self) -> usize {
            self.probes.load(Ordering::SeqCst)
        }

        fn resolutions(&self) -> usize {
            self.resolutions.lock().unwrap().len()
        }

        fn advance(&self, by: Duration) {
            *self.clock.lock().unwrap() += by;
        }

        fn answer_from_now_on(&self) {
            *self.probe_failure.lock().unwrap() = None;
        }
    }

    fn remote_node(needed_by: &str, probe_failure: Option<&'static str>) -> (Node, Fake) {
        let (effects, fake) = Fake::effects(probe_failure);
        let mut node = Node::with_target(
            NodeOptions::default(),
            remote(Ipv4Addr::new(192, 168, 0, 9)),
        )
        .with_effects(effects);
        node.needed_by = NeededBy::new(needed_by);
        (node, fake)
    }

    #[test]
    fn constructing_and_resolving_a_node_does_not_connect() {
        let (node, fake) = remote_node("mdma queue list", Some("down"));
        node.target().unwrap();
        node.remote_host().unwrap();
        node.require_node().unwrap();
        assert_eq!(fake.probes(), 0);
    }

    #[test]
    fn a_reachable_node_is_probed_once() {
        let (node, fake) = remote_node("mdma queue list", None);
        for _ in 0..3 {
            assert_eq!(
                node.gateway().unwrap(),
                Some("tcp://192.168.0.9:5555".to_string())
            );
        }
        node.event_gateway().unwrap();
        assert_eq!(fake.probes(), 1);
    }

    #[test]
    fn a_node_the_resolver_verified_is_not_probed() {
        let (node, fake) = remote_node("mdma queue list", Some("down"));
        let answer = Answer::Identity(NodeIdentity {
            id: Some("mdma-909".to_string()),
            ..identity_fields()
        });
        *locked(&node.reach) = Reach::Reached(answer.clone());
        node.gateway().unwrap();
        assert_eq!(fake.probes(), 0);
        assert_eq!(node.identity(), Some(answer));
    }

    fn identity_fields() -> NodeIdentity {
        NodeIdentity {
            id: None,
            name: None,
            version: "0.0.0".to_string(),
            proto: 1,
        }
    }

    #[test]
    fn an_unreachable_node_fails_at_once_for_ten_seconds_then_is_tried_again() {
        let (node, fake) = remote_node("mdma queue append", Some("no answer"));
        let text = node.library().err().unwrap().to_string();
        assert_eq!(text, "mdma queue append needs the node: no answer");
        assert_eq!(fake.probes(), 1);

        // Every way in fails without a new attempt.
        fake.advance(Duration::from_secs(9));
        assert_eq!(node.library().err().unwrap().to_string(), text);
        assert_eq!(node.playback().err().unwrap().to_string(), text);
        assert_eq!(node.admin().err().unwrap().to_string(), text);
        assert_eq!(node.gateway().err().unwrap().to_string(), text);
        assert_eq!(fake.probes(), 1);

        fake.advance(Duration::from_secs(1));
        assert_eq!(node.library().err().unwrap().to_string(), text);
        assert_eq!(fake.probes(), 2);
    }

    #[test]
    fn a_node_that_comes_back_is_used_after_the_retry_time() {
        let (node, fake) = remote_node("mdma queue list", Some("no answer"));
        assert!(node.gateway().is_err());
        fake.answer_from_now_on();
        assert!(node.gateway().is_err(), "still within the retry time");
        fake.advance(RETRY_AFTER);
        assert!(node.gateway().is_ok());
        assert_eq!(fake.probes(), 2);
    }

    #[test]
    fn a_failure_to_resolve_is_kept_like_a_failure_to_reach() {
        let (effects, fake) = Fake::effects(None);
        let node = Node::needed_by(NeededBy::new("mdma queue list"), NodeOptions::default())
            .with_effects(effects);
        let text = node.target().err().unwrap().to_string();
        assert!(
            text.starts_with("mdma queue list needs the node: No MDMA nodes found"),
            "{text}"
        );
        assert_eq!(fake.resolutions(), 1);

        // Within the window neither browsing nor prompting happens again.
        fake.advance(Duration::from_secs(9));
        assert_eq!(node.target().err().unwrap().to_string(), text);
        assert_eq!(node.library().err().unwrap().to_string(), text);
        assert_eq!(fake.resolutions(), 1);

        fake.advance(Duration::from_secs(1));
        assert!(node.target().is_err());
        assert_eq!(fake.resolutions(), 2);
    }

    #[test]
    fn only_the_first_resolution_is_interactive_and_prints() {
        let (effects, fake) = Fake::effects(None);
        let node = Node::needed_by(NeededBy::new("mdma queue list"), NodeOptions::default())
            .with_effects(effects);
        assert!(node.target().is_err());
        fake.advance(RETRY_AFTER);
        assert!(node.target().is_err());
        assert_eq!(
            *fake.resolutions.lock().unwrap(),
            vec![
                (Notes::Print, Interactive::Yes),
                (Notes::Silent, Interactive::No)
            ]
        );
    }

    #[test]
    fn a_cancelled_prompt_is_not_asked_again() {
        let (effects, fake) = Fake::effects(None);
        *fake.resolve_error.lock().unwrap() = ResolveError::Cancelled;
        let node = Node::needed_by(NeededBy::new("mdma queue list"), NodeOptions::default())
            .with_effects(effects);
        let text = node.target().err().unwrap().to_string();
        assert_eq!(
            text,
            "mdma queue list needs the node: Node selection cancelled."
        );
        fake.advance(RETRY_AFTER * 10);
        assert_eq!(node.target().err().unwrap().to_string(), text);
        assert_eq!(fake.resolutions(), 1);
    }

    #[test]
    fn every_error_about_reaching_the_node_names_what_needed_it() {
        let (node, _) = remote_node("mdma admin ls", Some("no answer"));
        assert_eq!(
            node.admin().err().unwrap().to_string(),
            "mdma admin ls needs the node: no answer"
        );
    }

    #[test]
    fn labelling_is_idempotent_and_leaves_other_errors_alone() {
        let node = Node::with_target(NodeOptions::default(), Target::LocalIpc);
        let once = node.labelled(NodeError::Connect("down".to_string()));
        assert_eq!(once.to_string(), "mdma needs the node: down");
        assert_eq!(node.labelled(once).to_string(), "mdma needs the node: down");
        for unchanged in [
            NodeError::InvalidEnv("bad".to_string()),
            NodeError::NoRemoteNode,
            NodeError::SourcesDirNotUtf8,
            NodeError::NeedsGateway,
        ] {
            let text = unchanged.to_string();
            assert_eq!(node.labelled(unchanged).to_string(), text);
        }
    }

    #[test]
    fn admin_needs_a_gateway_over_ipc() {
        let local = Node::with_target(NodeOptions::default(), Target::LocalIpc);
        assert_eq!(
            local.admin().err().unwrap().to_string(),
            "admin commands need a gateway; use --node (or MDMA_NODE) to name the node"
        );
    }

    #[test]
    fn two_library_calls_dial_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let address = format!("ipc://{}/library.sock", dir.path().display());
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        server.listen(&address).expect("listen");
        let node = Node::with_target(
            NodeOptions {
                library_socket: Some(address),
                ..NodeOptions::default()
            },
            Target::LocalIpc,
        );

        let (LibraryBackend::Direct(first), LibraryBackend::Direct(second)) =
            (node.library().unwrap(), node.library().unwrap())
        else {
            panic!("local IPC is direct");
        };
        assert!(first.is_same_connection(&second));
    }

    #[test]
    fn a_remote_nodes_services_share_one_gateway_socket() {
        use nng::options::Options as _;
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        let listener = nng::ListenerBuilder::new(&server, "tcp://127.0.0.1:0")
            .expect("listener")
            .start()
            .map_err(|(_, e)| e)
            .expect("listen");
        let port = listener
            .get_opt::<nng::options::transport::tcp::BoundPort>()
            .expect("bound port");
        let (effects, _) = Fake::effects(None);
        let node = Node::with_target(
            NodeOptions::default(),
            Target::Remote(RemoteTarget {
                host: "localhost".to_string(),
                ip: Some(Ipv4Addr::LOCALHOST),
                port,
                source: Source::Cache,
            }),
        )
        .with_effects(effects);

        let (LibraryBackend::Gateway(library), PlaybackBackend::Gateway(playback)) =
            (node.library().unwrap(), node.playback().unwrap())
        else {
            panic!("a remote node is reached through its gateway");
        };
        let admin = node.admin().unwrap();
        assert!(library.is_same_connection(&playback));
        assert!(library.is_same_connection(&admin));
    }

    #[test]
    fn a_failed_connect_is_not_cached() {
        let dir = tempfile::tempdir().expect("temp dir");
        let node = local_node_in(&dir);
        assert!(node.library().is_err());
        assert!(locked(&node.library).is_none());
        // The second call tries again, and fails the same way.
        assert!(node.library().is_err());
    }

    /// A service that accepts requests and never answers them: with the node's own
    /// 5 s request timeout, the first request on a connection to it times out.
    fn silent_service(dir: &tempfile::TempDir, name: &str) -> (nng::Socket, String) {
        let address = format!("ipc://{}/{name}.sock", dir.path().display());
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        server.listen(&address).expect("listen");
        (server, address)
    }

    fn local_node(options: NodeOptions) -> (Node, Fake) {
        let (effects, fake) = Fake::effects(None);
        let node = Node::with_target(options, Target::LocalIpc).with_effects(effects);
        (node, fake)
    }

    #[test]
    fn a_timed_out_library_request_makes_the_next_library_fail_without_dialling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (server, address) = silent_service(&dir, "library");
        let (node, fake) = local_node(NodeOptions {
            library_socket: Some(address),
            ..NodeOptions::default()
        });
        let first = node.library().unwrap();
        assert!(first.ping().expect_err("no answer").is_unreachable());

        // The service goes away: a dial would now fail with its own text, so the
        // text of the timeout shows that nothing dialled. The window runs from when
        // the node first hears of the failure: the next ask.
        drop(server);
        let refused = node.library().err().expect("fails fast").to_string();
        assert_eq!(refused, "mdma needs the node: nng error: Timed out");
        assert!(
            locked(&node.library).is_none(),
            "the dead socket is dropped"
        );
        fake.advance(Duration::from_secs(9));
        let refused = node.library().err().expect("still fails fast").to_string();
        assert_eq!(refused, "mdma needs the node: nng error: Timed out");

        // After the window it connects again, on a new socket.
        fake.advance(Duration::from_secs(1));
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        server.listen(node.library_socket()).expect("listen again");
        let LibraryBackend::Direct(again) = node.library().unwrap() else {
            panic!("local IPC is direct");
        };
        let LibraryBackend::Direct(first) = first else {
            panic!("local IPC is direct");
        };
        assert!(!first.is_same_connection(&again));
        drop(server);
    }

    /// On local IPC the services are separate sockets: a hung library does not take
    /// playback with it.
    #[test]
    fn on_local_ipc_a_library_timeout_leaves_playback_usable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (_library_server, library) = silent_service(&dir, "library");
        let (_playback_server, playback) = silent_service(&dir, "playback");
        let (node, _fake) = local_node(NodeOptions {
            library_socket: Some(library),
            playback_socket: Some(playback),
            ..NodeOptions::default()
        });
        let hung = node.library().unwrap();
        let playing = node.playback().unwrap();
        assert!(hung.ping().expect_err("no answer").is_unreachable());

        assert!(node.library().is_err(), "the library is dropped");
        let still = node.playback().expect("playback was not dropped");
        let (PlaybackBackend::Direct(before), PlaybackBackend::Direct(after)) = (playing, still)
        else {
            panic!("local IPC is direct");
        };
        assert!(
            before.is_same_connection(&after),
            "and it is the same socket"
        );
    }

    #[test]
    fn a_timed_out_playback_request_makes_the_next_playback_fail_without_dialling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (server, address) = silent_service(&dir, "playback");
        let (node, fake) = local_node(NodeOptions {
            playback_socket: Some(address),
            ..NodeOptions::default()
        });
        let first = node.playback().unwrap();
        assert!(first.queue_list().expect_err("no answer").is_unreachable());

        drop(server);
        let refused = node.playback().err().expect("fails fast").to_string();
        assert_eq!(refused, "mdma needs the node: nng error: Timed out");
        fake.advance(Duration::from_secs(9));
        let refused = node.playback().err().expect("still fails fast").to_string();
        assert_eq!(refused, "mdma needs the node: nng error: Timed out");

        fake.advance(Duration::from_secs(1));
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        server.listen(node.playback_socket()).expect("listen again");
        assert!(node.playback().is_ok(), "it dials again after the window");
        drop(server);
    }

    #[test]
    fn a_dead_gateway_socket_is_dropped_for_every_service_and_the_node_is_probed_again() {
        use nng::options::Options as _;
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        let listener = nng::ListenerBuilder::new(&server, "tcp://127.0.0.1:0")
            .expect("listener")
            .start()
            .map_err(|(_, e)| e)
            .expect("listen");
        let port = listener
            .get_opt::<nng::options::transport::tcp::BoundPort>()
            .expect("bound port");
        let (effects, fake) = Fake::effects(None);
        let node = Node::with_target(
            NodeOptions::default(),
            Target::Remote(RemoteTarget {
                host: "localhost".to_string(),
                ip: Some(Ipv4Addr::LOCALHOST),
                port,
                source: Source::Cache,
            }),
        )
        .with_effects(effects);
        let library = node.library().unwrap();
        node.playback().unwrap();
        assert_eq!(fake.probes(), 1);

        assert!(library.ping().expect_err("no answer").is_unreachable());

        // Within the window nothing is handed out and nothing is probed. It runs from
        // when the node first hears of the failure: the next ask.
        assert!(node.library().is_err());
        fake.advance(Duration::from_secs(9));
        assert!(node.library().is_err());
        assert!(node.playback().is_err());
        assert!(node.admin().is_err());
        assert_eq!(fake.probes(), 1);
        assert!(locked(&node.gateway_client).is_none());

        // After it the node is probed again, and used on a new socket.
        fake.advance(Duration::from_secs(1));
        let LibraryBackend::Gateway(again) = node.library().unwrap() else {
            panic!("a remote node is reached through its gateway");
        };
        let LibraryBackend::Gateway(library) = library else {
            panic!("a remote node is reached through its gateway");
        };
        assert_eq!(fake.probes(), 2);
        assert!(!library.is_same_connection(&again));
    }

    #[test]
    fn a_cancelled_pick_is_cancelled_through_the_label() {
        let (effects, fake) = Fake::effects(None);
        *fake.resolve_error.lock().unwrap() = ResolveError::Cancelled;
        let node = Node::needed_by(NeededBy::new("mdma-tui"), NodeOptions::default())
            .with_effects(effects);
        assert!(node.target().err().unwrap().is_cancelled());
        assert!(node.library().err().unwrap().is_cancelled());
        assert!(!NodeError::Connect("down".to_string()).is_cancelled());
        assert!(!NodeError::Resolve(ResolveError::NoNodesFound).is_cancelled());
    }

    #[test]
    fn gateway_uses_resolved_target() {
        // The resolved target's IP and port give the gateway address
        let (node, _) = remote_node("mdma", None);
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
                "mdma needs the node: \
                 Failed to connect to library at ipc://{}/none.sock: Connection refused\n\
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
                "mdma needs the node: \
                 Failed to connect to playback at ipc://{}/none-playback.sock: \
                 Connection refused\n\
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
                "mdma needs the node: \
                 Failed to connect to source 'bandcamp' at ipc://{}/bandcamp.sock: \
                 Connection refused\n\
                 Is mdma-bandcamp running?\n\
                 Hint: 'mdma source list' shows the available sources",
                dir.path().display()
            )
        );
    }

    #[test]
    fn a_dead_node_says_what_needed_it_and_that_it_did_not_answer() {
        // The one test of the real probe: connecting to a port that refuses.
        let (target, _port, _guard) = dead_node();
        let mut node = Node::with_target(NodeOptions::default(), target);
        node.needed_by = NeededBy::new("mdma queue list");
        assert_eq!(
            node.library().err().unwrap().to_string(),
            "mdma queue list needs the node: localhost (127.0.0.1) did not answer in time"
        );
    }

    #[test]
    fn node_is_not_resolved_until_needed() {
        // Nothing resolves a node on construction, only on first use of the target.
        let node = Node::needed_by(NeededBy::new("mdma"), NodeOptions::default());
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
        let labelled = NodeError::Needed {
            needed_by: NeededBy::new("mdma"),
            cause: Box::new(NodeError::NeedsGateway),
        };
        assert!(labelled.source().is_none());
    }
}
