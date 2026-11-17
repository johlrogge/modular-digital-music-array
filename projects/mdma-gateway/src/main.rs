use acid_protocol::{AcidRequest, AcidResponse};
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

/// How long a request to a backend may wait to be sent: that is, to find a
/// connection. Shorter than the clients' 5s request timeout, so the gateway can
/// replace a dead socket and retry once before a client gives up.
const BACKEND_SEND_TIMEOUT: Duration = Duration::from_secs(1);

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

/// A backend: a Req0 socket dialled to a service.
///
/// Every request uses its own Req0 context on the socket, so concurrent requests to
/// the same backend do not serialise and each reply reaches the request that asked.
#[derive(Clone)]
struct Backend {
    socket: nng::Socket,
    /// Lets a test kill the dialer, the state a restarting listener can leave behind.
    #[cfg(test)]
    dialer: nng::Dialer,
}

/// Connect a Req0 socket to a backend with reconnect options.
fn connect_backend(address: &str) -> Result<Backend, nng::Error> {
    let socket = nng::Socket::new(nng::Protocol::Req0)?;

    // Set reconnect options on the socket before dialing
    socket.set_opt::<nng::options::ReconnectMinTime>(Some(Duration::from_millis(100)))?;
    socket.set_opt::<nng::options::ReconnectMaxTime>(Some(Duration::from_secs(5)))?;

    // NNG would resend an unanswered request after 60s (the source timeout); slow
    // source operations are not idempotent, so never resend.
    socket.set_opt::<nng::options::protocol::reqrep::ResendTime>(None)?;

    // Nonblocking dial. NNG's dialer is not trusted to reconnect for good (it stops
    // for good after a connect that ends in `Closed`), so `BackendSlot` replaces the
    // whole socket on a connection failure.
    let _dialer = nng::Dialer::new(&socket, address, true)?;

    Ok(Backend {
        socket,
        #[cfg(test)]
        dialer: _dialer,
    })
}

/// The connection to one service, made on first use (`get`; `main` warms the core
/// slots at startup) and replaced after a connection failure, with the timeouts
/// that apply to it.
///
/// The lock is held only to clone or swap the socket handle, never during I/O, so
/// requests to one backend still run alongside each other.
struct BackendSlot {
    address: String,
    send_timeout: Duration,
    recv_timeout: Duration,
    current: Mutex<Option<Backend>>,
}

impl BackendSlot {
    fn new(address: String, send_timeout: Duration, recv_timeout: Duration) -> Self {
        Self {
            address,
            send_timeout,
            recv_timeout,
            current: Mutex::new(None),
        }
    }

    /// A slot for a core service, with the standard timeouts.
    fn service(address: &str) -> Self {
        Self::new(
            address.to_string(),
            BACKEND_SEND_TIMEOUT,
            SERVICE_RECV_TIMEOUT,
        )
    }

    /// A slot for a source, which may take long to answer.
    fn source(address: String) -> Self {
        Self::new(address, BACKEND_SEND_TIMEOUT, SOURCE_RECV_TIMEOUT)
    }

    /// The current connection, made now if there is none.
    fn get(&self) -> Result<Backend, nng::Error> {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(backend) = current.as_ref() {
            return Ok(backend.clone());
        }
        let backend = connect_backend(&self.address)?;
        *current = Some(backend.clone());
        Ok(backend)
    }

    /// Drop `backend`, unless another worker has already replaced it with a newer
    /// connection.
    fn forget(&self, backend: &Backend) {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if current
            .as_ref()
            .is_some_and(|held| held.socket == backend.socket)
        {
            *current = None;
        }
    }

    /// One attempt: forward on the current connection, forgetting it if the
    /// connection looks broken.
    fn attempt(
        &self,
        request_bytes: &[u8],
        recv_timeout: Duration,
    ) -> Result<Vec<u8>, ForwardError> {
        let backend = self.get().map_err(ForwardError::Send)?;
        let result = forward_raw(&backend, request_bytes, self.send_timeout, recv_timeout);
        if result
            .as_ref()
            .is_err_and(ForwardError::is_connection_failure)
        {
            self.forget(&backend);
        }
        result
    }

    /// Forward a serialized request and return the raw response bytes, waiting at
    /// most `recv_timeout` for the reply.
    ///
    /// A `Send` failure is safe to resend once on a fresh socket: in nng 1.4 req0 a
    /// send completes only when the message is assigned to a pipe
    /// (`req0_run_send_queue`), and a send that fails was withdrawn from the queue
    /// (`req0_ctx_cancel_send`), so the service never saw it. A `Recv` failure is
    /// never retried: the service may have acted on the request.
    fn exchange_within(
        &self,
        request_bytes: &[u8],
        recv_timeout: Duration,
    ) -> Result<Vec<u8>, ForwardError> {
        match self.attempt(request_bytes, recv_timeout) {
            Err(ForwardError::Send(_)) => self.attempt(request_bytes, recv_timeout),
            result => result,
        }
    }

    /// [`exchange_within`](Self::exchange_within) the slot's own reply timeout.
    fn exchange(&self, request_bytes: &[u8]) -> Result<Vec<u8>, ForwardError> {
        self.exchange_within(request_bytes, self.recv_timeout)
    }

    /// Kill the dialer of the current connection (the dead-dialer state).
    #[cfg(test)]
    fn close_dialer(&self) {
        self.get().unwrap().dialer.close();
    }
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
            ForwardError::Send(_)
                | ForwardError::Recv(nng::Error::Closed | nng::Error::ConnectionReset)
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
/// waiting at most `send_timeout` to send and `recv_timeout` for the reply.
/// Uses a fresh Req0 context, so other requests to the same backend run alongside.
fn forward_raw(
    backend: &Backend,
    request_bytes: &[u8],
    send_timeout: Duration,
    recv_timeout: Duration,
) -> Result<Vec<u8>, ForwardError> {
    let io = BlockingContext::new(&backend.socket).map_err(ForwardError::Send)?;
    io.send(nng::Message::from(request_bytes), Some(send_timeout))
        .map_err(ForwardError::Send)?;

    let response_msg = io.recv(Some(recv_timeout)).map_err(ForwardError::Recv)?;

    Ok(response_msg.as_slice().to_vec())
}

/// Serialize a request, forward it through `slot` (waiting its reply timeout),
/// and wrap the typed response with `wrap`. A failure becomes the error envelope
/// for the client. The one forwarding path for every backend.
fn forward_via<Req, Resp>(
    slot: &BackendSlot,
    request: &Req,
    service_name: &str,
    wrap: impl FnOnce(Resp) -> GatewayResponse,
) -> GatewayResponse
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let request_bytes = serde_json::to_vec(request)
        .expect("request serialization must not fail for well-formed protocol types");

    let resp_bytes = match slot.exchange(&request_bytes) {
        Ok(bytes) => bytes,
        Err(e) => {
            return GatewayResponse::Error {
                message: format!("{} service unreachable: {}", service_name, e),
            }
        }
    };

    match serde_json::from_slice(&resp_bytes) {
        Ok(response) => wrap(response),
        Err(e) => GatewayResponse::Error {
            message: format!("{} response parse error: {}", service_name, e),
        },
    }
}

/// Connections to source services, made on demand and shared by all workers.
type SourceCache = Mutex<HashMap<String, Arc<BackendSlot>>>;

/// Get or create the connection slot of a source service.
fn get_or_connect_source(
    sources_dir: &Path,
    name: &str,
    cache: &SourceCache,
) -> Result<Arc<BackendSlot>, String> {
    // Validate source name (prevent path traversal)
    if name.contains('/') || name.contains("..") || name.contains('\0') {
        return Err(format!("invalid source name: {}", name));
    }

    // The lock is held only for the lookup/insert, never while talking to a source.
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());

    // A source whose socket file is gone has stopped: evict its slot.
    let socket_path = sources_dir.join(format!("{}.sock", name));
    if !socket_path.exists() {
        cache.remove(name);
        return Err(format!("source '{}' not found", name));
    }

    if let Some(slot) = cache.get(name) {
        return Ok(Arc::clone(slot));
    }

    let slot = Arc::new(BackendSlot::source(format!(
        "ipc://{}",
        socket_path.display()
    )));
    cache.insert(name.to_string(), Arc::clone(&slot));
    Ok(slot)
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

/// A core service's slot, connected now so a misconfigured address fails at startup.
fn core_slot(name: &str, address: &str) -> Result<BackendSlot> {
    let slot = BackendSlot::service(address);
    slot.get()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to {}: {}", name, e))?;
    tracing::info!(address = %address, "Connected to {} backend", name);
    Ok(slot)
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

    // Bridge playback events (playback/ topic)
    spawn_event_bridge(Arc::clone(&event_pub), args.event_source.clone());

    // Bridge ACID events (acid/ topic)
    spawn_event_bridge(Arc::clone(&event_pub), args.acid_event_source.clone());

    let gateway = Arc::new(Gateway {
        identity,
        library: core_slot("library", &args.library_socket)?,
        playback: core_slot("playback", &args.playback_socket)?,
        acid: core_slot("acid", &args.acid_socket)?,
        admin: core_slot("admin", &args.admin_socket)?,
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
    library: BackendSlot,
    playback: BackendSlot,
    acid: BackendSlot,
    admin: BackendSlot,
    sources_dir: PathBuf,
    source_cache: SourceCache,
}

impl Gateway {
    /// Whether the named source answers a ping within `SOURCE_PING_TIMEOUT`.
    fn source_answers_ping(&self, name: &str) -> bool {
        let Ok(slot) = get_or_connect_source(&self.sources_dir, name, &self.source_cache) else {
            return false;
        };
        let ping_bytes = serde_json::to_vec(&SourceRequest::Ping)
            .expect("SourceRequest::Ping serialization must not fail");
        slot.exchange_within(&ping_bytes, SOURCE_PING_TIMEOUT)
            .is_ok()
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
                forward_via(&self.library, &request, "library", |response| {
                    GatewayResponse::Library { response }
                })
            }

            GatewayRequest::Playback { request } => {
                forward_via(&self.playback, &request, "playback", |response| {
                    GatewayResponse::Playback { response }
                })
            }

            GatewayRequest::Source { name, request } => {
                match get_or_connect_source(&self.sources_dir, name.as_str(), &self.source_cache) {
                    Ok(slot) => {
                        let svc = format!("source '{}'", name);
                        forward_via::<_, SourceResponse>(&slot, &request, &svc, |response| {
                            GatewayResponse::Source { name, response }
                        })
                    }
                    Err(e) => GatewayResponse::Error { message: e },
                }
            }

            GatewayRequest::Acid { request } => guard_acid(&request, |req| {
                forward_via(&self.acid, req, "acid", |response| GatewayResponse::Acid {
                    response,
                })
            }),

            GatewayRequest::Admin { request } => {
                forward_via(&self.admin, &request, "admin", |response| {
                    GatewayResponse::Admin { response }
                })
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

/// Message returned when a mutating ACID request arrives over the gateway.
const ACID_WRITE_REJECTED: &str =
    "acid writes are not accepted over the gateway; write via the library service";

/// Whether an ACID request may be forwarded from the network.
///
/// Exhaustive on purpose: a new `AcidRequest` variant must be classified here.
fn acid_allowed(request: &AcidRequest) -> bool {
    match request {
        AcidRequest::Ping => true,
        AcidRequest::ReadStream { .. } => true,
        AcidRequest::ReadEntity { .. } => true,
        AcidRequest::WriteFacts { .. } => false,
        AcidRequest::RetractFacts { .. } => false,
    }
}

/// Variant name only, so rejections can be logged without the payload.
fn acid_variant_name(request: &AcidRequest) -> &'static str {
    match request {
        AcidRequest::Ping => "Ping",
        AcidRequest::WriteFacts { .. } => "WriteFacts",
        AcidRequest::ReadStream { .. } => "ReadStream",
        AcidRequest::RetractFacts { .. } => "RetractFacts",
        AcidRequest::ReadEntity { .. } => "ReadEntity",
    }
}

/// Forward allowed ACID requests via `forward`; reject everything else
/// without calling it.
fn guard_acid(
    request: &AcidRequest,
    forward: impl FnOnce(&AcidRequest) -> GatewayResponse,
) -> GatewayResponse {
    if acid_allowed(request) {
        forward(request)
    } else {
        tracing::warn!(
            request = acid_variant_name(request),
            "Rejected acid request over gateway"
        );
        GatewayResponse::Acid {
            response: AcidResponse::Error {
                message: ACID_WRITE_REJECTED.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_protocol::FactEntry;

    fn fact() -> FactEntry {
        FactEntry {
            value_json: "1".to_string(),
            source_json: "{}".to_string(),
        }
    }

    fn forwarded_marker() -> GatewayResponse {
        GatewayResponse::Acid {
            response: AcidResponse::Pong,
        }
    }

    #[test]
    fn read_variants_are_forwarded() {
        let reads = [
            AcidRequest::Ping,
            AcidRequest::ReadStream {
                cursor: None,
                limit: 10,
                expect: None,
            },
            AcidRequest::ReadEntity {
                entity: "e".to_string(),
            },
        ];
        for req in &reads {
            let mut called = false;
            let resp = guard_acid(req, |_| {
                called = true;
                forwarded_marker()
            });
            assert!(called, "{} should be forwarded", acid_variant_name(req));
            assert!(matches!(
                resp,
                GatewayResponse::Acid {
                    response: AcidResponse::Pong
                }
            ));
        }
    }

    #[test]
    fn write_variants_are_rejected_and_never_reach_backend() {
        let writes = [
            AcidRequest::WriteFacts {
                entity: "e".to_string(),
                facts: vec![fact()],
            },
            AcidRequest::RetractFacts {
                entity: "e".to_string(),
                facts: vec![fact()],
            },
        ];
        for req in &writes {
            let mut called = false;
            let resp = guard_acid(req, |_| {
                called = true;
                forwarded_marker()
            });
            assert!(!called, "{} must not reach backend", acid_variant_name(req));
            match resp {
                GatewayResponse::Acid {
                    response: AcidResponse::Error { message },
                } => assert_eq!(message, ACID_WRITE_REJECTED),
                other => panic!("expected acid error, got {other:?}"),
            }
        }
    }

    #[test]
    fn backend_never_resends_requests() {
        let backend = connect_backend("ipc:///tmp/mdma-gateway-test-no-backend.sock").unwrap();
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
        assert!(ForwardError::Recv(nng::Error::ConnectionReset).is_connection_failure());
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
        start_gateway_with_playback(dir, &unused)
    }

    /// Like [`start_gateway`], with playback at `playback_address`.
    fn start_gateway_with_playback(dir: &Path, playback_address: &str) -> String {
        let playback = BackendSlot::service(playback_address);
        start_gateway_with(dir, playback).0
    }

    /// Like [`start_gateway`], with `playback` as the playback backend; also
    /// returns the gateway, so a test can reach its slots.
    fn start_gateway_with(dir: &Path, playback: BackendSlot) -> (String, Arc<Gateway>) {
        let unused = format!("ipc://{}/none.sock", dir.display());
        let gateway = Arc::new(Gateway {
            identity: build_identity(Some("test"), None),
            library: BackendSlot::service(&unused),
            playback,
            acid: BackendSlot::service(&unused),
            admin: BackendSlot::service(&unused),
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
        (address, gateway)
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

    /// A running fake playback service; `stop` closes it, like a restarting service.
    struct FakePlayback {
        address: String,
        commands: std::sync::mpsc::Receiver<media_protocol::Command>,
        socket: nng::Socket,
        thread: std::thread::JoinHandle<()>,
    }

    impl FakePlayback {
        /// Close the socket (so the service's recv returns `Closed`) and wait for it.
        fn stop(self) {
            self.socket.close();
            self.thread.join().unwrap();
        }
    }

    /// A playback service that records the commands it parses and answers
    /// `GetTransport` with a fixed status and everything else with `Ok`.
    fn spawn_fake_playback(dir: &Path) -> FakePlayback {
        use media_protocol::{
            Command, PositionMs, Response, ResponseData, Transport, TransportStatus,
        };
        let address = format!("ipc://{}/playback.sock", dir.display());
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        socket.listen(&address).unwrap();
        let (seen, commands) = std::sync::mpsc::channel();
        let served = socket.clone();
        let thread = std::thread::spawn(move || {
            while let Ok(msg) = served.recv() {
                let command: Command = serde_json::from_slice(&msg).unwrap();
                let response = match &command {
                    Command::GetTransport { .. } | Command::Seek { .. } => Response::Ok {
                        data: Some(ResponseData::Transport(TransportStatus::Loaded {
                            transport: Transport::Playing,
                            now: media_protocol::ContentHash::new("abc"),
                            position_ms: Some(PositionMs::new(90_000)),
                            duration_ms: Some(PositionMs::new(240_000)),
                            preview: None,
                        })),
                    },
                    _ => Response::Ok { data: None },
                };
                seen.send(command).unwrap();
                let _ = served.send(serde_json::to_vec(&response).unwrap().as_slice());
            }
        });
        FakePlayback {
            address,
            commands,
            socket,
            thread,
        }
    }

    /// A playback request any fake answers with `Ok`.
    fn ask_playback(address: &str) -> GatewayResponse {
        ask(
            address,
            &GatewayRequest::Playback {
                request: media_protocol::Command::GetTransport {
                    deck: media_protocol::Deck::A,
                },
            },
        )
    }

    /// A playback slot with short timeouts, so a test does not wait out the real ones.
    fn quick_playback_slot(address: &str) -> BackendSlot {
        BackendSlot::new(
            address.to_string(),
            Duration::from_millis(300),
            Duration::from_secs(2),
        )
    }

    fn assert_playback_ok(reply: GatewayResponse) {
        assert!(
            matches!(
                reply,
                GatewayResponse::Playback {
                    response: media_protocol::Response::Ok { .. }
                }
            ),
            "{reply:?}"
        );
    }

    /// A playback that reads one request, reports it and closes without replying.
    fn spawn_fake_playback_dropping_request(
        dir: &Path,
    ) -> (String, std::sync::mpsc::Receiver<media_protocol::Command>) {
        let address = format!("ipc://{}/playback.sock", dir.display());
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        socket.listen(&address).unwrap();
        let (seen, commands) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let msg = socket.recv().unwrap();
            seen.send(serde_json::from_slice(&msg).unwrap()).unwrap();
            socket.close();
        });
        (address, commands)
    }

    /// Safety invariant: a request that reached the service is never resent, so a
    /// failed receive is reported and the service sees the command exactly once.
    #[test]
    fn a_request_the_service_took_is_not_retried_after_a_recv_failure() {
        let dir = temp_dir("no-retry");
        let (address, commands) = spawn_fake_playback_dropping_request(&dir);
        let slot = BackendSlot::new(
            address,
            Duration::from_millis(300),
            Duration::from_millis(500),
        );
        let (gateway_address, _gateway) = start_gateway_with(&dir, slot);

        match ask_playback(&gateway_address) {
            GatewayResponse::Error { message } => assert!(
                message.starts_with("playback service unreachable: recv failed:"),
                "{message}"
            ),
            other => panic!("unexpected reply: {other:?}"),
        }
        commands.recv().unwrap();
        assert!(commands.try_recv().is_err(), "the request was resent");
    }

    #[test]
    fn forget_drops_only_the_connection_it_was_given() {
        let slot = BackendSlot::service("ipc:///tmp/mdma-gateway-test-no-backend.sock");
        let a = slot.get().unwrap();
        slot.forget(&a);
        let b = slot.get().unwrap();
        assert!(a.socket != b.socket);

        slot.forget(&a);
        assert!(
            slot.get().unwrap().socket == b.socket,
            "B was dropped by A's forget"
        );
    }

    /// A source whose socket file is gone is no longer found, and its slot is evicted.
    #[test]
    fn a_source_whose_socket_file_is_gone_is_evicted() {
        let dir = temp_dir("evict");
        std::fs::write(dir.join("gone.sock"), b"").unwrap();
        let cache: SourceCache = Mutex::new(HashMap::new());
        get_or_connect_source(&dir, "gone", &cache).unwrap();
        assert!(cache.lock().unwrap().contains_key("gone"));

        std::fs::remove_file(dir.join("gone.sock")).unwrap();
        let err = get_or_connect_source(&dir, "gone", &cache).err().unwrap();
        assert_eq!(err, "source 'gone' not found");
        assert!(cache.lock().unwrap().is_empty());
    }

    /// Regression guard for a clean restart between requests. It is not the proof
    /// for #111: the dead-dialer test below is.
    #[test]
    fn playback_restart_between_requests_still_answers() {
        let dir = temp_dir("restart");
        let first = spawn_fake_playback(&dir);
        let (address, _gateway) = start_gateway_with(&dir, quick_playback_slot(&first.address));
        assert_playback_ok(ask_playback(&address));

        first.stop();
        let _second = spawn_fake_playback(&dir);
        assert_playback_ok(ask_playback(&address));
    }

    /// #111: NNG's dialer can die for good. The next request must replace the
    /// socket and retry once.
    #[test]
    fn a_dead_dialer_is_replaced_and_the_request_retried() {
        let dir = temp_dir("dead-dialer");
        let fake = spawn_fake_playback(&dir);
        let (address, gateway) = start_gateway_with(&dir, quick_playback_slot(&fake.address));
        assert_playback_ok(ask_playback(&address));

        gateway.playback.close_dialer();
        assert_playback_ok(ask_playback(&address));
    }

    /// ADR-017: the gateway parses and re-serialises `Command`, so a variant
    /// it does not know (or knows differently) is dropped on the way.
    #[test]
    fn seek_and_get_transport_pass_through_the_gateway() {
        use media_protocol::{
            Command, Deck, PositionMs, Response, ResponseData, Transport, TransportStatus,
        };
        let dir = temp_dir("transport");
        let FakePlayback {
            address: playback,
            commands,
            ..
        } = spawn_fake_playback(&dir);
        let address = start_gateway_with_playback(&dir, &playback);

        let seek = ask(
            &address,
            &GatewayRequest::Playback {
                request: Command::Seek {
                    deck: Deck::A,
                    position_ms: PositionMs::new(90_000),
                },
            },
        );
        assert!(
            matches!(
                seek,
                GatewayResponse::Playback {
                    response: Response::Ok {
                        data: Some(ResponseData::Transport(TransportStatus::Loaded { .. }))
                    }
                }
            ),
            "{seek:?}"
        );
        assert!(matches!(
            commands.recv().unwrap(),
            Command::Seek {
                deck: Deck::A,
                position_ms
            } if position_ms == PositionMs::new(90_000)
        ));

        let transport = ask(
            &address,
            &GatewayRequest::Playback {
                request: Command::GetTransport { deck: Deck::A },
            },
        );
        let GatewayResponse::Playback {
            response:
                Response::Ok {
                    data: Some(ResponseData::Transport(status)),
                },
        } = transport
        else {
            panic!("unexpected reply: {transport:?}");
        };
        assert!(matches!(
            status,
            TransportStatus::Loaded {
                transport: Transport::Playing,
                position_ms: Some(position),
                duration_ms: Some(duration),
                ..
            } if position == PositionMs::new(90_000) && duration == PositionMs::new(240_000)
        ));
        assert!(matches!(
            commands.recv().unwrap(),
            Command::GetTransport { deck: Deck::A }
        ));
    }

    /// ADR-031: `Preview` and `EndPreview` are new variants, so an old
    /// gateway would drop them as unknown.
    #[test]
    fn preview_and_end_preview_pass_through_the_gateway() {
        use media_protocol::{Command, ContentHash, Deck, Response};
        let dir = temp_dir("preview");
        let FakePlayback {
            address: playback,
            commands,
            ..
        } = spawn_fake_playback(&dir);
        let address = start_gateway_with_playback(&dir, &playback);
        let hash = ContentHash::new(format!("sha256:{}", "b".repeat(64)));

        let preview = ask(
            &address,
            &GatewayRequest::Playback {
                request: Command::Preview {
                    deck: Deck::A,
                    hash: hash.clone(),
                },
            },
        );
        assert!(
            matches!(
                preview,
                GatewayResponse::Playback {
                    response: Response::Ok { .. }
                }
            ),
            "{preview:?}"
        );
        assert!(matches!(
            commands.recv().unwrap(),
            Command::Preview { deck: Deck::A, hash: h } if h == hash
        ));

        let end = ask(
            &address,
            &GatewayRequest::Playback {
                request: Command::EndPreview { deck: Deck::A },
            },
        );
        assert!(
            matches!(
                end,
                GatewayResponse::Playback {
                    response: Response::Ok { .. }
                }
            ),
            "{end:?}"
        );
        assert!(matches!(
            commands.recv().unwrap(),
            Command::EndPreview { deck: Deck::A }
        ));
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
