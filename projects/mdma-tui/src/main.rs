mod app;
mod browse_field;
mod browser_pane;
mod commands;
mod events;
mod input;
mod now_playing;
mod pane;
mod playlist_pane;
mod playlists_pane;
mod queue_pane;
mod search_pane;
mod search_parse;
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
#[command(
    name = "mdma-tui",
    about = "MDMA Terminal User Interface",
    after_help = "ENVIRONMENT:\n  \
        MDMA_TUI_COLORS  truecolor|24bit|256. Force 24-bit or 256-colour output. Without it\n  \
        \x20                the TUI uses 24-bit colour only when COLORTERM is truecolor or 24bit.\n  \
        \x20                Set it when COLORTERM is not forwarded, for example over SSH."
)]
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

    // Decide once, before the terminal switches to the alternate screen, so a
    // warning about a bad MDMA_TUI_COLORS stays visible.
    let color_mode = theme::mode_from_env();

    // Derive full NNG gateway/event addresses from node hostname via ClientConfig.
    let client_cfg = mdma_client::ClientConfig {
        node: cli.node.clone(),
        ..Default::default()
    };
    let gateway = client_cfg.gateway_addr();
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
    let event_rx = client_cfg
        .event_addr()
        .and_then(|addr| spawn_event_subscriber(&addr).ok());

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
        color_mode,
    );

    tui_base::run(&mut app, &tui_base::TuiConfig::default())?;

    Ok(())
}
