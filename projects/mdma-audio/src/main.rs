mod server;

use clap::Parser;
use color_eyre::Result;
use playback_engine::PlaybackEngine;
use server::Server;
use service::{ServiceConfig, ServiceSockets};
use std::path::PathBuf;
use tokio::runtime::Runtime;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Audio - File playback source speaking stream_source_protocol"
)]
struct Args {
    /// NNG Rep0 socket address to listen on
    #[arg(long, default_value = "ipc:///run/mdma/streams/audio.sock")]
    socket: String,

    /// Library IPC socket address for hash resolution
    #[arg(long, default_value = "ipc:///run/mdma/library.sock")]
    library_socket: String,

    /// Root directory where music blobs live (blob paths from library are relative to this)
    #[arg(long, default_value = "/music")]
    music_dir: PathBuf,

    /// Path to audio output configuration file
    #[arg(long, default_value = "/metadata/audio-output.json")]
    audio_config: PathBuf,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_audio=info".into()),
        )
        .init();

    let args = Args::parse();

    let runtime = Runtime::new()?;

    let engine = PlaybackEngine::new(args.audio_config)?;

    info!("Listening on {}", args.socket);
    let ServiceSockets {
        rep_socket: socket, ..
    } = service::create_sockets(&ServiceConfig {
        socket_address: args.socket.clone(),
        event_address: None,
    })?;

    let mut server = Server::new(engine, socket, args.library_socket, args.music_dir);

    runtime.block_on(server.run())?;

    Ok(())
}
