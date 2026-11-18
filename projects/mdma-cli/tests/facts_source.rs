//! Where the reads of `mdma` come from (ADR-030): the local replica by default, the
//! node under `--facts=node`, and what each says on stdout and stderr.
//!
//! These run the real `mdma` against a replica in a temp cache and a node that is not
//! there (`127.0.0.1:1` refuses at once). They spawn, so each holds `env_lock` (#107:
//! a child forked while another test holds a sync flock would inherit it).
#![cfg(unix)]

use std::process::{Command, Output};

use client_facts::test_support::{asserted, env_lock, nodes_dir, replica_with_lines_at};

const BLUE: &str = "sha256:aaaa000000000001";
const ATOMIC: &str = "sha256:cccc000000000003";
const AT: &str = "2026-01-02T10:00:00Z";
/// Nothing listens here, and nothing is resolved to reach it.
const DEAD_NODE: &str = "127.0.0.1:1";

fn field(entity: &str, name: &str, value: &str) -> String {
    asserted(entity, &format!(r#"{{"t":"{name}","v":"{value}"}}"#), AT)
}

fn catalog() -> Vec<String> {
    vec![
        field(BLUE, "Title", "Blue Monday"),
        field(BLUE, "Artist", "New Order"),
        field(ATOMIC, "Title", "Atomic"),
        field(ATOMIC, "Artist", "Blondie"),
    ]
}

/// Runs `mdma` with a replica of the catalog under a cache of its own for every run.
/// The replica is fresh, so a run spawns no detached sync that would outlive the test.
struct Lab {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Lab {
    fn new() -> Self {
        Lab { _lock: env_lock() }
    }

    /// `mdma --node DEAD_NODE args...`, with `env` set on top of a clean environment.
    fn mdma(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.mdma_node(DEAD_NODE, args, env)
    }

    /// [`Lab::mdma`] naming `node` (the replica is of 127.0.0.1 only).
    fn mdma_node(&self, node: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let dir = tempfile::tempdir().expect("temp dir");
        // Synced just now, so the throttle declines and no detached sync is spawned.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs();
        replica_with_lines_at(&nodes_dir(dir.path()), "stage", "127.0.0.1", catalog(), now);
        let mut command = Command::new(env!("CARGO_BIN_EXE_mdma"));
        command
            .args(["--node", node])
            .args(args)
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env_remove("MDMA_NODE")
            .env_remove("MDMA_FACTS")
            .env_remove("MDMA_LIBRARY_SOCKET")
            .env_remove("MDMA_PLAYBACK_SOCKET");
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().expect("ran mdma")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// One line, and the note of the replica of `stage`.
fn assert_one_replica_note(stderr: &str) {
    assert_eq!(stderr.lines().count(), 1, "one note: {stderr:?}");
    assert!(
        stderr.starts_with("replica of stage, synced "),
        "the note: {stderr:?}"
    );
}

#[test]
fn search_get_and_facts_answer_from_the_replica_with_the_node_dead() {
    let lab = Lab::new();
    let cases: [(&[&str], &str); 3] = [
        (&["search", "--artist", "New Order"], "Blue Monday"),
        (&["get", "aaaa0000"], "Title:    Blue Monday"),
        (&["facts", "aaaa0000"], "Blue Monday"),
    ];
    for (args, expected) in cases {
        let output = lab.mdma(args, &[]);
        let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));

        assert!(output.status.success(), "{args:?}: {stderr}");
        assert!(stdout.contains(expected), "{args:?}: {stdout:?}");
        assert!(!stdout.contains("replica of"), "{args:?}: {stdout:?}");
        assert_one_replica_note(&stderr);
    }
}

#[test]
fn a_node_command_with_the_node_dead_says_which_command_needed_it() {
    let lab = Lab::new();

    let output = lab.mdma(&["queue", "list"], &[]);

    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("mdma queue list needs the node"),
        "{stderr}"
    );
    assert!(!stderr.contains("replica of stage"), "no note: {stderr}");
}

#[test]
fn facts_node_with_the_node_dead_fails_naming_the_replica_and_answers_nothing() {
    let lab = Lab::new();

    let output = lab.mdma(&["--facts=node", "search"], &[]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--facts=replica answers from the local replica (synced "),
        "{stderr}"
    );
    assert!(stderr.contains("search needs the node"), "{stderr}");
    assert!(stdout.is_empty(), "nothing from the replica: {stdout:?}");
    assert!(!stderr.contains("replica of stage"), "no note: {stderr}");
}

#[test]
fn mdma_facts_is_the_flag_by_other_means_and_the_flag_wins() {
    let lab = Lab::new();

    let by_env = lab.mdma(&["search"], &[("MDMA_FACTS", "node")]);
    let overridden = lab.mdma(&["--facts=replica", "search"], &[("MDMA_FACTS", "node")]);

    let (by_env_err, overridden_err) = (text(&by_env.stderr), text(&overridden.stderr));
    assert_eq!(by_env.status.code(), Some(1), "{by_env_err}");
    assert!(
        by_env_err.contains("--facts=replica answers"),
        "{by_env_err}"
    );
    assert!(overridden.status.success(), "{overridden_err}");
    assert_one_replica_note(&overridden_err);
}

#[test]
fn a_refusal_from_the_replica_still_says_where_it_came_from() {
    let lab = Lab::new();

    let output = lab.mdma(&["get", "dddd0000"], &[]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(stderr.contains("track not found: dddd0000"), "{stderr}");
    assert_eq!(
        stderr
            .lines()
            .filter(|l| l.starts_with("replica of stage"))
            .count(),
        1,
        "one note: {stderr}"
    );
}

#[test]
fn a_replica_that_does_not_open_gets_no_note() {
    let lab = Lab::new();

    // Another node's name: no replica of it.
    let output = lab.mdma_node("127.0.0.2:1", &["search"], &[]);

    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no replica for '127.0.0.2'"), "{stderr}");
    assert!(!stderr.contains("replica of stage"), "{stderr}");
}

#[test]
fn rekordbox_import_matches_against_the_node_not_the_replica() {
    let lab = Lab::new();
    let xml = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(
        xml.path(),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <PRODUCT Name="rekordbox" Version="6.0.0" Company="AlphaTheta"/>
  <COLLECTION Entries="1">
    <TRACK TrackID="1" Name="Blue Monday" Artist="New Order" TotalTime="400" Location="file://localhost/x/blue.mp3"/>
  </COLLECTION>
  <PLAYLISTS><NODE Type="0" Name="ROOT" Count="0"/></PLAYLISTS>
</DJ_PLAYLISTS>
"#,
    )
    .expect("wrote the xml");
    let path = xml.path().to_str().expect("utf-8 path");

    let output = lab.mdma(&["rekordbox", "import", path], &[]);

    // The replica knows "Blue Monday", the node is dead: asking the replica would
    // match it, and a playlist built from a stale replica could replace a good one.
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("needs the node"), "{stderr}");
    assert!(!stdout.contains("Matched"), "{stdout}");
    assert!(!stderr.contains("replica of stage"), "{stderr}");
}

#[test]
fn an_empty_mdma_facts_counts_as_unset() {
    let lab = Lab::new();

    let output = lab.mdma(&["search"], &[("MDMA_FACTS", "")]);

    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_one_replica_note(&stderr);
}

#[test]
fn a_bad_facts_source_is_a_usage_error_from_the_flag_and_from_the_variable() {
    let lab = Lab::new();

    let flag = lab.mdma(&["--facts=cache", "search"], &[]);
    let variable = lab.mdma(&["search"], &[("MDMA_FACTS", "cache")]);

    for output in [flag, variable] {
        // bpaf wraps long lines: compare the words.
        let stderr = text(&output.stderr)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains("unknown facts source 'cache'; use replica or node"),
            "{stderr}"
        );
        assert!(text(&output.stdout).is_empty());
    }
}
