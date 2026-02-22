mod error;
mod server;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use color_eyre::Result;
use nng::{Protocol, Socket};
use playback_engine::PlaybackEngine;
use server::Server;
use tokio::runtime::Runtime;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Playback - Audio playback server with nng IPC"
)]
struct Args {
    /// nng IPC socket path
    #[arg(long, default_value = "ipc:///run/mdma/playback.sock")]
    socket: String,

    /// Also listen on TCP for remote connections (e.g., "tcp://0.0.0.0:5557")
    #[arg(long)]
    tcp: Option<String>,

    /// Path to queue persistence file (relative to cwd, which should be /music)
    #[arg(long, default_value = "queue.json")]
    queue_file: PathBuf,

    /// Event publishing socket (Pub0) for real-time notifications
    #[arg(long, default_value = "ipc:///run/mdma/events.sock")]
    event_socket: String,

    /// Path to the facts stream file for play history
    #[arg(long, default_value = "/metadata/facts.jsonl")]
    facts_path: PathBuf,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let runtime = Runtime::new()?;

    let engine = Arc::new(tokio::sync::Mutex::new(PlaybackEngine::new()?));

    let socket = Socket::new(Protocol::Rep0)?;
    info!("Listening on {}", args.socket);
    socket.listen(&args.socket)?;

    if let Some(ref tcp) = args.tcp {
        info!("Also listening on TCP: {}", tcp);
        socket.listen(tcp)?;
    }

    let event_pub = Socket::new(Protocol::Pub0)?;
    info!("Event publishing on {}", args.event_socket);
    event_pub.listen(&args.event_socket)?;

    let server = Server::new(engine, socket, args.queue_file, event_pub, args.facts_path);
    runtime.block_on(server.run())?;

    Ok(())
}
