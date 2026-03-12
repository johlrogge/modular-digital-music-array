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
mod track_list;
mod ui;

use app::App;
use browser_pane::BrowserPane;
use clap::Parser;
use color_eyre::Result;
use crossterm::{
    event::{poll, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use event_protocol::PlaybackEvent;
use events::{spawn_event_subscriber, AppEvent};
use mdma_client::{LibraryBackend, PlaybackBackend};
use pane::PaneKind;
use queue_pane::QueuePane;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::rc::Rc;
use std::time::Duration;
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

fn event_addr_from_node(node: &str) -> String {
    format!("tcp://{}:5556", node)
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

    // Attempt to subscribe to playback events. Failure here is non-fatal;
    // the TUI still works, just without live now-playing updates.
    let event_rx = cli
        .node
        .as_deref()
        .map(|node| spawn_event_subscriber(&event_addr_from_node(node)))
        .and_then(|r| r.ok());

    // Build initial panes: left = Browser (Artists), right = Queue.
    let left_pane: Box<dyn pane::Pane> = Box::new(BrowserPane::new(Rc::clone(&library)));
    let right_pane: Box<dyn pane::Pane> =
        Box::new(QueuePane::new(Rc::clone(&playback), Rc::clone(&library)));

    let mut app = App::new(
        left_pane,
        right_pane,
        Rc::clone(&library),
        Rc::clone(&playback),
    );

    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Restore terminal on drop (including panics)
    scopeguard::defer! {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        if poll(Duration::from_millis(100))? {
            match crossterm::event::read()? {
                Event::Key(key) => {
                    input::handle_key(&mut app, key);
                }
                Event::Resize(_, _) => {
                    // Force redraw on next iteration; nothing extra needed.
                }
                _ => {}
            }
        }

        if let Some(ref rx) = event_rx {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    AppEvent::Playback(pe) => {
                        // Refresh queue panes when the queue changes on the node.
                        if matches!(pe, PlaybackEvent::QueueChanged { .. }) {
                            if app.left_pane.pane_kind() == PaneKind::Queue {
                                app.left_pane.refresh();
                            }
                            if app.right_pane.pane_kind() == PaneKind::Queue {
                                app.right_pane.refresh();
                            }
                        }
                        app.now_playing.apply(&pe);
                    }
                    AppEvent::SubscriberError(msg) => {
                        app.set_status(format!("Event error: {msg}"));
                    }
                }
            }
        }

        terminal.draw(|f| ui::render(f, &app))?;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
