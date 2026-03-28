use clap::Parser;
use color_eyre::Result;
use service::{ServiceConfig, ServiceSockets};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA ACID - Append-only fact stream writer service"
)]
struct Args {
    #[arg(long, default_value = "/metadata")]
    metadata_dir: PathBuf,
    #[arg(long, default_value = "ipc:///run/mdma/acid.sock")]
    socket: String,
    #[arg(long, default_value = "ipc:///run/mdma/acid-events.sock")]
    event_socket: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_acid=info".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!(
        metadata_dir = %args.metadata_dir.display(),
        socket = %args.socket,
        event_socket = %args.event_socket,
        "Starting MDMA ACID service"
    );

    let ServiceSockets {
        rep_socket,
        event_socket,
    } = service::create_sockets(&ServiceConfig {
        socket_address: args.socket.clone(),
        event_address: Some(args.event_socket.clone()),
    })
    .map_err(|e| color_eyre::eyre::eyre!("Failed to create service sockets: {}", e))?;

    let pub_sock = event_socket.expect("event socket must be configured for mdma-acid");

    tracing::info!(address = %args.socket, "ACID service listening");
    tracing::info!(address = %args.event_socket, "ACID event socket listening");

    let _handle = acid_service::start(rep_socket, pub_sock, &args.metadata_dir)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to start ACID service: {}", e))?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
