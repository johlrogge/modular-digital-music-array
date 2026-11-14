use clap::Parser;
use color_eyre::Result;
use gateway_protocol::{GatewayRequest, GatewayResponse, NodeIdentity, SourceName, IDENTITY_PROTO};
use nng::options::Options;
use serde::{de::DeserializeOwned, Serialize};
// Polylith: explicitly names the `service` base crate selected for this binary.
// In Rust 2018+ the `service::` path prefix already works without this statement,
// but the bare `use` makes the polylith base selection visible at a glance and
// is the project convention for documenting which base backs this project.
#[allow(clippy::single_component_path_imports)]
use service;
use source_protocol::{SourceRequest, SourceResponse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Gateway - Single API gateway for all services"
)]
struct Args {
    /// TCP listen address
    #[arg(long, default_value = "tcp://0.0.0.0:5555")]
    listen: String,

    /// Library service IPC socket
    #[arg(long, default_value = "ipc:///run/mdma/library.sock")]
    library_socket: String,

    /// Playback service IPC socket
    #[arg(long, default_value = "ipc:///run/mdma/playback.sock")]
    playback_socket: String,

    /// ACID service IPC socket
    #[arg(long, default_value = "ipc:///run/mdma/acid.sock")]
    acid_socket: String,

    /// Admin service IPC socket
    #[arg(long, default_value = "ipc:///run/mdma/admin.sock")]
    admin_socket: String,

    /// Directory containing source service sockets
    #[arg(long, default_value = "/run/mdma/sources")]
    sources_dir: PathBuf,

    /// TCP listen address for event publishing (Pub0)
    #[arg(long, default_value = "tcp://0.0.0.0:5556")]
    event_listen: String,

    /// Local event source to subscribe to (Sub0)
    #[arg(long, default_value = "ipc:///run/mdma/events.sock")]
    event_source: String,

    /// ACID event source to subscribe to (Sub0)
    #[arg(long, default_value = "ipc:///run/mdma/acid-events.sock")]
    acid_event_source: String,

    /// Node id reported to clients (the hostname set at provisioning)
    #[arg(long, env = "MDMA_NODE_ID")]
    node_id: Option<String>,

    /// Node name reported to clients
    #[arg(long, env = "MDMA_NODE_NAME")]
    node_name: Option<String>,
}

/// Build the identity answered to `GatewayRequest::Identify`.
/// Missing (or empty) id/name stay `None`: "not configured".
fn build_identity(node_id: Option<&str>, node_name: Option<&str>) -> NodeIdentity {
    let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    NodeIdentity {
        id: node_id.and_then(non_empty),
        name: node_name.and_then(non_empty),
        version: env!("CARGO_PKG_VERSION").to_string(),
        proto: IDENTITY_PROTO,
    }
}

/// How long a request to a backend may wait to be sent.
const BACKEND_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a reply to a client may wait to be sent, so a client that never
/// accepts its reply cannot hold a worker.
const FRONTEND_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `ListSources` waits for a source to answer a ping. Short, so a hung
/// source cannot stall the listing for its full 60s request timeout.
const SOURCE_PING_TIMEOUT: Duration = Duration::from_secs(2);

/// Stack size of a worker thread: they only parse JSON and block on NNG.
const WORKER_STACK_SIZE: usize = 256 * 1024;

/// How long the gateway waits for a reply from the library, playback, ACID and admin.
const SERVICE_RECV_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the gateway waits for a reply from a source; sources do slow work
/// (downloads, scans) and the CLI allows them 60s.
const SOURCE_RECV_TIMEOUT: Duration = Duration::from_secs(60);

/// How many requests the gateway serves at the same time.
///
/// Each slot is one Rep0 context on the frontend plus one thread that blocks while
/// its request is in flight. A slow source call (up to 60s) occupies one slot, so
/// the limit is how many such calls can overlap before further clients queue inside
/// NNG. A node has a handful of clients (TUI, CLI, web console, sync jobs), so 16
/// leaves ample headroom while bounding threads and contexts on the Pi.
const MAX_CONCURRENT_REQUESTS: usize = 16;

/// A backend: a Req0 socket plus how long a reply from it may take.
///
/// Every request uses its own Req0 context on the socket, so concurrent requests to
/// the same backend do not serialise and each reply reaches the request that asked.
#[derive(Clone)]
struct Backend {
    socket: nng::Socket,
    recv_timeout: Duration,
}

/// Connect a Req0 socket to a backend with reconnect options. `recv_timeout` is how
/// long a reply may take.
fn connect_backend(address: &str, recv_timeout: Duration) -> Result<Backend, nng::Error> {
    let socket = nng::Socket::new(nng::Protocol::Req0)?;

    // Set reconnect options on the socket before dialing
    socket.set_opt::<nng::options::ReconnectMinTime>(Some(Duration::from_millis(100)))?;
    socket.set_opt::<nng::options::ReconnectMaxTime>(Some(Duration::from_secs(5)))?;

    // NNG would resend an unanswered request after 60s (the source timeout); slow
    // source operations are not idempotent, so never resend.
    socket.set_opt::<nng::options::protocol::reqrep::ResendTime>(None)?;

    // Nonblocking dial — will reconnect automatically
    socket.dial_async(address)?;

    Ok(Backend {
        socket,
        recv_timeout,
    })
}

/// An NNG context with its own `Aio`, driven synchronously by one thread.
///
/// The `Aio` callback only hands the result to a channel, so the owning thread can
/// block on one operation at a time while the socket serves many such contexts.
/// Timeouts live on the `Aio`, so they apply per operation, not per socket.
struct BlockingContext {
    ctx: nng::Context,
    aio: nng::Aio,
    results: mpsc::Receiver<nng::AioResult>,
}

impl BlockingContext {
    fn new(socket: &nng::Socket) -> Result<Self, nng::Error> {
        let (tx, results) = mpsc::channel();
        let aio = nng::Aio::new(move |_aio, result| {
            let _ = tx.send(result);
        })?;
        let ctx = nng::Context::new(socket)?;
        Ok(Self { ctx, aio, results })
    }

    fn recv(&self, timeout: Option<Duration>) -> Result<nng::Message, nng::Error> {
        self.aio.set_timeout(timeout)?;
        self.ctx.recv(&self.aio)?;
        match self.results.recv() {
            Ok(nng::AioResult::Recv(result)) => result,
            Ok(_) => Err(nng::Error::IncorrectState),
            Err(_) => Err(nng::Error::Closed),
        }
    }

    fn send(&self, msg: nng::Message, timeout: Option<Duration>) -> Result<(), nng::Error> {
        self.aio.set_timeout(timeout)?;
        self.ctx.send(&self.aio, msg).map_err(|(_, e)| e)?;
        match self.results.recv() {
            Ok(nng::AioResult::Send(result)) => result.map_err(|(_, e)| e),
            Ok(_) => Err(nng::Error::IncorrectState),
            Err(_) => Err(nng::Error::Closed),
        }
    }
}

/// Why forwarding a request to a backend failed.
#[derive(Debug)]
enum ForwardError {
    Send(nng::Error),
    Recv(nng::Error),
}

impl ForwardError {
    /// Whether the connection itself looks broken. A reply that merely took too
    /// long (`TimedOut`) says nothing about the connection.
    fn is_connection_failure(&self) -> bool {
        matches!(
            self,
            ForwardError::Send(_) | ForwardError::Recv(nng::Error::Closed)
        )
    }
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardError::Send(e) => write!(f, "send failed: {}", e),
            ForwardError::Recv(e) => write!(f, "recv failed: {}", e),
        }
    }
}

/// Forward a serialized request to a backend and return the raw response bytes,
/// waiting at most `recv_timeout` for the reply.
/// Uses a fresh Req0 context, so other requests to the same backend run alongside.
fn forward_raw(
    backend: &Backend,
    request_bytes: &[u8],
    recv_timeout: Duration,
) -> Result<Vec<u8>, ForwardError> {
    let io = BlockingContext::new(&backend.socket).map_err(ForwardError::Send)?;
    io.send(
        nng::Message::from(request_bytes),
        Some(BACKEND_SEND_TIMEOUT),
    )
    .map_err(ForwardError::Send)?;

    let response_msg = io.recv(Some(recv_timeout)).map_err(ForwardError::Recv)?;

    Ok(response_msg.as_slice().to_vec())
}

/// A failed typed forward: the error envelope for the client, and whether the
/// connection should be dropped from the source cache.
struct ForwardFailure {
    response: GatewayResponse,
    drop_connection: bool,
}

/// Serialize a request, forward it to a backend (waiting `backend.recv_timeout`),
/// and deserialize the typed response.
fn forward_typed<Req, Resp>(
    backend: &Backend,
    request: &Req,
    service_name: &str,
) -> Result<Resp, Box<ForwardFailure>>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let request_bytes = serde_json::to_vec(request)
        .expect("request serialization must not fail for well-formed protocol types");

    let resp_bytes = forward_raw(backend, &request_bytes, backend.recv_timeout).map_err(|e| {
        Box::new(ForwardFailure {
            drop_connection: e.is_connection_failure(),
            response: GatewayResponse::Error {
                message: format!("{} service unreachable: {}", service_name, e),
            },
        })
    })?;

    serde_json::from_slice(&resp_bytes).map_err(|e| {
        Box::new(ForwardFailure {
            drop_connection: false,
            response: GatewayResponse::Error {
                message: format!("{} response parse error: {}", service_name, e),
            },
        })
    })
}

/// Connections to source services, made on demand and shared by all workers.
type SourceCache = Mutex<HashMap<String, Backend>>;

/// Get or create a cached connection to a source service.
fn get_or_connect_source(
    sources_dir: &Path,
    name: &str,
    cache: &SourceCache,
) -> Result<Backend, String> {
    // Validate source name (prevent path traversal)
    if name.contains('/') || name.contains("..") || name.contains('\0') {
        return Err(format!("invalid source name: {}", name));
    }

    // The lock is held only for the lookup/insert, never while talking to a source.
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(backend) = cache.get(name) {
        return Ok(backend.clone());
    }

    let socket_path = sources_dir.join(format!("{}.sock", name));
    if !socket_path.exists() {
        return Err(format!("source '{}' not found", name));
    }

    let address = format!("ipc://{}", socket_path.display());
    let backend = connect_backend(&address, SOURCE_RECV_TIMEOUT)
        .map_err(|e| format!("failed to connect to source '{}': {}", name, e))?;

    cache.insert(name.to_string(), backend.clone());
    Ok(backend)
}

/// Scan the sources directory for available source sockets.
fn list_sources(sources_dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(sources_dir) {
        Ok(entries) => entries,
        Err(_) => return vec![],
    };

    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension()?.to_str()? == "sock" {
                path.file_stem()?.to_str().map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Spawn a background thread that subscribes to an IPC Pub0 source and
/// re-publishes every message on the shared TCP `event_pub` socket.
///
/// All topics are forwarded — the Pub0 wire format already contains the topic
/// prefix so downstream TCP subscribers can filter by topic themselves.
fn spawn_event_bridge(event_pub: Arc<nng::Socket>, source_addr: String) {
    std::thread::spawn(move || {
        let event_sub = match nng::Socket::new(nng::Protocol::Sub0) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, source = %source_addr, "Failed to create Sub0 socket for event bridge");
                return;
            }
        };

        // Subscribe to all topics (empty prefix = wildcard)
        if let Err(e) = event_sub.set_opt::<nng::options::protocol::pubsub::Subscribe>(vec![]) {
            tracing::error!(error = %e, "Failed to set subscription filter");
            return;
        }

        if let Err(e) = event_sub.dial_async(&source_addr) {
            tracing::error!(address = %source_addr, error = %e, "Failed to connect to event source");
            return;
        }

        tracing::info!(address = %source_addr, "Event bridge connected to source");

        loop {
            match event_sub.recv() {
                Ok(msg) => {
                    if let Err((_, e)) = event_pub.send(nng::Message::from(msg.as_slice())) {
                        tracing::warn!(error = %e, "Failed to re-publish event");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, source = %source_addr, "Event bridge recv error, retrying...");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    });
}

fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_gateway=info".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!(
        listen = %args.listen,
        library = %args.library_socket,
        playback = %args.playback_socket,
        sources_dir = %args.sources_dir.display(),
        "Starting MDMA Gateway"
    );

    if args.node_id.is_none() || args.node_name.is_none() {
        tracing::warn!("MDMA_NODE_ID/MDMA_NODE_NAME not set; node identity is unconfigured");
    }
    let identity = build_identity(args.node_id.as_deref(), args.node_name.as_deref());
    tracing::info!(id = ?identity.id, name = ?identity.name, "Node identity");

    // Create sources directory if it doesn't exist
    std::fs::create_dir_all(&args.sources_dir)?;

    // Create frontend (Rep0) and event (Pub0) sockets via the service base.
    let sockets = service::create_sockets(&service::ServiceConfig {
        socket_address: args.listen.clone(),
        event_address: Some(args.event_listen.clone()),
    })?;
    let frontend = sockets.rep_socket;
    let event_pub = Arc::new(sockets.event_socket.expect("event socket always created"));
    tracing::info!(address = %args.listen, "Gateway listening");
    tracing::info!(address = %args.event_listen, "Event publishing on TCP");

    // Connect to core backends
    let library_backend = connect_backend(&args.library_socket, SERVICE_RECV_TIMEOUT)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to library: {}", e))?;
    tracing::info!(address = %args.library_socket, "Connected to library backend");

    let playback_backend = connect_backend(&args.playback_socket, SERVICE_RECV_TIMEOUT)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to playback: {}", e))?;
    tracing::info!(address = %args.playback_socket, "Connected to playback backend");

    let acid_backend = connect_backend(&args.acid_socket, SERVICE_RECV_TIMEOUT)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to acid: {}", e))?;
    tracing::info!(address = %args.acid_socket, "Connected to acid backend");

    let admin_backend = connect_backend(&args.admin_socket, SERVICE_RECV_TIMEOUT)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to admin: {}", e))?;
    tracing::info!(address = %args.admin_socket, "Connected to admin backend");

    // Bridge playback events (playback/ topic)
    spawn_event_bridge(Arc::clone(&event_pub), args.event_source.clone());

    // Bridge ACID events (acid/ topic)
    spawn_event_bridge(Arc::clone(&event_pub), args.acid_event_source.clone());

    let gateway = Arc::new(Gateway {
        identity,
        library: library_backend,
        playback: playback_backend,
        acid: acid_backend,
        admin: admin_backend,
        sources_dir: args.sources_dir.clone(),
        source_cache: Mutex::new(HashMap::new()),
    });

    tracing::info!(workers = MAX_CONCURRENT_REQUESTS, "Gateway ready");

    for worker in serve(&frontend, &gateway, MAX_CONCURRENT_REQUESTS)? {
        let _ = worker.join();
    }
    Ok(())
}

/// Everything needed to answer a request: backends, source discovery and identity.
struct Gateway {
    identity: NodeIdentity,
    library: Backend,
    playback: Backend,
    acid: Backend,
    admin: Backend,
    sources_dir: PathBuf,
    source_cache: SourceCache,
}

impl Gateway {
    /// Drop `backend` from the source cache, unless another worker has already
    /// replaced it with a newer connection.
    fn forget_source(&self, name: &str, backend: &Backend) {
        let mut cache = self.source_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache
            .get(name)
            .is_some_and(|cached| cached.socket == backend.socket)
        {
            cache.remove(name);
        }
    }

    /// Whether the named source answers a ping within `SOURCE_PING_TIMEOUT`.
    fn source_answers_ping(&self, name: &str) -> bool {
        let Ok(backend) = get_or_connect_source(&self.sources_dir, name, &self.source_cache) else {
            return false;
        };
        let ping_bytes = serde_json::to_vec(&SourceRequest::Ping)
            .expect("SourceRequest::Ping serialization must not fail");
        match forward_raw(&backend, &ping_bytes, SOURCE_PING_TIMEOUT) {
            Ok(_) => true,
            Err(e) => {
                if e.is_connection_failure() {
                    self.forget_source(name, &backend);
                }
                false
            }
        }
    }

    /// Answer one request envelope. Blocks while waiting for the backend, so it
    /// is called from a worker that has its own frontend context.
    fn handle(&self, request_bytes: &[u8]) -> GatewayResponse {
        let envelope: GatewayRequest = match serde_json::from_slice(request_bytes) {
            Ok(e) => e,
            Err(e) => return GatewayResponse::invalid_request(e),
        };

        match envelope {
            GatewayRequest::Library { request } => {
                match forward_typed(&self.library, &request, "library") {
                    Ok(resp) => GatewayResponse::Library { response: resp },
                    Err(e) => e.response,
                }
            }

            GatewayRequest::Playback { request } => {
                match forward_typed(&self.playback, &request, "playback") {
                    Ok(resp) => GatewayResponse::Playback { response: resp },
                    Err(e) => e.response,
                }
            }

            GatewayRequest::Source { name, request } => {
                match get_or_connect_source(&self.sources_dir, name.as_str(), &self.source_cache) {
                    Ok(backend) => {
                        let svc = format!("source '{}'", name);
                        match forward_typed::<_, SourceResponse>(&backend, &request, &svc) {
                            Ok(resp) => GatewayResponse::Source {
                                name,
                                response: resp,
                            },
                            Err(e) => {
                                if e.drop_connection {
                                    self.forget_source(name.as_str(), &backend);
                                }
                                e.response
                            }
                        }
                    }
                    Err(e) => GatewayResponse::Error { message: e },
                }
            }

            GatewayRequest::Acid { request } => match forward_typed(&self.acid, &request, "acid") {
                Ok(resp) => GatewayResponse::Acid { response: resp },
                Err(e) => e.response,
            },

            GatewayRequest::Admin { request } => {
                match forward_typed(&self.admin, &request, "admin") {
                    Ok(resp) => GatewayResponse::Admin { response: resp },
                    Err(e) => e.response,
                }
            }

            GatewayRequest::Identify => GatewayResponse::Identity {
                identity: self.identity.clone(),
            },

            GatewayRequest::ListSources => {
                // Handle ListSources with optional liveness check
                let raw_names = list_sources(&self.sources_dir);
                let mut source_names: Vec<SourceName> =
                    raw_names.into_iter().map(SourceName::new).collect();

                // Ping all sources at once, briefly, and keep those that answer.
                let alive: Vec<bool> = std::thread::scope(|scope| {
                    let pings: Vec<_> = source_names
                        .iter()
                        .map(|name| scope.spawn(|| self.source_answers_ping(name.as_str())))
                        .collect();
                    pings
                        .into_iter()
                        .map(|ping| ping.join().unwrap_or(false))
                        .collect()
                });
                let mut alive = alive.into_iter();
                source_names.retain(|_| alive.next().unwrap_or(false));

                GatewayResponse::Sources {
                    names: source_names,
                }
            }
        }
    }
}

/// Start `workers` threads, each serving requests on its own Rep0 context of
/// `frontend`. A slow request occupies one worker; the others keep answering.
fn serve(
    frontend: &nng::Socket,
    gateway: &Arc<Gateway>,
    workers: usize,
) -> Result<Vec<std::thread::JoinHandle<()>>, nng::Error> {
    (0..workers)
        .map(|i| {
            let io = BlockingContext::new(frontend)?;
            let gateway = Arc::clone(gateway);
            std::thread::Builder::new()
                .name(format!("gw-{i}"))
                .stack_size(WORKER_STACK_SIZE)
                .spawn(move || worker_loop(&gateway, &io))
                .map_err(|_| nng::Error::OutOfMemory)
        })
        .collect()
}

fn worker_loop(gateway: &Gateway, io: &BlockingContext) {
    loop {
        let msg = match io.recv(None) {
            Ok(m) => m,
            Err(nng::Error::Closed) => return,
            Err(e) => {
                tracing::error!(error = %e, "Failed to receive request");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        let response = gateway.handle(&msg);
        let data =
            serde_json::to_vec(&response).expect("GatewayResponse serialization must not fail");
        if let Err(e) = io.send(nng::Message::from(&data[..]), Some(FRONTEND_SEND_TIMEOUT)) {
            tracing::error!(error = %e, "Failed to send response");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_never_resends_requests() {
        let backend = connect_backend(
            "ipc:///tmp/mdma-gateway-test-no-backend.sock",
            SOURCE_RECV_TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            backend
                .socket
                .get_opt::<nng::options::protocol::reqrep::ResendTime>()
                .unwrap(),
            None
        );
    }

    #[test]
    fn connection_failures_are_send_errors_and_closed_but_not_reply_timeouts() {
        assert!(ForwardError::Send(nng::Error::TimedOut).is_connection_failure());
        assert!(ForwardError::Recv(nng::Error::Closed).is_connection_failure());
        assert!(!ForwardError::Recv(nng::Error::TimedOut).is_connection_failure());
    }

    // --- concurrency ---

    /// Counts the requests a fake source holds and keeps them until opened.
    struct Gate {
        state: Mutex<GateState>,
        changed: std::sync::Condvar,
    }

    struct GateState {
        in_flight: usize,
        open: bool,
    }

    /// Only a safety net: the tests proceed as soon as their condition holds.
    const SAFETY_TIMEOUT: Duration = Duration::from_secs(5);

    impl Gate {
        fn new(open: bool) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(GateState { in_flight: 0, open }),
                changed: std::sync::Condvar::new(),
            })
        }

        /// Called by a fake source: announce a request, hold it until opened.
        fn hold_request(&self) {
            let mut state = self.state.lock().unwrap();
            state.in_flight += 1;
            self.changed.notify_all();
            let (mut state, _) = self
                .changed
                .wait_timeout_while(state, 30 * SAFETY_TIMEOUT, |s| !s.open)
                .unwrap();
            state.in_flight -= 1;
        }

        /// Wait until `n` requests are being held at the same time.
        fn wait_in_flight(&self, n: usize) {
            let state = self.state.lock().unwrap();
            let (state, _) = self
                .changed
                .wait_timeout_while(state, SAFETY_TIMEOUT, |s| s.in_flight < n)
                .unwrap();
            assert!(
                state.in_flight >= n,
                "only {} requests in flight",
                state.in_flight
            );
        }

        fn open(&self) {
            self.state.lock().unwrap().open = true;
            self.changed.notify_all();
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mdma-gw-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake source answering every request with `Pong` once `gate` lets it,
    /// serving up to `slots` requests at once.
    fn spawn_fake_source(sources_dir: &Path, name: &str, gate: &Arc<Gate>, slots: usize) {
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let slots: Vec<_> = (0..slots)
            .map(|_| BlockingContext::new(&socket).unwrap())
            .collect();
        socket
            .listen(&format!(
                "ipc://{}",
                sources_dir.join(format!("{name}.sock")).display()
            ))
            .unwrap();
        for io in slots {
            // Keep the socket alive as long as any slot thread runs.
            let socket = socket.clone();
            let gate = Arc::clone(gate);
            std::thread::spawn(move || {
                let _socket = socket;
                while io.recv(None).is_ok() {
                    gate.hold_request();
                    let pong = serde_json::to_vec(&SourceResponse::Pong).unwrap();
                    let _ = io.send(nng::Message::from(&pong[..]), None);
                }
            });
        }
    }

    /// Start a gateway on a fresh frontend; returns its address.
    fn start_gateway(dir: &Path) -> String {
        let unused = format!("ipc://{}/none.sock", dir.display());
        let gateway = Arc::new(Gateway {
            identity: build_identity(Some("test"), None),
            library: connect_backend(&unused, SERVICE_RECV_TIMEOUT).unwrap(),
            playback: connect_backend(&unused, SERVICE_RECV_TIMEOUT).unwrap(),
            acid: connect_backend(&unused, SERVICE_RECV_TIMEOUT).unwrap(),
            admin: connect_backend(&unused, SERVICE_RECV_TIMEOUT).unwrap(),
            sources_dir: dir.to_path_buf(),
            source_cache: Mutex::new(HashMap::new()),
        });
        let frontend = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let workers = serve(&frontend, &gateway, MAX_CONCURRENT_REQUESTS).unwrap();
        // Next to the sources dir, not in it, or it would be discovered as a source.
        let address = format!("ipc://{}.gateway.sock", dir.display());
        frontend.listen(&address).unwrap();
        std::thread::spawn(move || {
            let _frontend = frontend;
            for w in workers {
                let _ = w.join();
            }
        });
        address
    }

    /// Send `request` through the gateway as a separate client.
    fn ask(address: &str, request: &GatewayRequest) -> GatewayResponse {
        let client = nng::Socket::new(nng::Protocol::Req0).unwrap();
        client
            .set_opt::<nng::options::RecvTimeout>(Some(30 * SAFETY_TIMEOUT))
            .unwrap();
        client.dial(address).unwrap();
        client
            .send(nng::Message::from(
                &serde_json::to_vec(request).unwrap()[..],
            ))
            .map_err(|(_, e)| e)
            .unwrap();
        serde_json::from_slice(&client.recv().unwrap()).unwrap()
    }

    /// Ping source `name` through the gateway; returns the name the reply is for.
    fn ping_source(address: &str, name: &str) -> String {
        let request = GatewayRequest::Source {
            name: SourceName::new(name.to_string()),
            request: SourceRequest::Ping,
        };
        match ask(address, &request) {
            GatewayResponse::Source {
                name,
                response: SourceResponse::Pong,
            } => name.as_str().to_string(),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn slow_source_does_not_delay_a_fast_one() {
        let dir = temp_dir("slow-fast");
        let slow_gate = Gate::new(false);
        spawn_fake_source(&dir, "slow", &slow_gate, 1);
        spawn_fake_source(&dir, "fast", &Gate::new(true), 1);
        let address = start_gateway(&dir);

        let slow_address = address.clone();
        let slow = std::thread::spawn(move || ping_source(&slow_address, "slow"));
        slow_gate.wait_in_flight(1);

        // The slow request is held at its source, yet the fast one is answered.
        assert_eq!(ping_source(&address, "fast"), "fast");
        assert!(!slow.is_finished(), "slow request finished before release");

        slow_gate.open();
        assert_eq!(slow.join().unwrap(), "slow");
    }

    #[test]
    fn concurrent_requests_to_one_backend_run_in_parallel_and_reach_their_requesters() {
        let dir = temp_dir("parallel");
        let gate = Gate::new(false);
        spawn_fake_source(&dir, "a", &gate, 8);
        spawn_fake_source(&dir, "b", &gate, 8);
        let address = start_gateway(&dir);

        let clients: Vec<_> = ["a", "b", "a", "b", "a", "b"]
            .into_iter()
            .map(|name| {
                let address = address.clone();
                std::thread::spawn(move || (name, ping_source(&address, name)))
            })
            .collect();
        // All six are held at the sources at the same time: none waits for another.
        gate.wait_in_flight(6);
        gate.open();
        for client in clients {
            let (asked, answered) = client.join().unwrap();
            assert_eq!(asked, answered, "reply went to the wrong requester");
        }
    }

    #[test]
    fn list_sources_omits_a_hung_source_after_the_short_ping_timeout() {
        let dir = temp_dir("list");
        let hung_gate = Gate::new(false);
        spawn_fake_source(&dir, "hung", &hung_gate, 1);
        spawn_fake_source(&dir, "live", &Gate::new(true), 1);
        let address = start_gateway(&dir);

        let started = std::time::Instant::now();
        let reply = ask(&address, &GatewayRequest::ListSources);
        hung_gate.open();

        match reply {
            GatewayResponse::Sources { names } => {
                let names: Vec<_> = names.iter().map(|n| n.as_str().to_string()).collect();
                assert_eq!(names, ["live"]);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        // Waiting out the 60s source timeout instead of the ping timeout would blow this.
        assert!(started.elapsed() < SOURCE_RECV_TIMEOUT / 2);
    }

    #[test]
    fn identity_uses_given_id_and_name() {
        let id = build_identity(Some("mdma-stage"), Some("Stage Pi"));
        assert_eq!(id.id.as_deref(), Some("mdma-stage"));
        assert_eq!(id.name.as_deref(), Some("Stage Pi"));
        assert_eq!(id.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(id.proto, IDENTITY_PROTO);
    }

    #[test]
    fn identity_is_none_when_not_configured() {
        let id = build_identity(None, None);
        assert_eq!(id.id, None);
        assert_eq!(id.name, None);
        let id = build_identity(Some(""), Some(""));
        assert_eq!(id.id, None);
        assert_eq!(id.name, None);
    }

    // Single test touching the process environment, to avoid races between tests.
    #[test]
    fn args_parse_node_flags_and_env() {
        std::env::remove_var("MDMA_NODE_ID");
        std::env::remove_var("MDMA_NODE_NAME");
        let args = Args::parse_from(["mdma-gateway", "--node-id", "a", "--node-name", "b"]);
        assert_eq!(args.node_id.as_deref(), Some("a"));
        assert_eq!(args.node_name.as_deref(), Some("b"));
        let args = Args::parse_from(["mdma-gateway"]);
        assert!(args.node_id.is_none() && args.node_name.is_none());

        std::env::set_var("MDMA_NODE_ID", "mdma-stage");
        std::env::set_var("MDMA_NODE_NAME", "Stage");
        let args = Args::parse_from(["mdma-gateway"]);
        std::env::remove_var("MDMA_NODE_ID");
        std::env::remove_var("MDMA_NODE_NAME");
        assert_eq!(args.node_id.as_deref(), Some("mdma-stage"));
        assert_eq!(args.node_name.as_deref(), Some("Stage"));
    }
}
