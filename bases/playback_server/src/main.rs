mod error;
mod server;

use std::sync::{Arc, Mutex};

use crate::error::ServerError;
use color_eyre::Result;
use nng::{Protocol, Socket};
use playback_engine::PlaybackEngine;
use server::Server;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize error handling and logging
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    // Create the playback engine
    let engine = PlaybackEngine::new()?;
    let engine = Arc::new(Mutex::new(engine));

    // Create NNG socket for receiving commands
    let socket = Socket::new(Protocol::Rep0).map_err(|e| ServerError::Nng(e.to_string()))?;
    socket
        .listen("ipc:///tmp/mdma-commands")
        .map_err(|e| ServerError::Nng(e.to_string()))?;

    // Create and run server
    let server = Server::new(engine, socket);
    server.run().await?;

    Ok(())
}
