mod error;
mod server;

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

    let server = Server::new(engine, socket);
    runtime.block_on(server.run())?;

    Ok(())
}
