use clap::Parser;
use color_eyre::Result;

mod fact_generator;
mod pipeline;

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

    // TODO: Initialize service
    // 1. Create inbox watcher
    // 2. Start nng IPC server
    // 3. Process existing inbox files
    // 4. Enter main loop

    tracing::info!("MDMA Library service stub - implementation pending");

    Ok(())
}
