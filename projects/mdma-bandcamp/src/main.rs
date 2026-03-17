use clap::Parser;
use color_eyre::Result;

mod cache;
mod ipc;
mod service;

use ::service::{ServiceConfig, ServiceSockets};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Bandcamp - Bandcamp download service with NNG IPC"
)]
struct Args {
    /// Path to cookies file
    #[arg(long, default_value = "/etc/mdma/bandcamp-cookies.json")]
    cookies: std::path::PathBuf,

    /// Path to downloads staging directory
    #[arg(long, default_value = "/music/downloads")]
    downloads_dir: std::path::PathBuf,

    /// Path to inbox directory (completed downloads)
    #[arg(long, default_value = "/music/inbox")]
    inbox_dir: std::path::PathBuf,

    /// Path to download cache file
    #[arg(long, default_value = "/var/lib/mdma/bandcamp.cache")]
    cache: std::path::PathBuf,

    /// NNG IPC socket path
    #[arg(long, default_value = "ipc:///run/mdma/sources/bandcamp.sock")]
    socket: String,

    /// Library service socket address for auto-ingest
    #[arg(long, default_value = "ipc:///run/mdma/library.sock")]
    library_socket: String,

    /// Audio format to download
    #[arg(long, default_value = "flac")]
    format: String,

    /// Bandcamp username (used for collection sync)
    #[arg(long, env = "MDMA_BANDCAMP_USERNAME")]
    username: Option<String>,
}

fn parse_format(s: &str) -> bandcamp_api::AudioFormat {
    match s.to_lowercase().as_str() {
        "flac" => bandcamp_api::AudioFormat::Flac,
        "wav" => bandcamp_api::AudioFormat::Wav,
        "aac-hi" | "aac" => bandcamp_api::AudioFormat::AacHi,
        "mp3-320" | "mp3" => bandcamp_api::AudioFormat::Mp3_320,
        "aiff-lossless" | "aiff" => bandcamp_api::AudioFormat::AiffLossless,
        "vorbis" | "ogg" => bandcamp_api::AudioFormat::Vorbis,
        "mp3-v0" => bandcamp_api::AudioFormat::Mp3V0,
        "alac" => bandcamp_api::AudioFormat::Alac,
        _ => {
            tracing::warn!(format = %s, "Unknown format, defaulting to FLAC");
            bandcamp_api::AudioFormat::Flac
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_bandcamp=info".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!(
        cookies = %args.cookies.display(),
        downloads_dir = %args.downloads_dir.display(),
        inbox_dir = %args.inbox_dir.display(),
        cache = %args.cache.display(),
        socket = %args.socket,
        library_socket = %args.library_socket,
        format = %args.format,
        username = ?args.username,
        "Starting MDMA Bandcamp service"
    );

    // Create directories if they don't exist
    std::fs::create_dir_all(&args.downloads_dir)?;
    std::fs::create_dir_all(&args.inbox_dir)?;

    // Create cache directory if needed
    if let Some(parent) = args.cache.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Validate cookies before starting the service so failures surface immediately
    // with an actionable message rather than obscure downstream errors.
    if let Err(err) = bandcamp_api::load_cookies(&args.cookies) {
        tracing::error!(
            path = %args.cookies.display(),
            error = %err,
            "Bandcamp cookies not found or invalid at {} — upload via web console at http://mdma-909.local or see bandcamp-setup.md",
            args.cookies.display(),
        );
        std::process::exit(1);
    }

    let format = parse_format(&args.format);

    // Initialize service
    let bandcamp_service = service::BandcampService::new(
        args.cookies,
        args.downloads_dir,
        args.inbox_dir,
        args.cache,
        format,
        args.library_socket,
        args.username,
    )?;

    let service = std::sync::Arc::new(bandcamp_service);

    // Spawn download worker
    let worker_service = service.clone();
    tokio::spawn(async move {
        service::run_download_worker(worker_service).await;
    });

    // Create IPC socket directory and bind the socket
    let ServiceSockets { rep_socket, .. } = ::service::create_sockets(&ServiceConfig {
        socket_address: args.socket.clone(),
        event_address: None,
    })?;

    // Run async IPC server
    // NNG blocking I/O runs in a spawn_blocking task, bridged to async via channels
    service::run_async_ipc_server(service, rep_socket).await?;

    Ok(())
}
