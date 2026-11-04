//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to services via gateway (single address) or direct IPC.

mod admin;
mod args;
mod completions;
mod ctx;
mod editor;
mod fact;
mod inbox;
mod library;
mod node;
mod output;
mod playback;
mod playlist;
mod queue;
mod rekordbox;
mod search;
mod source;
mod stdin;
mod subscribe;
#[cfg(test)]
mod test_support;
mod tracks;
mod transfer;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use color_eyre::Result;
use ctx::{Ctx, Globals};

#[derive(Parser, Debug)]
#[command(name = "mdma")]
#[command(author, version, about = "MDMA CLI - Control the music services")]
struct Cli {
    /// Node hostname (e.g. mdma-909.local), optionally with a port. Also read from
    /// MDMA_NODE. Without either, the remembered or discovered node is used.
    #[arg(long)]
    node: Option<node_discovery::NodeSpec>,

    /// Library IPC socket address (direct mode, ignored when --node is set)
    #[arg(
        long,
        default_value = ctx::DEFAULT_LIBRARY_SOCKET,
        env = "MDMA_LIBRARY_SOCKET"
    )]
    socket: String,

    /// Playback server socket address (direct mode, ignored when --node is set)
    #[arg(
        long,
        default_value = ctx::DEFAULT_PLAYBACK_SOCKET,
        env = "MDMA_PLAYBACK_SOCKET"
    )]
    playback_socket: String,

    /// Sources directory for direct mode (contains *.sock files)
    #[arg(long, default_value = "/run/mdma/sources", env = "MDMA_SOURCES_DIR")]
    sources_dir: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Check if the service is running
    Ping,

    /// Get service status
    Status,

    /// List tracks in library
    List(tracks::ListCli),

    /// Get a specific track by hash (supports partial hashes like git)
    Get(tracks::GetCli),

    /// Show all facts for a track
    Facts(tracks::FactsCli),

    /// Search for tracks
    Search(search::SearchCli),

    /// Inbox management commands
    Inbox {
        #[command(subcommand)]
        command: inbox::Command,
    },

    /// Music source management
    #[command(
        override_usage = "mdma source list\n       mdma source <SOURCE-NAME> <COMMAND>",
        after_help = "Per-source commands: mdma source <SOURCE-NAME> <COMMAND>\n\n\
                      Commands:\n  \
                      sync, status, downloads, cancel, pause, resume, resync,\n  \
                      check-item, check-updates, playlists, import-playlist, import-status\n\n\
                      Example: mdma source bandcamp sync\n\
                      Run 'mdma source <SOURCE-NAME> --help' for details.\n\
                      'list' is reserved and cannot be a source name."
    )]
    Source {
        #[command(subcommand)]
        command: source::Command,
    },

    /// Playback control commands
    Playback {
        #[command(subcommand)]
        command: playback::Command,
    },

    /// Queue management (feeds deck A)
    Queue {
        #[command(subcommand)]
        command: queue::Command,
    },

    /// Sort hashes from stdin by a track metadata field.
    /// Reads one hash per line from stdin, outputs sorted hashes.
    /// Stable sort: chain multiple invocations for multi-key sort (right-to-left priority).
    ///
    /// Examples:
    ///   mdma search --artist=CBL | mdma sort title -a
    ///   mdma queue list | mdma sort bpm -d | mdma queue append
    ///   cat friday.plist | mdma sort title -a | mdma sort artist -a > sorted.plist
    Sort(tracks::SortArgs),

    /// Subscribe to real-time events from the playback system
    Subscribe(subscribe::SubscribeCli),

    /// Upload a file (audio or ZIP of audio) to the library
    Upload(transfer::UploadArgs),

    /// Export tracks from the library to local files.
    ///
    /// Reads content hashes from stdin (one per line, pipe-friendly).
    /// Compatible with search output: `mdma search --artist CBL | mdma export`
    ///
    /// Examples:
    ///   mdma search --artist CBL | mdma export
    ///   mdma search --artist CBL | mdma export --format aiff
    ///   mdma search --artist CBL | mdma export --format wav --output ./archive/
    ///   mdma search --artist CBL | mdma export --lossless-format aiff --lossy-format wav
    Export(transfer::ExportArgs),

    /// Export tracks for Pioneer Rekordbox
    ///
    /// Downloads audio files and generates rekordbox.xml for import
    /// via Rekordbox File → Import Library.
    ///
    /// Examples:
    ///   mdma rekordbox export --playlist my-set
    ///   mdma search --artist CBL | mdma rekordbox export
    Rekordbox {
        #[command(subcommand)]
        command: rekordbox::Command,
    },

    /// Manage playlists
    Playlist {
        #[command(subcommand)]
        command: playlist::Command,
    },

    /// Library maintenance commands
    Library {
        #[command(subcommand)]
        command: library::Command,
    },

    /// Bookmark the currently playing track, or a specific track by hash
    Bookmark(tracks::BookmarkCli),

    /// List all bookmarked tracks
    Bookmarks,

    /// Add or retract metadata facts on a track.
    Fact {
        #[command(subcommand)]
        command: fact::Command,
    },

    /// Display selected fields for tracks read from stdin.
    ///
    /// Reads hashes from stdin (one per line), looks up TrackInfo for each hash,
    /// and renders a colored, columnized table with the selected fields.
    ///
    /// Field order in the output matches the order the flags appear on the command line.
    /// Field-order is detected via clap's indices_of: the position at which each flag
    /// was specified in the argv determines the column order.
    ///
    /// Examples:
    ///   mdma search --artist CBL | mdma view --hash --artist --title
    ///   mdma search --genre Techno | mdma view --title --duration
    ///   cat friday.plist | mdma view --hash --artist --title --duration
    View(tracks::ViewCli),

    /// Generate shell completions
    #[command(hide = true)]
    GenerateCompletions(completions::CompletionsCli),

    /// Admin commands (service mode, reboot)
    Admin {
        #[command(subcommand)]
        command: admin::Command,
    },

    /// Find and choose the MDMA node to talk to
    Node {
        #[command(subcommand)]
        command: node::Command,
    },
}

/// The global options, with `socket` and `playback_socket` only set when given.
fn globals_from(cli: &Cli, matches: &clap::ArgMatches) -> Globals {
    let given = |id: &str, value: &String| {
        matches
            .value_source(id)
            .is_some_and(|s| s != clap::parser::ValueSource::DefaultValue)
            .then(|| value.clone())
    };
    Globals {
        node: cli.node.clone(),
        socket: given("socket", &cli.socket),
        playback_socket: given("playback_socket", &cli.playback_socket),
        sources_dir: cli.sources_dir.clone(),
    }
}

/// The direction of `mdma sort`: exactly one of `-a` / `-d`.
fn sort_direction(
    args: &tracks::SortArgs,
) -> std::result::Result<tracks::SortDirection, clap::Error> {
    tracks::SortDirection::from_flags(args.ascending, args.descending).ok_or_else(|| {
        args::usage_error(
            clap::error::ErrorKind::ArgumentConflict,
            "Specify exactly one of -a (ascending) or -d (descending)",
        )
    })
}

/// The columns of `mdma view`, in the order their flags were typed; at least one.
fn view_columns(
    matches: &clap::ArgMatches,
) -> std::result::Result<tracks::ViewColumns, clap::Error> {
    let columns = tracks::view_columns_from_matches(matches);
    if columns.is_empty() {
        return Err(args::usage_error(
            clap::error::ErrorKind::MissingRequiredArgument,
            "No fields selected. Use --hash, --artist, --title, and/or --duration.",
        ));
    }
    Ok(tracks::ViewColumns(columns))
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // The node is resolved once, on first use, so only commands that talk to
    // one resolve it.
    let ctx = Ctx::new(globals_from(&cli, &matches));

    // Dispatch command - connect to appropriate service based on command
    match cli.command {
        Commands::Ping => tracks::ping(&ctx),
        Commands::Status => tracks::status(&ctx),
        Commands::List(tracks::ListCli { limit }) => tracks::list(tracks::LimitArg(limit), &ctx),
        Commands::Get(tracks::GetCli { hash }) => tracks::get(hash, &ctx),
        Commands::Facts(tracks::FactsCli { hash }) => tracks::facts(hash, &ctx),
        Commands::Search(cli) => search::dispatch(cli, &ctx),
        Commands::Inbox { command } => inbox::dispatch(command, &ctx),
        Commands::Source { command } => source::dispatch(command, &ctx),
        Commands::Playback { command } => playback::dispatch(command, &ctx),
        Commands::Queue { command } => queue::dispatch(command, &ctx),
        Commands::Sort(args) => {
            let direction = sort_direction(&args).unwrap_or_else(|e| e.exit());
            tracks::sort(args.field, direction, &ctx)
        }
        Commands::Subscribe(subscribe::SubscribeCli { topic }) => {
            subscribe::subscribe(subscribe::TopicArg(topic), &ctx)
        }
        Commands::Upload(args) => transfer::upload(args, &ctx),
        Commands::Export(args) => transfer::export(args, &ctx),
        Commands::Rekordbox { command } => rekordbox::dispatch(command, &ctx),
        Commands::Playlist { command } => playlist::dispatch(command, &ctx),
        Commands::Library { command } => library::dispatch(command, &ctx),
        Commands::Bookmark(tracks::BookmarkCli { scope, hash }) => {
            tracks::bookmark(scope, hash, &ctx)
        }
        Commands::Bookmarks => tracks::bookmarks(&ctx),
        Commands::Fact { command } => fact::dispatch(command, &ctx),
        Commands::View(_) => {
            // The column order is the order the flags were typed, which only the
            // raw matches know.
            let view_matches = matches.subcommand_matches("view").unwrap_or_else(|| {
                eprintln!("internal error: expected 'view' subcommand matches");
                std::process::exit(1);
            });
            let columns = view_columns(view_matches).unwrap_or_else(|e| e.exit());
            tracks::view(columns, &ctx)
        }
        Commands::GenerateCompletions(completions::CompletionsCli { shell }) => {
            completions::generate(completions::ShellArg(shell))
        }
        Commands::Admin { command } => admin::dispatch(command, &ctx),
        Commands::Node { command } => node::dispatch(command, &ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn given(args: &[&str]) -> bool {
        let mut argv = vec!["mdma"];
        argv.extend_from_slice(args);
        let matches = Cli::command()
            .try_get_matches_from(argv)
            .expect("valid args");
        let cli = Cli::from_arg_matches(&matches).expect("valid args");
        let globals = globals_from(&cli, &matches);
        globals.socket.is_some() || globals.playback_socket.is_some()
    }

    #[test]
    fn explicit_sockets_are_detected_from_flags_and_environment() {
        // One test, because it touches process-wide environment variables.
        std::env::remove_var("MDMA_LIBRARY_SOCKET");
        std::env::remove_var("MDMA_PLAYBACK_SOCKET");

        assert!(!given(&["ping"]), "defaults are not explicit");
        assert!(given(&["--socket", "ipc:///tmp/lib.sock", "ping"]));
        assert!(given(&["--playback-socket", "ipc:///tmp/pb.sock", "ping"]));

        // The sources directory says nothing about library/playback.
        assert!(!given(&["--sources-dir", "/tmp/sources", "ping"]));
        std::env::set_var("MDMA_SOURCES_DIR", "/tmp/sources");
        assert!(!given(&["ping"]));
        std::env::remove_var("MDMA_SOURCES_DIR");

        std::env::set_var("MDMA_LIBRARY_SOCKET", "ipc:///tmp/lib.sock");
        assert!(given(&["ping"]), "an environment override counts");
        std::env::remove_var("MDMA_LIBRARY_SOCKET");

        std::env::set_var("MDMA_PLAYBACK_SOCKET", "ipc:///tmp/pb.sock");
        assert!(given(&["ping"]));
        std::env::remove_var("MDMA_PLAYBACK_SOCKET");
    }

    fn parse_err(args: &[&str]) -> clap::Error {
        let mut argv = vec!["mdma"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).expect_err("should be rejected")
    }

    #[test]
    fn hash_arguments_are_validated_at_parse_time() {
        for cmd in ["get", "facts", "bookmark"] {
            for bad in ["abcd123", "ABCD1234", "sha256:abc", "xyz"] {
                let e = parse_err(&[cmd, bad]);
                assert_eq!(
                    e.kind(),
                    clap::error::ErrorKind::ValueValidation,
                    "{cmd} {bad}"
                );
                assert_eq!(e.exit_code(), 2, "{cmd} {bad}");
            }
            for good in ["abcd1234", "sha256:abcd1234", "0123456789abcdef"] {
                assert!(
                    Cli::try_parse_from(["mdma", cmd, good]).is_ok(),
                    "{cmd} {good}"
                );
            }
        }
        assert!(Cli::try_parse_from(["mdma", "bookmark"]).is_ok());
        assert!(Cli::try_parse_from(["mdma", "bookmark", "--scope", "x", "abcd1234"]).is_ok());
    }

    fn sort_args(args: &[&str]) -> tracks::SortArgs {
        let mut argv = vec!["mdma", "sort"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).expect("valid args").command {
            Commands::Sort(a) => a,
            other => panic!("expected sort, got {other:?}"),
        }
    }

    #[test]
    fn sort_needs_exactly_one_direction() {
        assert_eq!(
            sort_direction(&sort_args(&["title", "-a"])).unwrap(),
            tracks::SortDirection::Ascending
        );
        assert_eq!(
            sort_direction(&sort_args(&["title", "-d"])).unwrap(),
            tracks::SortDirection::Descending
        );
        for flags in [&["title", "-a", "-d"][..], &["title"][..]] {
            let e = sort_direction(&sort_args(flags)).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{flags:?}");
            assert!(e
                .to_string()
                .contains("Specify exactly one of -a (ascending) or -d (descending)"));
        }
    }

    #[test]
    fn view_needs_at_least_one_column() {
        let matches = Cli::command()
            .try_get_matches_from(["mdma", "view"])
            .unwrap();
        let e = view_columns(matches.subcommand_matches("view").unwrap()).unwrap_err();
        assert_eq!(e.exit_code(), 2);
        assert!(e
            .to_string()
            .contains("No fields selected. Use --hash, --artist, --title, and/or --duration."));

        let matches = Cli::command()
            .try_get_matches_from(["mdma", "view", "--title", "--hash"])
            .unwrap();
        let tracks::ViewColumns(cols) =
            view_columns(matches.subcommand_matches("view").unwrap()).unwrap();
        assert_eq!(cols.len(), 2);
    }

    #[test]
    fn node_flag_is_checked_at_parse_time() {
        for bad in ["host:notaport", "host:0", "host:99999", "", ":5555"] {
            let e =
                crate::Cli::try_parse_from(["mdma", "--node", bad, "node", "show"]).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{bad:?}");
        }
        for (good, port) in [("host", None), ("host:5555", Some(5555))] {
            let cli = crate::Cli::try_parse_from(["mdma", "--node", good, "node", "show"]).unwrap();
            let spec = cli.node.unwrap();
            assert_eq!((spec.host(), spec.port()), ("host", port), "{good}");
        }
    }

    #[test]
    fn node_flag_no_longer_reads_the_environment() {
        // MDMA_NODE is read separately so --node and the env var can be told apart.
        let arg = Cli::command()
            .get_arguments()
            .find(|a| a.get_id() == "node")
            .expect("node arg")
            .get_env()
            .is_none();
        assert!(arg);
    }
}
