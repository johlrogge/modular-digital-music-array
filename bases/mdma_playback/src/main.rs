mod error;
mod playback_state;
mod server;
pub(crate) mod stream_client;

use acid_client::AcidClient;
use server::Server;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use color_eyre::Result;
use nng::{Protocol, Socket};
use stream_client::StreamClient;
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

    /// Socket address of the mdma-audio source
    #[arg(long, default_value = "ipc:///run/mdma/streams/audio.sock")]
    audio_socket: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let runtime = Runtime::new()?;

    // NNG Req0 dial is non-blocking — it starts connecting in background.
    // The actual error only surfaces on the first send if nobody is listening.
    let audio_client = StreamClient::connect(&args.audio_socket)?;
    let audio = Arc::new(tokio::sync::Mutex::new(audio_client));

    let socket = Socket::new(Protocol::Rep0)?;
    info!("Listening on {}", args.socket);
    socket.listen(&args.socket)?;

    let event_pub = Socket::new(Protocol::Pub0)?;
    info!("Event publishing on {}", args.event_socket);
    event_pub.listen(&args.event_socket)?;

    let acid_client = Arc::new(AcidClient::connect(&args.acid_socket)?);
    let server = Server::new(audio, socket, args.queue_file, event_pub, acid_client);
    runtime.block_on(server.run())?;

    Ok(())
}
