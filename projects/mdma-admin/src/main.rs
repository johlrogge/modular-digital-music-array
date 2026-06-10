mod service;

use clap::Parser;
use color_eyre::Result;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Admin — privileged system-level operations service"
)]
struct Args {
    #[arg(long, default_value = "ipc:///run/mdma/admin.sock")]
    socket: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_admin=info".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!(socket = %args.socket, "Starting MDMA Admin service");

    tokio::task::spawn_blocking(move || service::run(&args.socket))
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Service task panicked: {e}"))??;

    Ok(())
}
