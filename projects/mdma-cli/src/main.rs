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
/// # Examples
///
/// ```sh
/// mdma rekordbox export --playlist my-set
/// mdma search --artist CBL | mdma rekordbox export
/// ```
#[hicli::group]
mod rekordbox;
/// Search for tracks
///
/// Filter with options such as --artist, --bpm or --key, or give a free-text query
/// that is applied to all text fields. Give `-` to read hashes from stdin and keep
/// only the tracks among them (stdin is read only for `-`; no hashes on stdin matches
/// nothing, or everything with --not); --not inverts the result.
///
/// # Examples
///
/// ```sh
/// mdma search --bpm 120..130
/// mdma search --bpm 120..130 | mdma mark -
/// mdma playlist get friday | mdma search --genre=Techno -
/// ```
#[hicli::group]
mod search;
/// Music source management
///
/// Per-source commands: mdma source <SOURCE-NAME> <COMMAND>
///
/// Run 'mdma source <SOURCE-NAME> --help' for the commands.
/// 'list' is reserved and cannot be a source name.
///
/// # Examples
///
/// ```sh
/// mdma source list
/// mdma source bandcamp sync
/// ```
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

    /// Stdin is read only when `-` is given, by every command that reads it: each of
    /// these is a usage error without the `-`, and parses with it.
    #[test]
    fn stdin_only_commands_need_a_dash() {
        let cases: [&[&str]; 9] = [
            &["sort", "title", "-a"],
            &["view", "--hash"],
            &["queue", "replace"],
            &["export", "--format", "wav"],
            &["playlist", "contains"],
            &["playlist", "contains", "--all"],
            &["playlist", "new", "set"],
            &["playlist", "append", "set"],
            &["playlist", "replace", "set"],
        ];
        for argv in cases {
            let err = usage_error(argv);
            assert!(err.contains('-'), "{argv:?}: {err}");
            let mut with_dash = argv.to_vec();
            with_dash.push("-");
            parses(&with_dash);
        }
    }

    /// Every command that takes track hashes accepts `-` for hashes on stdin.
    #[test]
    fn hash_commands_accept_a_dash_for_stdin() {
        let cases: [&[&str]; 8] = [
            &["get", "-"],
            &["facts", "-"],
            &["library", "track", "delete", "-"],
            &["library", "track", "restore", "-"],
            &["fact", "add", "--field", "title", "--value", "x", "-"],
            &["fact", "retract", "--field", "title", "--value", "x", "-"],
            &["fact", "add", "-", "--field", "title", "--value", "x"],
            &["queue", "remove", "-"],
        ];
        for argv in cases {
            parses(argv);
        }
        // Still a hash, still validated.
        usage_error(&["get", "xyz"]);
        // Nothing at all is a usage error that names both spellings.
        let err = usage_error(&["get"]);
        assert!(err.contains("<HASH|->"), "{err}");
    }

    fn given(args: &[&str]) -> bool {
        let (globals, _) = parse_cli(args).expect("valid args");
        Ctx::with_target(globals, node_discovery::Target::LocalIpc).sockets_given()
    }

    #[test]
    fn explicit_sockets_are_detected_from_flags_and_environment() {
        // One test, because it touches process-wide environment variables.
        let _env = crate::test_support::env_lock();
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
        std::env::set_var("MDMA_LIBRARY_SOCKET", "");
        assert!(
            !given(&["ping"]),
            "an empty variable is unset, like MDMA_NODE"
        );
        std::env::remove_var("MDMA_LIBRARY_SOCKET");

        std::env::set_var("MDMA_PLAYBACK_SOCKET", "ipc:///tmp/pb.sock");
        assert!(given(&["ping"]));
        std::env::set_var("MDMA_PLAYBACK_SOCKET", "");
        assert!(
            !given(&["ping"]),
            "an empty variable is unset, like MDMA_NODE"
        );
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
        assert!(globals.node.0.is_none());
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
            let spec = globals.node.0.unwrap();
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
            (vec!["queue", "next", "-"], vec!["queue", "next"]),
            (vec!["queue", "append", "abcd1234"], vec!["queue", "append"]),
            (vec!["queue", "list"], vec!["queue", "list"]),
            (vec!["queue", "clear"], vec!["queue", "clear"]),
            (vec!["queue", "remove", "-"], vec!["queue", "remove"]),
            (vec!["queue", "replace", "-"], vec!["queue", "replace"]),
            (vec!["queue", "edit"], vec!["queue", "edit"]),
            (vec!["sort", "title", "-a", "-"], vec!["sort"]),
            (vec!["subscribe"], vec!["subscribe"]),
            (
                vec!["upload", "--ssh-key", manifest, zip.as_str()],
                vec!["upload"],
            ),
            (
                vec!["export", "--format", "wav", "--output", "out", "-"],
                vec!["export"],
            ),
            (
                vec!["rekordbox", "export", "--playlist", "set"],
                vec!["rekordbox", "export"],
            ),
            (
                vec!["rekordbox", "export", "-"],
                vec!["rekordbox", "export"],
            ),
            (
                vec!["rekordbox", "import", manifest],
                vec!["rekordbox", "import"],
            ),
            (vec!["playlist", "list"], vec!["playlist", "list"]),
            (vec!["playlist", "get", "-"], vec!["playlist", "get"]),
            (
                vec!["playlist", "contains", "--all", "-"],
                vec!["playlist", "contains"],
            ),
            (vec!["playlist", "new", "set", "-"], vec!["playlist", "new"]),
            (
                vec!["playlist", "append", "set", "-"],
                vec!["playlist", "append"],
            ),
            (
                vec!["playlist", "replace", "set", "-"],
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
            (vec!["view", "--title", "--hash", "-"], vec!["view"]),
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
            &["sort", "title", "-"],
            &["sort", "title", "-a", "-d", "-"],
            &["view", "-"],
            &["playlist", "contains", "--all", "--no", "-"],
            &["export", "--format", "wav", "--lossy-format", "wav", "-"],
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
        let (target, _, _guard) = crate::test_support::dead_node();
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

    fn complete(words: &[&str]) -> Vec<String> {
        app().complete(words.iter().copied())
    }

    #[test]
    fn node_select_completes_the_remembered_node() {
        use node_discovery::{cache, NodeCache};
        // One test, because it touches a process-wide environment variable.
        let _env = crate::test_support::env_lock();
        let config = tempfile::tempdir().expect("temp dir");
        let original = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", config.path());

        let empty = complete(&["node", "select", ""]);
        let node = NodeCache {
            id: "abc123".into(),
            name: "mdma-johlyroger".into(),
            host: "mdma-johlyroger.local".into(),
            ip: std::net::Ipv4Addr::new(192, 168, 0, 9),
            port: 5555,
        };
        cache::save(&cache::default_path().expect("config dir"), &node).expect("saved");
        let all = complete(&["node", "select", ""]);
        let by_prefix = complete(&["node", "select", "mdma-j"]);
        let none = complete(&["node", "select", "zzz"]);
        let flag = complete(&["--node", "mdma-johlyroger.l"]);

        match original {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        assert!(empty.is_empty(), "no remembered node: {empty:?}");
        for offered in ["mdma-johlyroger", "abc123", "mdma-johlyroger.local"] {
            assert!(all.iter().any(|c| c == offered), "{offered} in {all:?}");
        }
        assert_eq!(by_prefix.len(), 2, "{by_prefix:?}");
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(flag, ["mdma-johlyroger.local"]);
    }

    #[test]
    fn fact_retract_origin_and_subscribe_topic_complete_their_known_values() {
        assert_eq!(
            complete(&["fact", "retract", "--origin", ""]),
            ["User", "Rekordbox", "Upload"]
        );
        assert_eq!(
            complete(&["fact", "retract", "--origin", "Re"]),
            ["Rekordbox"]
        );
        assert_eq!(
            complete(&["subscribe", "--topic", "playback/track_s"]),
            ["playback/track_started", "playback/track_stopped"]
        );
    }

    #[test]
    fn source_names_complete_from_the_local_sources_directory() {
        let _env = crate::test_support::env_lock();
        let dir = tempfile::tempdir().expect("temp dir");
        for file in ["bandcamp.sock", "beatport.sock", "notes.txt"] {
            std::fs::write(dir.path().join(file), b"").expect("file");
        }
        let original = std::env::var_os("MDMA_SOURCES_DIR");
        std::env::set_var("MDMA_SOURCES_DIR", dir.path());
        let by_prefix = complete(&["source", "b"]);
        let narrowed = complete(&["source", "ban"]);
        match original {
            Some(value) => std::env::set_var("MDMA_SOURCES_DIR", value),
            None => std::env::remove_var("MDMA_SOURCES_DIR"),
        }
        assert!(by_prefix.contains(&"bandcamp".to_string()), "{by_prefix:?}");
        assert!(by_prefix.contains(&"beatport".to_string()), "{by_prefix:?}");
        assert!(!by_prefix.contains(&"notes".to_string()), "{by_prefix:?}");
        assert!(narrowed.contains(&"bandcamp".to_string()), "{narrowed:?}");
        assert!(!narrowed.contains(&"beatport".to_string()), "{narrowed:?}");
    }

    #[test]
    fn examples_are_footers_not_details() {
        for argv in [
            &["rekordbox", "--help"][..],
            &["source", "--help"],
            &["search", "fact-values-for", "--help"],
            &["export", "--help"],
        ] {
            let text = help(argv);
            assert!(text.contains("Examples:"), "{argv:?}: {text}");
            assert!(!text.contains("```"), "{argv:?}: {text}");
        }
    }
}
