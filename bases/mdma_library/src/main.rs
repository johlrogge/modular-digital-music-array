use clap::Parser;
use color_eyre::Result;

mod fact_generator;
mod fact_writer;
pub mod ipc;
mod pipeline;
mod service;

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

    // Create socket directory if needed
    if args.socket.starts_with("ipc://") {
        if let Some(path) = args.socket.strip_prefix("ipc://") {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
    }

    // Initialize service
    let library = service::LibraryService::new(args.music_dir, args.metadata_dir)?;

    tracing::info!(
        tracks = library.tracks_count(),
        facts = library.facts_count(),
        "Loaded library from facts"
    );

    let library = std::sync::Arc::new(library);

    // Run IPC server (blocking)
    service::run_ipc_server(library, &args.socket)?;

    Ok(())
}
