mod error;
mod playback_state;
mod server;

use acid_client::AcidClient;
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

    /// Path to queue persistence file (relative to cwd, which should be /music)
    #[arg(long, default_value = "queue.json")]
    queue_file: PathBuf,

    /// Event publishing socket (Pub0) for real-time notifications
    #[arg(long, default_value = "ipc:///run/mdma/events.sock")]
    event_socket: String,

    /// ACID service socket address for writing play history facts
    #[arg(long, default_value = "ipc:///run/mdma/acid.sock")]
    acid_socket: String,

    /// Path to audio output configuration file
    #[arg(long, default_value = "/metadata/audio-output.json")]
    audio_config: PathBuf,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let runtime = Runtime::new()?;

    let engine = Arc::new(tokio::sync::Mutex::new(PlaybackEngine::new(
        args.audio_config,
    )?));

    let socket = Socket::new(Protocol::Rep0)?;
    info!("Listening on {}", args.socket);
    socket.listen(&args.socket)?;

    let event_pub = Socket::new(Protocol::Pub0)?;
    info!("Event publishing on {}", args.event_socket);
    event_pub.listen(&args.event_socket)?;

    let acid_client = Arc::new(AcidClient::connect(&args.acid_socket)?);
    let server = Server::new(engine, socket, args.queue_file, event_pub, acid_client);
    runtime.block_on(server.run())?;

    Ok(())
}
