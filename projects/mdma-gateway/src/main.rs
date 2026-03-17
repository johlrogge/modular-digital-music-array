use clap::Parser;
use color_eyre::Result;
use gateway_protocol::{GatewayRequest, GatewayResponse, SourceName};
use nng::options::Options;
use serde::{de::DeserializeOwned, Serialize};
use source_protocol::{SourceRequest, SourceResponse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
}

/// Connect a Req0 socket to a backend with reconnect options.
fn connect_backend(address: &str) -> Result<nng::Socket, nng::Error> {
    let socket = nng::Socket::new(nng::Protocol::Req0)?;

    // Set send/recv timeout so we don't hang forever if a backend is down
    socket.set_opt::<nng::options::SendTimeout>(Some(Duration::from_secs(5)))?;
    socket.set_opt::<nng::options::RecvTimeout>(Some(Duration::from_secs(10)))?;

    // Set reconnect options on the socket before dialing
    socket.set_opt::<nng::options::ReconnectMinTime>(Some(Duration::from_millis(100)))?;
    socket.set_opt::<nng::options::ReconnectMaxTime>(Some(Duration::from_secs(5)))?;

    // Nonblocking dial — will reconnect automatically
    socket.dial_async(address)?;

    Ok(socket)
}

/// Forward a serialized request to a backend socket and return the raw response bytes.
fn forward_raw(backend: &nng::Socket, request_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let msg = nng::Message::from(request_bytes);
    backend
        .send(msg)
        .map_err(|(_, e)| format!("send failed: {}", e))?;

    let response_msg = backend.recv().map_err(|e| format!("recv failed: {}", e))?;

    Ok(response_msg.as_slice().to_vec())
}

/// Serialize a request, forward it to a backend, and deserialize the typed response.
///
/// Returns `Ok(response)` on success, or `Err(GatewayResponse::Error { .. })` on any failure
/// so callers can propagate an error envelope without extra boilerplate.
#[allow(clippy::result_large_err)]
fn forward_typed<Req, Resp>(
    backend: &nng::Socket,
    request: &Req,
    service_name: &str,
) -> Result<Resp, GatewayResponse>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let request_bytes = serde_json::to_vec(request)
        .expect("request serialization must not fail for well-formed protocol types");

    let resp_bytes = forward_raw(backend, &request_bytes).map_err(|e| GatewayResponse::Error {
        message: format!("{} service unreachable: {}", service_name, e),
    })?;

    serde_json::from_slice(&resp_bytes).map_err(|e| GatewayResponse::Error {
        message: format!("{} response parse error: {}", service_name, e),
    })
}

/// Get or create a cached connection to a source service.
fn get_or_connect_source<'a>(
    sources_dir: &Path,
    name: &str,
    cache: &'a mut HashMap<String, nng::Socket>,
) -> Result<&'a mut nng::Socket, String> {
    // Validate source name (prevent path traversal)
    if name.contains('/') || name.contains("..") || name.contains('\0') {
        return Err(format!("invalid source name: {}", name));
    }

    if !cache.contains_key(name) {
        let socket_path = sources_dir.join(format!("{}.sock", name));
        if !socket_path.exists() {
            return Err(format!("source '{}' not found", name));
        }

        let address = format!("ipc://{}", socket_path.display());
        let socket = connect_backend(&address)
            .map_err(|e| format!("failed to connect to source '{}': {}", name, e))?;

        cache.insert(name.to_string(), socket);
    }

    Ok(cache.get_mut(name).unwrap())
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

    // Create sources directory if it doesn't exist
    std::fs::create_dir_all(&args.sources_dir)?;

    // Create frontend (Rep0) socket
    let frontend = nng::Socket::new(nng::Protocol::Rep0)?;
    frontend.listen(&args.listen)?;
    tracing::info!(address = %args.listen, "Gateway listening");

    // Connect to core backends
    let library_backend = connect_backend(&args.library_socket)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to library: {}", e))?;
    tracing::info!(address = %args.library_socket, "Connected to library backend");

    let playback_backend = connect_backend(&args.playback_socket)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to playback: {}", e))?;
    tracing::info!(address = %args.playback_socket, "Connected to playback backend");

    let acid_backend = connect_backend(&args.acid_socket)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to acid: {}", e))?;
    tracing::info!(address = %args.acid_socket, "Connected to acid backend");

    // Event bridge: Sub0 (local) -> Pub0 (TCP)
    // A single Pub0 socket re-publishes events from all IPC sources to TCP subscribers.
    let event_pub = Arc::new(nng::Socket::new(nng::Protocol::Pub0)?);
    event_pub.listen(&args.event_listen)?;
    tracing::info!(address = %args.event_listen, "Event publishing on TCP");

    // Bridge playback events (playback/ topic)
    spawn_event_bridge(Arc::clone(&event_pub), args.event_source.clone());

    // Bridge ACID events (acid/ topic)
    spawn_event_bridge(Arc::clone(&event_pub), args.acid_event_source.clone());

    // Source backend cache (connected on demand)
    let mut source_cache: HashMap<String, nng::Socket> = HashMap::new();

    tracing::info!("Gateway ready");

    loop {
        // Receive request from client
        let msg = match frontend.recv() {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "Failed to receive request");
                continue;
            }
        };

        let envelope: GatewayRequest = match serde_json::from_slice(&msg) {
            Ok(e) => e,
            Err(e) => {
                let response = GatewayResponse::Error {
                    message: format!("Invalid request: {}", e),
                };
                let data = serde_json::to_vec(&response)
                    .expect("GatewayResponse::Error serialization must not fail");
                let _ = frontend.send(nng::Message::from(&data[..]));
                continue;
            }
        };

        let response = match envelope {
            GatewayRequest::Library { request } => {
                match forward_typed(&library_backend, &request, "library") {
                    Ok(resp) => GatewayResponse::Library { response: resp },
                    Err(e) => e,
                }
            }

            GatewayRequest::Playback { request } => {
                match forward_typed(&playback_backend, &request, "playback") {
                    Ok(resp) => GatewayResponse::Playback { response: resp },
                    Err(e) => e,
                }
            }

            GatewayRequest::Source { name, request } => {
                match get_or_connect_source(&args.sources_dir, name.as_str(), &mut source_cache) {
                    Ok(backend) => {
                        let svc = format!("source '{}'", name);
                        match forward_typed::<_, SourceResponse>(backend, &request, &svc) {
                            Ok(resp) => GatewayResponse::Source {
                                name,
                                response: resp,
                            },
                            Err(e) => {
                                // Remove broken connection from cache on any forwarding error
                                source_cache.remove(name.as_str());
                                e
                            }
                        }
                    }
                    Err(e) => GatewayResponse::Error { message: e },
                }
            }

            GatewayRequest::Acid { request } => {
                match forward_typed(&acid_backend, &request, "acid") {
                    Ok(resp) => GatewayResponse::Acid { response: resp },
                    Err(e) => e,
                }
            }

            GatewayRequest::ListSources => {
                // Handle ListSources with optional liveness check
                let raw_names = list_sources(&args.sources_dir);
                let mut source_names: Vec<SourceName> =
                    raw_names.into_iter().map(SourceName::new).collect();

                // Try to ping each source to verify it's alive
                source_names.retain(|name| {
                    match get_or_connect_source(&args.sources_dir, name.as_str(), &mut source_cache)
                    {
                        Ok(backend) => {
                            let ping = SourceRequest::Ping;
                            let ping_bytes = serde_json::to_vec(&ping)
                                .expect("SourceRequest::Ping serialization must not fail");
                            forward_raw(backend, &ping_bytes).is_ok()
                        }
                        Err(_) => false,
                    }
                });

                GatewayResponse::Sources {
                    names: source_names,
                }
            }
        };

        let data =
            serde_json::to_vec(&response).expect("GatewayResponse serialization must not fail");
        if let Err((_, e)) = frontend.send(nng::Message::from(&data[..])) {
            tracing::error!(error = %e, "Failed to send response");
        }
    }
}
