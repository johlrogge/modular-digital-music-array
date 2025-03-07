mod error;
mod server;

use std::sync::Arc;

use color_eyre::Result;
use nng::{Protocol, Socket};
use playback_engine::PlaybackEngine;
use server::Server;

use tokio::runtime::Runtime;
use tokio::sync::Mutex;

// In playback_server/src/main.rs
fn main() -> Result<()> {
    // Initialize error handling and logging
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    // Create a Tokio runtime explicitly
    let runtime = Runtime::new()?;

    // Create the playback engine
    let engine = Arc::new(Mutex::new(PlaybackEngine::new()?));

    // Create NNG socket for receiving commands
    let socket = Socket::new(Protocol::Rep0)?;
    socket.listen("ipc:///tmp/mdma-commands")?;

    // Create and run server
    let server = Server::new(engine, socket);
    runtime.block_on(server.run())?;

    Ok(())
}
