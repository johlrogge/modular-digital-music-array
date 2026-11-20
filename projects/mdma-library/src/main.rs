use clap::Parser;
use color_eyre::Result;

use library_service::service::{run_ipc_server, spawn_fact_subscriber};
use library_service::{LibraryService, ReleaseVersion};
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

/// On SIGTERM or SIGINT (the IPC loop never returns on its own): stop the AIFF
/// conversion worker after the job in hand, then exit. An encode longer than the
/// supervisor's grace period is killed with the process; the startup sweep
/// removes what it left in staging (ADR-037).
///
/// This task needs the multi-threaded runtime (the `#[tokio::main]` default):
/// `run_ipc_server` blocks the thread `main` runs on, so the task must run on a
/// worker thread.
fn stop_on_signal(library: std::sync::Arc<LibraryService>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            tracing::warn!(
                "Could not listen for SIGTERM; the conversion worker is not stopped on exit"
            );
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        tracing::info!("Stopping");
        // Wait briefly for the job in hand, then exit anyway: the crash order makes an
        // interrupted conversion safe, and the flac child dies with us.
        let ended = tokio::task::spawn_blocking(move || {
            library.shutdown_conversions_timeout(std::time::Duration::from_secs(3))
        })
        .await
        .unwrap_or(false);
        if !ended {
            tracing::warn!("A conversion was still running; exiting without it");
        }
        std::process::exit(0);
    });
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
        ReleaseVersion::new(env!("CARGO_PKG_VERSION"))?,
    )?;

    tracing::info!(
        tracks = library.tracks_count(),
        facts = library.facts_count(),
        "Loaded library from facts"
    );

    // Spawn background ACID fact subscriber for incremental index updates
    spawn_fact_subscriber(std::sync::Arc::clone(&library));

    // Run IPC server (blocking)
    stop_on_signal(std::sync::Arc::clone(&library));

    let served = run_ipc_server(std::sync::Arc::clone(&library), &args.socket);

    // Stop the AIFF conversion worker after the job in hand, whatever ended the server.
    library.shutdown_conversions();
    served?;

    Ok(())
}
