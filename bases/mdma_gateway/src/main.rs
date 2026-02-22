use clap::Parser;
use color_eyre::Result;
use gateway_protocol::{GatewayRequest, GatewayResponse};
use nng::options::Options;
use source_protocol::{SourceRequest, SourceResponse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

    /// Directory containing source service sockets
    #[arg(long, default_value = "/run/mdma/sources")]
    sources_dir: PathBuf,
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
    socket.dial_async(address).map_err(|e| e)?;

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
                let data = serde_json::to_vec(&response).unwrap();
                let _ = frontend.send(nng::Message::from(&data[..]));
                continue;
            }
        };

        let response = match envelope {
            GatewayRequest::Library { request } => {
                let request_bytes = serde_json::to_vec(&request).unwrap();
                match forward_raw(&library_backend, &request_bytes) {
                    Ok(resp_bytes) => match serde_json::from_slice(&resp_bytes) {
                        Ok(resp) => GatewayResponse::Library { response: resp },
                        Err(e) => GatewayResponse::Error {
                            message: format!("library response parse error: {}", e),
                        },
                    },
                    Err(e) => GatewayResponse::Error {
                        message: format!("library service unreachable: {}", e),
                    },
                }
            }

            GatewayRequest::Playback { request } => {
                let request_bytes = serde_json::to_vec(&request).unwrap();
                match forward_raw(&playback_backend, &request_bytes) {
                    Ok(resp_bytes) => match serde_json::from_slice(&resp_bytes) {
                        Ok(resp) => GatewayResponse::Playback { response: resp },
                        Err(e) => GatewayResponse::Error {
                            message: format!("playback response parse error: {}", e),
                        },
                    },
                    Err(e) => GatewayResponse::Error {
                        message: format!("playback service unreachable: {}", e),
                    },
                }
            }

            GatewayRequest::Source { name, request } => {
                match get_or_connect_source(&args.sources_dir, &name, &mut source_cache) {
                    Ok(backend) => {
                        let request_bytes = serde_json::to_vec(&request).unwrap();
                        match forward_raw(backend, &request_bytes) {
                            Ok(resp_bytes) => {
                                match serde_json::from_slice::<SourceResponse>(&resp_bytes) {
                                    Ok(resp) => GatewayResponse::Source {
                                        name,
                                        response: resp,
                                    },
                                    Err(e) => {
                                        // Remove broken connection from cache
                                        source_cache.remove(&name);
                                        GatewayResponse::Error {
                                            message: format!(
                                                "source '{}' response parse error: {}",
                                                name, e
                                            ),
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                // Remove broken connection from cache
                                source_cache.remove(&name);
                                GatewayResponse::Error {
                                    message: format!("source '{}' unreachable: {}", name, e),
                                }
                            }
                        }
                    }
                    Err(e) => GatewayResponse::Error { message: e },
                }
            }

            GatewayRequest::ListSources => {
                // Handle ListSources with optional liveness check
                let mut source_names = list_sources(&args.sources_dir);

                // Try to ping each source to verify it's alive
                source_names.retain(|name| {
                    match get_or_connect_source(&args.sources_dir, name, &mut source_cache) {
                        Ok(backend) => {
                            let ping = SourceRequest::Ping;
                            let ping_bytes = serde_json::to_vec(&ping).unwrap();
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

        let data = serde_json::to_vec(&response).unwrap();
        if let Err((_, e)) = frontend.send(nng::Message::from(&data[..])) {
            tracing::error!(error = %e, "Failed to send response");
        }
    }
}
