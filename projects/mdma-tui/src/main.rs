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
mod rows;
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
use mdma_client::{
    LibraryBackend, NeededBy, Node, NodeError, NodeOptions, NodeSpec, PlaybackBackend,
};
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
        \x20                Set it when COLORTERM is not forwarded, for example over SSH.\n  \
        MDMA_NODE        host[:port] of the node to use (same as --node).\n  \
        MDMA_LIBRARY_SOCKET, MDMA_PLAYBACK_SOCKET\n  \
        \x20                Local-mode socket paths on the node; ignored when --node or\n  \
        \x20                MDMA_NODE is set."
)]
struct Cli {
    /// MDMA node as host[:port] (e.g. mdma-909.local). Without it, MDMA_NODE, local
    /// services, the node chosen with `mdma node select`, or discovery are used.
    // No `env = "MDMA_NODE"`: `Node` reads it itself, so an explicit flag stays
    // distinguishable from the environment.
    #[arg(long, value_parser = clap::value_parser!(NodeSpec))]
    node: Option<NodeSpec>,

    /// Library IPC socket path, for local mode on the node. Ignored when --node or
    /// MDMA_NODE is set.
    #[arg(long, env = "MDMA_LIBRARY_SOCKET")]
    library_socket: Option<String>,

    /// Playback IPC socket path, for local mode on the node. Ignored when --node or
    /// MDMA_NODE is set.
    #[arg(long, env = "MDMA_PLAYBACK_SOCKET")]
    playback_socket: Option<String>,
}

impl From<Cli> for NodeOptions {
    fn from(cli: Cli) -> Self {
        Self {
            node: cli.node,
            library_socket: cli.library_socket,
            playback_socket: cli.playback_socket,
            sources_dir: None,
        }
    }
}

/// Resolved node connections: library, playback and the event gateway address.
type Connections = (LibraryBackend, PlaybackBackend, Option<String>);

/// Resolve the node and connect. Runs before raw mode so the pick prompt and
/// notes print on the normal terminal.
fn connect(options: NodeOptions) -> Result<Connections, NodeError> {
    let node = Node::needed_by(NeededBy::new("mdma-tui"), options);
    node.target()?;
    Ok((node.library()?, node.playback()?, node.event_gateway()?))
}

fn main() -> Result<()> {
    // Node errors are advice for the user, not crashes: print them plainly,
    // like the CLI, and keep color_eyre's crash report for unexpected errors.
    let cli = Cli::parse();
    let (library, playback, event_gateway) = match connect(cli.into()) {
        Ok(connections) => connections,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Decide once, before the terminal switches to the alternate screen, so a
    // warning about a bad MDMA_TUI_COLORS stays visible.
    let color_mode = theme::mode_from_env();

    let library = Rc::new(library);
    let playback = Rc::new(playback);

    // Subscribe to playback events from the resolved node.
    // Non-fatal: TUI still works without live updates if subscription fails.
    let event_rx = event_gateway.and_then(|addr| spawn_event_subscriber(&addr).ok());

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn options(args: &[&str]) -> Result<NodeOptions, clap::Error> {
        Cli::try_parse_from(args).map(NodeOptions::from)
    }

    /// Run `f` under ENV_LOCK with the given vars set (`Some`) or removed
    /// (`None`), restoring the previous values before returning.
    fn with_env<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        let result = f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        result
    }

    const NO_SOCKETS: [(&str, Option<&str>); 2] = [
        ("MDMA_LIBRARY_SOCKET", None),
        ("MDMA_PLAYBACK_SOCKET", None),
    ];

    #[test]
    fn sockets_are_unset_unless_given() {
        let (none, given) = with_env(&NO_SOCKETS, || {
            (
                options(&["mdma-tui"]),
                options(&["mdma-tui", "--library-socket", "x"]),
            )
        });
        let none = none.unwrap();
        assert_eq!(none.library_socket, None);
        assert_eq!(none.playback_socket, None);
        assert_eq!(given.unwrap().library_socket.as_deref(), Some("x"));
    }

    #[test]
    fn mdma_node_env_is_left_to_node() {
        let parsed = with_env(&[("MDMA_NODE", Some("mdma.local"))], || {
            options(&["mdma-tui"])
        });
        assert!(parsed.unwrap().node.is_none());
    }

    #[test]
    fn node_flag_is_parsed_as_a_node_spec() {
        let (good, bad) = with_env(&NO_SOCKETS, || {
            (
                options(&["mdma-tui", "--node", "mdma.local:6000"]),
                options(&["mdma-tui", "--node", "mdma.local:nope"]),
            )
        });
        let spec = good.unwrap().node.unwrap();
        assert_eq!(spec.host(), "mdma.local");
        assert_eq!(spec.port(), Some(6000));
        assert!(bad.is_err());
    }

    #[test]
    fn invalid_mdma_node_is_a_node_error() {
        let result = with_env(&[("MDMA_NODE", Some("mdma.local:nope"))], || {
            connect(NodeOptions::default())
        });
        assert!(matches!(result, Err(NodeError::InvalidEnv(_))));
    }
}
