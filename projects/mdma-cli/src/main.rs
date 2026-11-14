//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to services via gateway (single address) or direct IPC.

#![deny(clippy::exit)]

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
mod origins;
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

use ctx::Ctx;

// The hicli command tree.
fn app() -> hicli::App<ctx::Globals, ctx::Ctx> {
    hicli::app! {
        globals: ctx::globals(),
        commands: [
            tracks::ping, tracks::status, tracks::list, tracks::get, tracks::facts,
            search, inbox, source, playback, queue, tracks::sort, subscribe::subscribe,
            transfer::upload, transfer::export, rekordbox, playlist, library,
            tracks::mark, tracks::unmark, tracks::marked, fact, tracks::view,
            completions::generate_completions, admin, node,
        ]
    }
    // Fixed, so usage reads `mdma ...` however the binary was invoked (a
    // symlink, `cargo test`) and the tests see what `run` prints.
    .bin_name("mdma")
    .about("MDMA CLI - Control the music services")
    .version(env!("CARGO_PKG_VERSION"))
}

fn main() -> std::process::ExitCode {
    // Rust ignores SIGPIPE, so printing to a closed pipe (`mdma ... | head`)
    // would panic. Restore the default so the process ends quietly instead.
    #[cfg(unix)]
    // SAFETY: SIG_DFL installs no handler, and no other thread exists yet to race the change.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    color_eyre::install().expect("installed once");

    // The node is resolved once, on first use, so only commands that talk to
    // one resolve it.
    app().run(|globals| Ok(Ctx::new(globals)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{help, parse, parse_cli, parses, usage_error};

    fn given(args: &[&str]) -> bool {
        let (globals, _) = parse_cli(args).expect("valid args");
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
    fn node_flag_does_not_read_the_environment() {
        // MDMA_NODE is read separately so --node and the env var can be told apart.
        // One test, because it touches a process-wide environment variable (which
        // devenv sets, so it is put back).
        let _env = crate::test_support::env_lock();
        let original = std::env::var_os("MDMA_NODE");
        std::env::set_var("MDMA_NODE", "envhost");
        let (globals, _) = parse_cli(&["node", "show"]).expect("valid args");
        match original {
            Some(value) => std::env::set_var("MDMA_NODE", value),
            None => std::env::remove_var("MDMA_NODE"),
        }
        assert!(globals.node.is_none());
    }

    #[test]
    fn hash_arguments_are_validated_at_parse_time() {
        for cmd in ["get", "facts"] {
            for bad in ["abcd123", "ABCD1234", "sha256:abc", "xyz"] {
                let e = usage_error(&[cmd, bad]);
                assert!(e.contains("not a content hash"), "{cmd} {bad}: {e}");
            }
            for good in ["abcd1234", "sha256:abcd1234", "0123456789abcdef"] {
                assert!(parse(&[cmd, good]).is_ok(), "{cmd} {good}");
            }
        }
    }

    #[test]
    fn node_flag_is_checked_at_parse_time() {
        for bad in ["host:notaport", "host:0", "host:99999", "", ":5555"] {
            usage_error(&["--node", bad, "node", "show"]);
        }
        for (good, port) in [("host", None), ("host:5555", Some(5555))] {
            let (globals, _) = parse_cli(&["--node", good, "node", "show"]).unwrap();
            let spec = globals.node.unwrap();
            assert_eq!((spec.host(), spec.port()), ("host", port), "{good}");
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
            (vec!["mark"], vec!["mark"]),
            (vec!["mark", "banger"], vec!["mark"]),
            (vec!["mark", "liked"], vec!["mark"]),
            (vec!["unmark"], vec!["unmark"]),
            (vec!["unmark", "banger"], vec!["unmark"]),
            (vec!["marked"], vec!["marked"]),
            (vec!["marked", "banger"], vec!["marked"]),
            (vec!["bookmark"], vec!["mark"]),
            (vec!["bookmark", "banger"], vec!["mark"]),
            (vec!["bookmarks"], vec!["marked"]),
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
            assert_eq!(parses(&argv), path, "{argv:?}");
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
            usage_error(argv);
        }
    }

    #[test]
    fn run_exits_zero_for_version_and_two_for_any_parse_failure() {
        let run = |args: &[&str]| {
            app().run_with_args(args.iter().copied(), |globals| {
                Ok(Ctx::with_target(globals, node_discovery::Target::LocalIpc))
            })
        };
        assert_eq!(run(&["--version"]), 0);
        assert_eq!(run(&["get", "xyz"]), 2);
        assert_eq!(run(&["frobnicate"]), 2);
    }

    #[test]
    fn run_exits_one_when_the_service_cannot_be_reached() {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = format!("ipc://{}/none.sock", dir.path().display());
        let code = app().run_with_args(["--socket", socket.as_str(), "ping"], |globals| {
            Ok(Ctx::with_target(globals, node_discovery::Target::LocalIpc))
        });
        assert_eq!(code, 1);
    }

    #[test]
    fn run_exits_one_when_an_admin_command_cannot_reach_the_node() {
        let (target, _) = crate::test_support::dead_node();
        let code = app().run_with_args(["admin", "service-mode", "status"], move |globals| {
            Ok(Ctx::with_target(globals, target))
        });
        assert_eq!(code, 1);
    }

    #[test]
    fn version_flag_reports_the_package_version() {
        // run_with_args prints to the real stdout, so the text is read from the
        // parse outcome.
        assert_eq!(
            help(&["--version"]).trim(),
            format!("Version: {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn help_lists_the_commands_but_not_hidden_ones() {
        let top = help(&["--help"]);
        for command in ["ping", "search", "source", "library", "node"] {
            assert!(top.contains(command), "missing {command} in {top}");
        }
        assert!(!top.contains("generate-completions"), "hidden: {top}");
        assert!(!top.contains("bookmark"), "alias shown: {top}");
    }
}
