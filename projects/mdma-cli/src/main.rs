//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to services via gateway (single address) or direct IPC.

/// Admin commands (service mode, reboot)
#[hicli::group]
mod admin;
mod args;
mod completions;
mod ctx;
mod editor;
/// Add or retract metadata facts on a track
#[hicli::group]
mod fact;
/// Inbox management commands
#[hicli::group]
mod inbox;
/// Library maintenance commands
#[hicli::group]
mod library;
/// Find and choose the MDMA node to talk to
#[hicli::group]
mod node;
mod output;
/// Playback control commands
#[hicli::group]
mod playback;
/// Manage playlists
#[hicli::group]
mod playlist;
/// Queue management (feeds deck A)
#[hicli::group]
mod queue;
/// Export tracks for Pioneer Rekordbox
///
/// Downloads audio files and generates rekordbox.xml for import
/// via Rekordbox File -> Import Library.
///
/// Examples:
///   mdma rekordbox export --playlist my-set
///   mdma search --artist CBL | mdma rekordbox export
#[hicli::group]
mod rekordbox;
/// Search for tracks
///
/// Filter with options such as --artist, --bpm or --key, or give a free-text query
/// that is applied to all text fields. Hashes piped on stdin restrict the search to
/// those tracks (--no-stdin ignores them); --not inverts the result.
#[hicli::group]
mod search;
/// Music source management
///
/// Per-source commands: mdma source <SOURCE-NAME> <COMMAND>
///
/// Commands: sync, status, downloads, cancel, pause, resume, resync, check-item,
/// check-updates, playlists, import-playlist, import-status.
/// Run 'mdma source <SOURCE-NAME> --help' for details.
/// 'list' is reserved and cannot be a source name.
///
/// Examples:
///   mdma source list
///   mdma source bandcamp sync
#[hicli::group(args = crate::source::SourceArg)]
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

// The hicli command tree, declared beside clap's until `main` runs it.
#[allow(dead_code)] // wired into main in step 2
fn app() -> hicli::App<ctx::Globals, ctx::Ctx> {
    hicli::app! {
        globals: ctx::globals(),
        commands: [
            tracks::ping, tracks::status, tracks::list, tracks::get, tracks::facts,
            search, inbox, source, playback, queue, tracks::sort, subscribe::subscribe,
            transfer::upload, transfer::export, rekordbox, playlist, library,
            tracks::bookmark, tracks::bookmarks, fact, tracks::view,
            completions::generate_completions, admin, node,
        ]
    }
    .about("MDMA CLI - Control the music services")
    .version(env!("CARGO_PKG_VERSION"))
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
        Commands::List(tracks::ListCli { limit }) => tracks::list(tracks::OptLimitArg(limit), &ctx),
        Commands::Get(tracks::GetCli { hash }) => tracks::get(hash, &ctx),
        Commands::Facts(tracks::FactsCli { hash }) => tracks::facts(hash, &ctx),
        Commands::Search(cli) => search::dispatch(cli, &ctx),
        Commands::Inbox { command } => inbox::dispatch(command, &ctx),
        Commands::Source { command } => source::dispatch(command, &ctx),
        Commands::Playback { command } => playback::dispatch(command, &ctx),
        Commands::Queue { command } => queue::dispatch(command, &ctx),
        Commands::Sort(args) => {
            let direction = args.direction().unwrap_or_else(|e| e.exit());
            tracks::sort(direction, args.field, &ctx)
        }
        Commands::Subscribe(subscribe::SubscribeCli { topic }) => {
            subscribe::subscribe(subscribe::OptTopicArg(topic), &ctx)
        }
        Commands::Upload(args) => transfer::upload(args, &ctx),
        Commands::Export(args) => transfer::export(args, &ctx),
        Commands::Rekordbox { command } => rekordbox::dispatch(command, &ctx),
        Commands::Playlist { command } => playlist::dispatch(command, &ctx),
        Commands::Library { command } => library::dispatch(command, &ctx),
        Commands::Bookmark(tracks::BookmarkCli { scope, hash }) => {
            tracks::bookmark(tracks::OptScopeArg(scope), args::OptTrackArg(hash), &ctx)
        }
        Commands::Bookmarks => tracks::bookmarks(&ctx),
        Commands::Fact { command } => fact::dispatch(command, &ctx),
        Commands::View(_) => {
            // The column order is the order the flags were typed, which only the
            // raw matches know.
            let view_matches = matches
                .subcommand_matches("view")
                .expect("Commands::View implies view matches");
            let columns =
                tracks::ViewColumns::from_matches(view_matches).unwrap_or_else(|e| e.exit());
            tracks::view(columns, &ctx)
        }
        Commands::GenerateCompletions(completions::CompletionsCli { shell }) => {
            completions::generate_clap(shell)
        }
        Commands::Admin { command } => admin::dispatch(command, &ctx),
        Commands::Node { command } => node::dispatch(command, &ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{parse, parse_cli, usage_error};

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

    #[test]
    fn hash_arguments_are_validated_at_parse_time() {
        for cmd in ["get", "facts", "bookmark"] {
            for bad in ["abcd123", "ABCD1234", "sha256:abc", "xyz"] {
                let e = usage_error(&[cmd, bad]);
                assert_eq!(
                    e.kind(),
                    clap::error::ErrorKind::ValueValidation,
                    "{cmd} {bad}"
                );
            }
            for good in ["abcd1234", "sha256:abcd1234", "0123456789abcdef"] {
                assert!(parse(&[cmd, good]).is_ok(), "{cmd} {good}");
            }
        }
        assert!(parse(&["bookmark"]).is_ok());
        assert!(parse(&["bookmark", "--scope", "x", "abcd1234"]).is_ok());
    }

    #[test]
    fn node_flag_is_checked_at_parse_time() {
        for bad in ["host:notaport", "host:0", "host:99999", "", ":5555"] {
            usage_error(&["--node", bad, "node", "show"]);
        }
        for (good, port) in [("host", None), ("host:5555", Some(5555))] {
            let cli = parse_cli(&["--node", good, "node", "show"]).unwrap();
            let spec = cli.node.unwrap();
            assert_eq!((spec.host(), spec.port()), ("host", port), "{good}");
        }
    }

    /// The command path `argv` reaches in the hicli tree.
    fn app_path(argv: &[&str]) -> Vec<&'static str> {
        match app().parse(argv.iter().copied()) {
            Ok((_, invocation)) => invocation.path().to_vec(),
            Err(e) => panic!("{argv:?} should parse: {e:?}"),
        }
    }

    #[test]
    fn app_parses_the_smoke_list() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let zip = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(zip.path(), b"PK\x03\x04").expect("write zip magic");
        let zip = zip.path().to_str().expect("utf-8 path").to_string();

        let smoke: Vec<(Vec<&str>, Vec<&str>)> = vec![
            (vec!["ping"], vec!["ping"]),
            (vec!["status"], vec!["status"]),
            (vec!["list"], vec!["list"]),
            (vec!["list", "--limit", "3"], vec!["list"]),
            (vec!["get", "abcd1234"], vec!["get"]),
            (vec!["facts", "abcd1234"], vec!["facts"]),
            (vec!["search"], vec!["search"]),
            (vec!["search", "--artist", "CBL", "jazz"], vec!["search"]),
            (
                vec!["search", "fact-values-for", "genre"],
                vec!["search", "fact-values-for"],
            ),
            (vec!["inbox", "list"], vec!["inbox", "list"]),
            (vec!["inbox", "delete", "a.mp3"], vec!["inbox", "delete"]),
            (vec!["inbox", "ingest", "a.mp3"], vec!["inbox", "ingest"]),
            (vec!["inbox", "ingest-all"], vec!["inbox", "ingest-all"]),
            (vec!["source", "list"], vec!["source", "list"]),
            (vec!["source", "bandcamp", "sync"], vec!["source", "sync"]),
            (
                vec!["source", "bandcamp", "import-playlist", "--dry-run", "slug"],
                vec!["source", "import-playlist"],
            ),
            (vec!["playback", "start"], vec!["playback", "start"]),
            (vec!["playback", "stop"], vec!["playback", "stop"]),
            (vec!["playback", "pause"], vec!["playback", "pause"]),
            (vec!["playback", "resume"], vec!["playback", "resume"]),
            (vec!["playback", "skip"], vec!["playback", "skip"]),
            (vec!["playback", "now"], vec!["playback", "now"]),
            (vec!["playback", "session"], vec!["playback", "session"]),
            (vec!["playback", "outputs"], vec!["playback", "outputs"]),
            (
                vec!["playback", "set-output", "dev"],
                vec!["playback", "set-output"],
            ),
            (
                vec!["playback", "get-output"],
                vec!["playback", "get-output"],
            ),
            (vec!["queue", "next"], vec!["queue", "next"]),
            (vec!["queue", "append", "abcd1234"], vec!["queue", "append"]),
            (vec!["queue", "list"], vec!["queue", "list"]),
            (vec!["queue", "clear"], vec!["queue", "clear"]),
            (vec!["queue", "remove"], vec!["queue", "remove"]),
            (vec!["queue", "replace"], vec!["queue", "replace"]),
            (vec!["queue", "edit"], vec!["queue", "edit"]),
            (vec!["sort", "title", "-a"], vec!["sort"]),
            (vec!["subscribe"], vec!["subscribe"]),
            (
                vec!["upload", "--ssh-key", manifest, zip.as_str()],
                vec!["upload"],
            ),
            (
                vec!["export", "--format", "wav", "--output", "out"],
                vec!["export"],
            ),
            (
                vec!["rekordbox", "export", "--playlist", "set"],
                vec!["rekordbox", "export"],
            ),
            (
                vec!["rekordbox", "import", manifest],
                vec!["rekordbox", "import"],
            ),
            (vec!["playlist", "list"], vec!["playlist", "list"]),
            (vec!["playlist", "get"], vec!["playlist", "get"]),
            (
                vec!["playlist", "contains", "--all"],
                vec!["playlist", "contains"],
            ),
            (vec!["playlist", "new", "set"], vec!["playlist", "new"]),
            (
                vec!["playlist", "append", "set"],
                vec!["playlist", "append"],
            ),
            (
                vec!["playlist", "replace", "set"],
                vec!["playlist", "replace"],
            ),
            (vec!["playlist", "edit", "set"], vec!["playlist", "edit"]),
            (
                vec!["playlist", "remove", "set"],
                vec!["playlist", "remove"],
            ),
            (
                vec!["playlist", "rename", "old", "new"],
                vec!["playlist", "rename"],
            ),
            (
                vec!["library", "reindex-covers"],
                vec!["library", "reindex-covers"],
            ),
            (
                vec!["library", "track", "delete", "abcd1234"],
                vec!["library", "track", "delete"],
            ),
            (
                vec!["library", "track", "restore", "abcd1234"],
                vec!["library", "track", "restore"],
            ),
            (
                vec!["library", "track", "replace", "abcd1234", "/tmp/new.flac"],
                vec!["library", "track", "replace"],
            ),
            (
                vec!["library", "track", "orphans"],
                vec!["library", "track", "orphans"],
            ),
            (
                vec!["library", "identity", "backfill", "--pcm"],
                vec!["library", "identity", "backfill"],
            ),
            (
                vec!["library", "identity", "status"],
                vec!["library", "identity", "status"],
            ),
            (
                vec!["library", "identity", "cancel"],
                vec!["library", "identity", "cancel"],
            ),
            (vec!["library", "dedup"], vec!["library", "dedup"]),
            (
                vec!["library", "dedup", "--apply"],
                vec!["library", "dedup"],
            ),
            (
                vec!["library", "dedup", "merge", "abcd1234", "bcde2345"],
                vec!["library", "dedup", "merge"],
            ),
            (vec!["library", "archive"], vec!["library", "archive"]),
            (
                vec!["library", "archive", "empty", "-y"],
                vec!["library", "archive", "empty"],
            ),
            (vec!["bookmark"], vec!["bookmark"]),
            (
                vec!["bookmark", "--scope", "friday", "abcd1234"],
                vec!["bookmark"],
            ),
            (vec!["bookmarks"], vec!["bookmarks"]),
            (
                vec![
                    "fact", "add", "--field", "year", "--value", "2020", "abcd1234",
                ],
                vec!["fact", "add"],
            ),
            (
                vec![
                    "fact", "retract", "--field", "year", "--value", "2020", "abcd1234",
                ],
                vec!["fact", "retract"],
            ),
            (vec!["view", "--title", "--hash"], vec!["view"]),
            (
                vec!["generate-completions", "bash"],
                vec!["generate-completions"],
            ),
            (
                vec!["admin", "service-mode", "status"],
                vec!["admin", "service-mode", "status"],
            ),
            (
                vec!["admin", "service-mode", "enable"],
                vec!["admin", "service-mode", "enable"],
            ),
            (
                vec!["admin", "service-mode", "disable"],
                vec!["admin", "service-mode", "disable"],
            ),
            (vec!["admin", "boot-health"], vec!["admin", "boot-health"]),
            (vec!["admin", "reboot", "--force"], vec!["admin", "reboot"]),
            (vec!["node", "list"], vec!["node", "list"]),
            (vec!["node", "select", "host"], vec!["node", "select"]),
            (vec!["node", "forget"], vec!["node", "forget"]),
            (
                vec!["--node", "host:5555", "node", "show"],
                vec!["node", "show"],
            ),
        ];
        for (argv, path) in smoke {
            assert_eq!(app_path(&argv), path, "{argv:?}");
        }
    }

    #[test]
    fn app_rejects() {
        let rejected: &[&[&str]] = &[
            &["sort", "title"],
            &["sort", "title", "-a", "-d"],
            &["view"],
            &["playlist", "contains", "--all", "--no"],
            &["export", "--format", "wav", "--lossy-format", "wav"],
            &["library", "archive", "--yes"],
            &["library", "dedup", "merge", "aaaaaaaa"],
            &["search", "--added", "-7"],
            &["get", "xyz"],
            &["bandcamp", "playlists"],
        ];
        for argv in rejected {
            match app().parse(argv.iter().copied()) {
                Ok((_, invocation)) => {
                    panic!("{argv:?} should be rejected, got {:?}", invocation.path())
                }
                Err(e) => assert_eq!(hicli::exit_code_for(&e), 2, "{argv:?}"),
            }
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
