use bevy_app::config::DjWorkspaceConfig;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "dj-workspace", about = "MDMA DJ Workspace")]
struct Cli {
    /// MDMA node hostname (e.g. mdma-909.local). Derives gateway addresses automatically.
    #[arg(long, env = "MDMA_NODE")]
    node: Option<String>,

    /// Library IPC socket path (used when --node is not set).
    #[arg(
        long,
        default_value = "ipc:///run/mdma/library.sock",
        env = "MDMA_LIBRARY_SOCKET"
    )]
    library_socket: String,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // Derive full NNG gateway address from node hostname via ClientConfig.
    let client_cfg = client::ClientConfig {
        node: cli.node.clone(),
        ..Default::default()
    };
    let gateway = client_cfg.gateway_addr();

    bevy_app::run(DjWorkspaceConfig {
        gateway,
        library_socket: cli.library_socket,
    });
}
