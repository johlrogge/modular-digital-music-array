//! Completion from the local replica (ADR-021 §4 and §6).
//!
//! hicli completes on every keystroke and hands over only the raw word, so nothing
//! here may block: the completion functions read `completion.json` from the replica
//! directory and nothing else, never the network. Which replica is decided offline
//! ([`replica_from_environment`]); an explicit `--node` is not visible to completion.
//!
//! Every call also asks for a background refresh, which the throttle in
//! [`request_sync`] turns into at most one spawn per 30 s (backed off after
//! failures). With no replica yet a completion offers nothing, and the same ask
//! starts the first sync.

use std::path::Path;

use fact_replica::{NodeId, Replica, UnixSecs};
use hicli::Candidate;

use crate::background::{request_sync, spawn_background_sync, Urgency};
use crate::replica::{replica_from_environment, replicas_dir, Probed};
use crate::snapshot::CompletionSnapshot;

/// A list of names in the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Marks,
    Artists,
    Titles,
    Albums,
    Labels,
    Genres,
    Styles,
    Origins,
    Playlists,
    Sources,
}

impl Field {
    fn names(self, snapshot: &CompletionSnapshot) -> &[String] {
        match self {
            Field::Marks => &snapshot.marks,
            Field::Artists => &snapshot.artists,
            Field::Titles => &snapshot.titles,
            Field::Albums => &snapshot.albums,
            Field::Labels => &snapshot.labels,
            Field::Genres => &snapshot.genres,
            Field::Styles => &snapshot.styles,
            Field::Origins => &snapshot.origins,
            Field::Playlists => names_of(&snapshot.probe.playlists),
            Field::Sources => names_of(&snapshot.probe.sources),
        }
    }

    /// What a candidate of this field is, shown beside it when a completion mixes fields.
    fn hint(self) -> Option<&'static str> {
        match self {
            Field::Titles => Some("title"),
            Field::Artists => Some("artist"),
            _ => None,
        }
    }
}

fn names_of(probed: &Option<Probed>) -> &[String] {
    probed.as_ref().map_or(&[], |p| p.names.as_slice())
}

/// The names in `fields` that start with `typed`, ignoring case, in the order of
/// `fields` and then sorted; every match, each value once. The shell pages or
/// prompts for long lists itself.
///
/// The shell inserts the value, and shows the hint only as a description, so a title
/// completes as the search term it is, not as the track it names (ADR-021 §7).
fn candidates(snapshot: &CompletionSnapshot, fields: &[Field], typed: &str) -> Vec<Candidate> {
    let typed = typed.to_lowercase();
    let mixed = fields.len() > 1;
    let mut out: Vec<Candidate> = Vec::new();
    for field in fields {
        for name in field.names(snapshot) {
            if name.to_lowercase().starts_with(&typed) && !out.iter().any(|(v, _)| v == name) {
                let hint = if mixed { field.hint() } else { None };
                out.push((name.clone(), hint.map(str::to_string)));
            }
        }
    }
    out
}

/// The snapshot of `replica` for a completion, with the probed names that have not
/// expired; `None` without a replica or a snapshot. Asks for a background refresh
/// through `spawn` (see [`request_sync`]).
fn snapshot_for(
    nodes_dir: &Path,
    replica: Option<&NodeId>,
    now: UnixSecs,
    spawn: &mut dyn FnMut(),
) -> Option<CompletionSnapshot> {
    let snapshot = replica
        .and_then(|id| CompletionSnapshot::load(&Replica::new(nodes_dir, id)))
        .map(|snapshot| snapshot.live(now));
    request_sync(nodes_dir, replica, now, Urgency::WhenStale, spawn);
    snapshot
}

/// The snapshot of the replica for the node in use, found offline.
fn current_snapshot() -> Option<CompletionSnapshot> {
    let nodes_dir = replicas_dir()?;
    let id = replica_from_environment(&nodes_dir, None);
    // A failed spawn only means no refresh this time; completion must not fail.
    let mut spawn = || {
        let _ = spawn_background_sync(None);
    };
    snapshot_for(&nodes_dir, id.as_ref(), UnixSecs::now(), &mut spawn)
}

fn complete(fields: &[Field], typed: &str) -> Vec<Candidate> {
    current_snapshot()
        .map(|snapshot| candidates(&snapshot, fields, typed))
        .unwrap_or_default()
}

/// Mark names, `liked` included.
pub(crate) fn complete_marks(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Marks], typed)
}

/// Playlist names, as probed at the last sync.
pub(crate) fn complete_playlists(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Playlists], typed)
}

/// Source names, as probed at the last sync.
pub(crate) fn complete_sources(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Sources], typed)
}

/// Origin kinds found in the facts.
pub(crate) fn complete_origins(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Origins], typed)
}

/// `offered`, then the candidates of `more` whose value it does not have yet. How
/// a fixed list of known values and the ones found in the replica are combined.
pub(crate) fn merge_unlisted(mut offered: Vec<Candidate>, more: Vec<Candidate>) -> Vec<Candidate> {
    for candidate in more {
        if !offered.iter().any(|(value, _)| *value == candidate.0) {
            offered.push(candidate);
        }
    }
    offered
}

pub(crate) fn complete_artists(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Artists], typed)
}

pub(crate) fn complete_titles(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Titles], typed)
}

pub(crate) fn complete_albums(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Albums], typed)
}

pub(crate) fn complete_labels(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Labels], typed)
}

pub(crate) fn complete_genres(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Genres], typed)
}

pub(crate) fn complete_styles(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Styles], typed)
}

/// A free-text search query: titles and artists, as search terms.
pub(crate) fn complete_query(typed: &str) -> Vec<Candidate> {
    complete(&[Field::Titles, Field::Artists], typed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica::{Probe, Probed};
    use crate::test_support::{env_lock, EnvVars};
    use fact_replica::test_support::FakeNode;
    use fact_replica::{Host, LineCount};
    use pretty_assertions::assert_eq;

    fn secs(n: u64) -> UnixSecs {
        UnixSecs::new(n)
    }

    fn id(s: &str) -> NodeId {
        NodeId::new(s).unwrap()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn values(candidates: Vec<Candidate>) -> Vec<String> {
        candidates.into_iter().map(|(value, _)| value).collect()
    }

    fn snapshot() -> CompletionSnapshot {
        snapshot_probed_at(100)
    }

    fn snapshot_probed_at(at: u64) -> CompletionSnapshot {
        CompletionSnapshot {
            lines: LineCount::new(10),
            marks: strings(&["banger", "liked", "opener"]),
            artists: strings(&["CBL", "Carbon Based Lifeforms", "deadmau5"]),
            titles: strings(&["Blue Monday", "Strobe", "carbon"]),
            albums: strings(&["Power"]),
            labels: strings(&["Factory"]),
            genres: strings(&["Techno"]),
            styles: strings(&["Acid"]),
            origins: strings(&["Bandcamp", "User"]),
            unreadable: 0,
            probe: Probe {
                playlists: Some(Probed {
                    at: secs(at),
                    names: strings(&["friday", "saturday"]),
                }),
                sources: Some(Probed {
                    at: secs(at),
                    names: strings(&["bandcamp", "beatport"]),
                }),
            },
        }
    }

    // --- matching ------------------------------------------------------------------

    #[test]
    fn a_prefix_matches_ignoring_case_and_keeps_the_stored_spelling() {
        let s = snapshot();
        let artists = |typed: &str| values(candidates(&s, &[Field::Artists], typed));
        for typed in ["ca", "CA", "Ca"] {
            assert_eq!(artists(typed), ["Carbon Based Lifeforms"], "{typed}");
        }
        assert_eq!(artists("c"), ["CBL", "Carbon Based Lifeforms"]);
        assert_eq!(
            artists(""),
            ["CBL", "Carbon Based Lifeforms", "deadmau5"],
            "an empty word offers everything"
        );
        assert!(artists("zzz").is_empty());
        assert!(artists("lifeforms").is_empty(), "a prefix only");
    }

    #[test]
    fn matching_folds_non_ascii_case() {
        let s = CompletionSnapshot {
            artists: strings(&["Ärzte", "Östen"]),
            ..CompletionSnapshot::default()
        };
        let artists = |typed: &str| values(candidates(&s, &[Field::Artists], typed));
        assert_eq!(artists("är"), ["Ärzte"]);
        assert_eq!(artists("ÖS"), ["Östen"]);
    }

    #[test]
    fn every_match_is_offered_however_many() {
        let titles: Vec<String> = (0..300).map(|i| format!("title {i:04}")).collect();
        let artists: Vec<String> = (0..300).map(|i| format!("artist {i:04}")).collect();
        let s = CompletionSnapshot {
            titles: titles.clone(),
            artists: artists.clone(),
            ..CompletionSnapshot::default()
        };

        let offered = values(candidates(&s, &[Field::Titles, Field::Artists], ""));

        let expected: Vec<String> = titles.into_iter().chain(artists).collect();
        assert_eq!(offered, expected, "titles, then the artists after them");
    }

    #[test]
    fn a_query_offers_titles_and_artists_once_each_with_a_hint_of_which() {
        let s = snapshot();

        let offered = candidates(&s, &[Field::Titles, Field::Artists], "ca");

        let hinted = |value: &str, hint: &str| (value.to_string(), Some(hint.to_string()));
        let expected = [
            hinted("carbon", "title"),
            hinted("Carbon Based Lifeforms", "artist"),
        ];
        assert_eq!(offered, expected);
        let same = CompletionSnapshot {
            titles: strings(&["Strobe"]),
            artists: strings(&["Strobe"]),
            ..CompletionSnapshot::default()
        };
        assert_eq!(
            candidates(&same, &[Field::Titles, Field::Artists], "s").len(),
            1,
            "a title that is also an artist is offered once"
        );
        assert_eq!(
            candidates(&s, &[Field::Titles], "st"),
            [("Strobe".to_string(), None)],
            "no hint when the field is the only one"
        );
    }

    #[test]
    fn every_field_reads_its_own_list() {
        let s = snapshot();
        let first = |field| values(candidates(&s, &[field], ""));
        assert_eq!(first(Field::Marks), ["banger", "liked", "opener"]);
        assert_eq!(first(Field::Albums), ["Power"]);
        assert_eq!(first(Field::Labels), ["Factory"]);
        assert_eq!(first(Field::Genres), ["Techno"]);
        assert_eq!(first(Field::Styles), ["Acid"]);
        assert_eq!(first(Field::Origins), ["Bandcamp", "User"]);
        assert_eq!(first(Field::Playlists), ["friday", "saturday"]);
        assert_eq!(first(Field::Sources), ["bandcamp", "beatport"]);
    }

    #[test]
    fn found_values_are_added_after_the_known_ones_without_repeating_them() {
        let known = vec![("User".to_string(), Some("manual edits".to_string()))];
        let found = vec![("Bandcamp".to_string(), None), ("User".to_string(), None)];

        let merged = merge_unlisted(known, found);

        let expected = [
            ("User".to_string(), Some("manual edits".to_string())),
            ("Bandcamp".to_string(), None),
        ];
        assert_eq!(merged, expected);
    }

    // --- finding the snapshot, and asking for a refresh -----------------------------------

    fn replica_with_snapshot(dir: &Path, name: &str, snapshot: &CompletionSnapshot) -> Replica {
        let replica = Replica::new(dir, &id(name));
        let node = FakeNode::new("e1", 0);
        let host = Host::new("mdma-test.local");
        let mut read = |c, l, p| node.serve(c, l, p);
        replica.sync(secs(1_000), Some(&host), &mut read).unwrap();
        let json = serde_json::to_vec(snapshot).unwrap();
        std::fs::write(replica.dir().join("completion.json"), json).unwrap();
        replica
    }

    fn ask(
        dir: &Path,
        replica: Option<&NodeId>,
        now: u64,
        spawns: &mut u32,
    ) -> Option<CompletionSnapshot> {
        snapshot_for(dir, replica, secs(now), &mut || *spawns += 1)
    }

    #[test]
    fn with_no_replica_a_completion_offers_nothing_and_starts_a_sync() {
        let dir = tempfile::tempdir().unwrap();
        let mut spawns = 0;

        let unsynced = id("not-synced-yet");

        let found = ask(dir.path(), None, 5_000, &mut spawns);
        let named = ask(dir.path(), Some(&unsynced), 5_000, &mut spawns);

        // `complete` offers nothing without a snapshot.
        assert_eq!(found, None);
        assert_eq!(named, None);
        assert_eq!(spawns, 2, "one spawn each");
    }

    #[test]
    fn a_completion_spawns_a_refresh_at_most_once_per_interval() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_snapshot(dir.path(), "n", &snapshot());
        let mut spawns = 0;
        let n = id("n");

        // The replica was synced at 1000: fresh for 30 s, however fast one types.
        for now in [1_000, 1_005, 1_010, 1_029] {
            assert!(ask(dir.path(), Some(&n), now, &mut spawns).is_some());
        }
        assert_eq!(spawns, 0);
        // Stale: one spawn, and the keystrokes after it do not spawn more.
        for now in [1_030, 1_031, 1_032, 1_033] {
            assert!(ask(dir.path(), Some(&n), now, &mut spawns).is_some());
        }
        assert_eq!(spawns, 1);
        ask(dir.path(), Some(&n), 1_060, &mut spawns);
        assert_eq!(spawns, 2);
    }

    #[test]
    fn the_snapshot_is_served_without_the_expired_probe() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_snapshot(dir.path(), "n", &snapshot());
        let mut spawns = 0;
        let day = 24 * 60 * 60;

        let fresh = ask(dir.path(), Some(&id("n")), 100 + day - 1, &mut spawns).unwrap();
        let stale = ask(dir.path(), Some(&id("n")), 100 + day, &mut spawns).unwrap();

        assert!(fresh.probe.playlists.is_some());
        assert_eq!(stale.probe, Probe::default());
        assert_eq!(stale.marks, fresh.marks, "the facts' names do not expire");
    }

    // --- the real thing: through the command tree ------------------------------------------

    fn complete_words(words: &[&str]) -> Vec<String> {
        crate::app().complete(words.iter().copied())
    }

    /// A cache with one replica for `mdma-test.local`, and an environment that points
    /// completion at it and at nothing else (the real cache and node file are not read).
    fn offline_environment(dir: &Path) -> EnvVars {
        let nodes_dir = dir.join("cache").join("mdma").join("nodes");
        std::fs::create_dir_all(&nodes_dir).unwrap();
        let fresh = snapshot_probed_at(UnixSecs::now().get());
        replica_with_snapshot(&nodes_dir, "n", &fresh);
        let mut env = EnvVars::default();
        env.set("XDG_CACHE_HOME", dir.join("cache"));
        env.set("XDG_CONFIG_HOME", dir.join("config"));
        env.set("MDMA_NODE", "mdma-test.local");
        env.set("MDMA_LIBRARY_SOCKET", "");
        env.set("MDMA_PLAYBACK_SOCKET", "");
        env
    }

    #[test]
    fn the_arguments_complete_from_the_local_replica() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _env = offline_environment(dir.path());

        let cases: [(&[&str], &[&str]); 22] = [
            // marks
            (&["mark", ""], &["banger", "liked", "opener"]),
            (&["mark", "BA"], &["banger"]),
            (&["unmark", "o"], &["opener"]),
            (&["marked", "l"], &["liked"]),
            // playlists
            (&["playlist", "get", "f"], &["friday"]),
            (&["playlist", "append", "sat"], &["saturday"]),
            (&["playlist", "rename", "f"], &["friday"]),
            // TO names a new playlist: nothing to offer
            (&["playlist", "rename", "friday", "s"], &[]),
            (&["playlist", "new", "s"], &[]),
            // search filters
            (&["search", "--artist", "ca"], &["Carbon Based Lifeforms"]),
            (&["search", "--title", "bl"], &["Blue Monday"]),
            (&["search", "--album", "po"], &["Power"]),
            (&["search", "--label", "fa"], &["Factory"]),
            (&["search", "--genre", "te"], &["Techno"]),
            (&["search", "--style", "ac"], &["Acid"]),
            // the free-text query: titles and artists
            (&["search", "ca"], &["carbon", "Carbon Based Lifeforms"]),
            (&["search", "dead"], &["deadmau5"]),
            // sources
            (&["search", "--source", "bea"], &["beatport"]),
            (&["search", "--source", "up"], &["upload"]),
            (&["source", "bandc"], &["bandcamp"]),
            (&["--node", "x", "mark", "ban"], &["banger"]),
            // origins: the ones the CLI writes, then the ones found
            (&["fact", "retract", "--origin", "b"], &["Bandcamp"]),
        ];
        for (words, expected) in cases {
            assert_eq!(complete_words(words), expected, "{words:?}");
        }
    }

    #[test]
    fn without_a_replica_the_arguments_complete_to_nothing() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut env = EnvVars::default();
        env.set("XDG_CACHE_HOME", dir.path().join("cache"));
        env.set("XDG_CONFIG_HOME", dir.path().join("config"));
        env.set("MDMA_NODE", "nobody.local");
        env.set("MDMA_LIBRARY_SOCKET", "");
        env.set("MDMA_PLAYBACK_SOCKET", "");

        assert!(complete_words(&["mark", ""]).is_empty());
        assert!(complete_words(&["playlist", "get", ""]).is_empty());
        assert!(complete_words(&["search", "--artist", ""]).is_empty());
    }
}
