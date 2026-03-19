use clap::Parser;
use color_eyre::Result;

use library_service::service::{run_ipc_server, spawn_fact_subscriber};
use library_service::LibraryService;
use service::ensure_ipc_dir;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Library - Music library service with nng IPC"
)]
struct Args {
    /// Path to music directory
    #[arg(long, default_value = "/music")]
    music_dir: std::path::PathBuf,

    /// Path to metadata directory
    #[arg(long, default_value = "/metadata")]
    metadata_dir: std::path::PathBuf,

    /// nng IPC socket path
    #[arg(long, default_value = "ipc:///run/mdma/library.sock")]
    socket: String,

    /// ACID service socket address
    #[arg(long, default_value = "ipc:///run/mdma/acid.sock")]
    acid_socket: String,

    /// ACID events pub/sub socket address (Sub0)
    #[arg(long, default_value = "ipc:///run/mdma/acid-events.sock")]
    acid_events_socket: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_library=info".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!(
        music_dir = %args.music_dir.display(),
        metadata_dir = %args.metadata_dir.display(),
        socket = %args.socket,
        "Starting MDMA Library service"
    );

    // Create directories if they don't exist
    std::fs::create_dir_all(args.music_dir.join("inbox"))?;
    std::fs::create_dir_all(args.music_dir.join("blobs"))?;
    std::fs::create_dir_all(args.music_dir.join("by-artist"))?;
    std::fs::create_dir_all(&args.metadata_dir)?;
    std::fs::create_dir_all(args.metadata_dir.join("playlists"))?;

    // Create socket directory if needed
    ensure_ipc_dir(&args.socket)?;

    // Initialize service
    let library = LibraryService::new_with_events(
        args.music_dir,
        args.metadata_dir,
        &args.acid_socket,
        &args.acid_events_socket,
    )?;

    tracing::info!(
        tracks = library.tracks_count(),
        facts = library.facts_count(),
        "Loaded library from facts"
    );

    let library = std::sync::Arc::new(library);

    // Spawn background ACID fact subscriber for incremental index updates
    spawn_fact_subscriber(std::sync::Arc::clone(&library));

    // Run IPC server (blocking)
    run_ipc_server(library, &args.socket)?;

    Ok(())
}
