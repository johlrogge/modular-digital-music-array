mod app;
mod browse_field;
mod browser_pane;
mod commands;
mod error;
mod events;
mod input;
mod now_playing;
mod pane;
mod playlist_pane;
mod playlists_pane;
mod queue_pane;
mod search_pane;
mod selection;
mod theme;
mod track_list;
mod ui;

use app::App;
use browser_pane::BrowserPane;
use clap::Parser;
use color_eyre::Result;
use events::spawn_event_subscriber;
use mdma_client::{LibraryBackend, PlaybackBackend};
use queue_pane::QueuePane;
use std::rc::Rc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "mdma-tui", about = "MDMA Terminal User Interface")]
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

    /// Playback IPC socket path (used when --node is not set).
    #[arg(
        long,
        default_value = "ipc:///run/mdma/playback.sock",
        env = "MDMA_PLAYBACK_SOCKET"
    )]
    playback_socket: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // Derive full NNG gateway address from node hostname (tcp://hostname:5555).
    let gateway: Option<String> = cli.node.as_deref().map(|n| format!("tcp://{}:5555", n));
    let gateway = gateway.as_deref();

    let library = Rc::new(
        LibraryBackend::connect(gateway, &cli.library_socket)
            .map_err(|e| color_eyre::eyre::eyre!("Library connect failed: {}", e))?,
    );
    let playback = Rc::new(
        PlaybackBackend::connect(gateway, &cli.playback_socket)
            .map_err(|e| color_eyre::eyre::eyre!("Playback connect failed: {}", e))?,
    );

    // Subscribe to playback events. Derives address from --node (same as CLI).
    // Non-fatal: TUI still works without live updates if subscription fails.
    let event_rx = cli
        .node
        .as_deref()
        .and_then(|n| spawn_event_subscriber(&format!("tcp://{}:5556", n)).ok());

    // Build initial panes: left = Browser (Artists), right = Queue.
    let left_pane: Box<dyn pane::Pane> = Box::new(BrowserPane::new(Rc::clone(&library)));
    let right_pane: Box<dyn pane::Pane> =
        Box::new(QueuePane::new(Rc::clone(&playback), Rc::clone(&library)));

    let mut app = App::new(
        left_pane,
        right_pane,
        Rc::clone(&library),
        Rc::clone(&playback),
        event_rx,
    );

    tui_base::run(&mut app, &tui_base::TuiConfig::default())?;

    Ok(())
}
