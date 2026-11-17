mod app;
mod background;
mod browse_field;
mod browser_pane;
mod commands;
mod events;
mod input;
mod link;
mod now_playing;
mod pane;
mod playlist_pane;
mod playlists_pane;
mod queue_pane;
mod rows;
mod search_pane;
mod search_parse;
mod selection;
#[cfg(test)]
mod test_support;
mod theme;
mod track_list;
mod ui;

use app::App;
use clap::Parser;
use client_facts::{FactsChoice, FactsWord, Urgency};
use color_eyre::Result;
use link::Link;
use mdma_client::{NeededBy, Node, NodeOptions, NodeSpec};
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
        MDMA_FACTS       replica|node. Where browsing and search read from (same as --facts).\n  \
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

    /// Where browsing and search read from: replica (the local copy of the node's
    /// facts, works offline) or node. Defaults to replica. The flag wins over
    /// MDMA_FACTS; an empty value counts as not given.
    #[arg(long, env = "MDMA_FACTS")]
    facts: Option<FactsWord>,
}

impl Cli {
    /// The facts source asked for, the replica when none was.
    fn facts_choice(&self) -> FactsChoice {
        self.facts.and_then(FactsWord::choice).unwrap_or_default()
    }
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let facts = cli.facts_choice();
    let node = Rc::new(Node::needed_by(NeededBy::new("mdma-tui"), cli.into()));

    // Resolve the node, probe it once (at most 2 s for a remote node) and open the
    // facts before raw mode, so the pick prompt and notes print on the normal
    // terminal. A node that cannot be chosen or reached is a state the panes show,
    // not a reason to exit. Events are subscribed to on the first tick, and again
    // whenever the node comes back.
    let link = Link::new(node, facts);

    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Decide once, before the terminal switches to the alternate screen, so a
    // warning about a bad MDMA_TUI_COLORS stays visible.
    let color_mode = theme::mode_from_env();

    // Left = Browser (Artists), right = Queue.
    let mut app = App::start(link, color_mode);

    // A sync of the replica is due when it is stale; the TUI never syncs inline.
    if let Some(problem) = app.request_sync(Urgency::WhenStale) {
        app.set_status(problem);
    }

    tui_base::run(&mut app, &tui_base::TuiConfig::default())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::LinkError;
    use crate::test_support::World;
    use client_facts::test_support::{env_lock, EnvVars};
    use mdma_client::NodeError;

    /// `args` parsed by [`Cli`] with the environment `vars` (set when `Some`, removed
    /// when `None`) for the length of the parse, and `pick` taken of the result.
    fn parsed<T>(
        vars: &[(&'static str, Option<&str>)],
        args: &[&str],
        pick: impl FnOnce(Cli) -> T,
    ) -> Result<T, clap::Error> {
        // Declared first, so the environment is put back before the lock goes.
        let _lock = env_lock();
        let mut env = EnvVars::default();
        for (name, value) in vars {
            match value {
                Some(value) => env.set(name, value),
                None => env.unset(name),
            }
        }
        Cli::try_parse_from(args).map(pick)
    }

    const NO_SOCKETS: [(&str, Option<&str>); 2] = [
        ("MDMA_LIBRARY_SOCKET", None),
        ("MDMA_PLAYBACK_SOCKET", None),
    ];

    fn options(vars: &[(&'static str, Option<&str>)], args: &[&str]) -> NodeOptions {
        parsed(vars, args, NodeOptions::from).unwrap()
    }

    /// The source asked for with `MDMA_FACTS` as `env` (unset when `None`) and `args`.
    fn facts_choice(env: Option<&str>, args: &[&str]) -> Result<FactsChoice, clap::Error> {
        parsed(&[("MDMA_FACTS", env)], args, |cli| cli.facts_choice())
    }

    #[test]
    fn sockets_are_unset_unless_given() {
        let none = options(&NO_SOCKETS, &["mdma-tui"]);
        let given = options(&NO_SOCKETS, &["mdma-tui", "--library-socket", "x"]);

        assert_eq!(none.library_socket, None);
        assert_eq!(none.playback_socket, None);
        assert_eq!(given.library_socket.as_deref(), Some("x"));
    }

    #[test]
    fn mdma_node_env_is_left_to_node() {
        let parsed = options(&[("MDMA_NODE", Some("mdma.local"))], &["mdma-tui"]);

        assert!(parsed.node.is_none());
    }

    #[test]
    fn node_flag_is_parsed_as_a_node_spec() {
        let good = options(&NO_SOCKETS, &["mdma-tui", "--node", "mdma.local:6000"]);
        let bad = parsed(
            &NO_SOCKETS,
            &["mdma-tui", "--node", "mdma.local:nope"],
            NodeOptions::from,
        );

        let spec = good.node.unwrap();
        assert_eq!(spec.host(), "mdma.local");
        assert_eq!(spec.port(), Some(6000));
        assert!(bad.is_err());
    }

    /// A node that cannot even be named does not end the TUI: it is a state the node
    /// panes show, and the facts are read from the replica all the same.
    #[test]
    fn invalid_mdma_node_is_a_node_error_not_a_reason_to_exit() {
        // Declared before `env`, so the environment is put back before the lock goes.
        let _world = World::new(None);
        let mut env = EnvVars::default();
        env.set("MDMA_NODE", "mdma.local:nope");
        let node = Rc::new(Node::needed_by(
            NeededBy::new("mdma-tui"),
            NodeOptions::default(),
        ));

        let link = Link::new(node, FactsChoice::default());

        let error = link.library().err().expect("the node cannot be named");
        assert!(
            matches!(error, LinkError::Node(NodeError::InvalidEnv(_))),
            "{error:?}"
        );
    }

    #[test]
    fn facts_default_to_the_replica_and_the_flag_wins_over_the_variable() {
        let default = facts_choice(None, &["mdma-tui"]);
        let from_env = facts_choice(Some("node"), &["mdma-tui"]);
        let flag = facts_choice(Some("node"), &["mdma-tui", "--facts=replica"]);
        let invalid = facts_choice(None, &["mdma-tui", "--facts=cache"]);
        let bad_env = facts_choice(Some("cache"), &["mdma-tui"]);

        assert_eq!(default.unwrap(), FactsChoice::Replica);
        assert_eq!(from_env.unwrap(), FactsChoice::Node);
        assert_eq!(flag.unwrap(), FactsChoice::Replica);
        assert!(invalid.is_err(), "an invalid value is a usage error");
        assert!(bad_env.is_err(), "so is an invalid MDMA_FACTS");
    }

    /// An empty value counts as not given, for the variable and for the flag alike.
    #[test]
    fn an_empty_facts_choice_is_not_given() {
        let empty_env = facts_choice(Some(""), &["mdma-tui"]);
        let empty_flag = facts_choice(None, &["mdma-tui", "--facts="]);

        assert_eq!(empty_env.unwrap(), FactsChoice::Replica);
        assert_eq!(empty_flag.unwrap(), FactsChoice::Replica);
    }
}
