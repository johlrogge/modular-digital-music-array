//! The completion snapshot: `completion.json` in the replica directory (ADR-021 §4).
//!
//! Completion must answer in a few milliseconds and never touch the network, so it
//! does not fold the replica's raw lines. After each sync the client folds them once
//! with `library_projection` and writes the distinct names completion offers: mark
//! names, artists, titles, albums, labels, genres, styles and origins, plus the
//! playlist and source names probed at the same sync.
//!
//! Lives here, not in `fact_replica`: the snapshot needs the [`Probe`] and the
//! fold, and `fact_replica` stays a plain store without a dependency on
//! `library_projection`.
//!
//! The file is written under the sync lock, with tmp + rename, and only when something
//! it is made of changed. Lines the fold cannot read are kept in the replica, left
//! out of the names and counted; see [`CompletionSnapshot::refresh`].

use std::collections::BTreeSet;
use std::path::PathBuf;

use color_eyre::eyre::eyre;
use color_eyre::Result;
use fact_replica::{LineCount, Replica, SyncReport, UnixSecs};
use library_projection::{project_lines, IndexedTrackInfo, Projection};
use music_facts::{Mark, StoredMark};
use serde::{Deserialize, Serialize};

use crate::replica::{Probe, Probed, PROBE_TTL_SECS};

const SNAPSHOT_FILE: &str = "completion.json";

/// The distinct names completion offers, each sorted.
///
/// `#[serde(default)]` so a file written by another version still reads; one that
/// does not parse at all counts as missing and is rebuilt by the next sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompletionSnapshot {
    /// Lines of the replica this was built from; a different count means it is behind.
    pub lines: LineCount,
    /// Mark names; `liked`, the default mark, is always among them.
    pub marks: Vec<String>,
    pub artists: Vec<String>,
    pub titles: Vec<String>,
    pub albums: Vec<String>,
    pub labels: Vec<String>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    /// Origin kinds of the facts (`User`, `Rekordbox`, ...).
    pub origins: Vec<String>,
    /// Lines of the replica the fold could not read (written by a newer node, say).
    /// They are kept in the replica and left out of the names above.
    pub unreadable: usize,
    /// Playlist and source names, with when they were probed (they expire).
    pub probe: Probe,
}

/// Collect the non-empty values of one field.
fn distinct<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    values
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether two probed lists would complete the same, and the snapshot's copy is
/// not so old that it would expire before the next rebuild: the snapshot keeps the
/// probe time it was built with, so a list that is re-probed unchanged must still
/// be rewritten now and then, or it would expire from the snapshot while fresh.
fn same_list(old: &Option<Probed>, new: &Option<Probed>) -> bool {
    match (old, new) {
        (None, None) => true,
        (Some(old), Some(new)) => {
            let age = new.at.since(old.at);
            old.names == new.names && age.is_none_or(|age| age < PROBE_TTL_SECS / 2)
        }
        _ => false,
    }
}

/// The distinct non-empty values of one scalar field of the tracks.
fn field(tracks: &[IndexedTrackInfo], get: fn(&IndexedTrackInfo) -> Option<&str>) -> Vec<String> {
    distinct(tracks.iter().filter_map(get))
}

impl CompletionSnapshot {
    /// Fold of `projection` and the probed names, built from `lines` replica lines.
    pub fn build(projection: &Projection, probe: &Probe, lines: LineCount) -> Self {
        let tracks = &projection.tracks;
        let styles = tracks.iter().flat_map(|t| &t.styles).map(String::as_str);
        let mut marks: BTreeSet<String> = BTreeSet::from([Mark::Default.to_string()]);
        let mut origins: BTreeSet<&str> = BTreeSet::new();
        for (value, source) in tracks.iter().flat_map(|t| &t.provenance) {
            // A legacy free-text scope is not a mark name `mark` would accept.
            if let Some(StoredMark::Mark(mark)) = value.stored_mark() {
                marks.insert(mark.to_string());
            }
            origins.insert(source.origin.kind.as_str());
        }
        Self {
            lines,
            marks: marks.into_iter().collect(),
            artists: field(tracks, |t| t.artist.as_deref()),
            titles: field(tracks, |t| t.title.as_deref()),
            albums: field(tracks, |t| t.album.as_deref()),
            labels: field(tracks, |t| t.label.as_deref()),
            genres: field(tracks, |t| t.genre.as_deref()),
            styles: distinct(styles),
            origins: distinct(origins.into_iter()),
            unreadable: projection.unparseable_lines,
            probe: probe.clone(),
        }
    }

    fn path(replica: &Replica) -> PathBuf {
        replica.dir().join(SNAPSHOT_FILE)
    }

    /// The stored snapshot as written. `None` when there is none or it is unreadable.
    pub fn load(replica: &Replica) -> Option<Self> {
        let bytes = std::fs::read(Self::path(replica)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Only the probed names still within their TTL.
    pub fn live(self, now: UnixSecs) -> Self {
        Self {
            probe: self.probe.live(now),
            ..self
        }
    }

    /// Atomic (tmp + rename), so a completion reading it never sees half a file.
    fn save(&self, replica: &Replica) -> Result<()> {
        let json = serde_json::to_vec(self).expect("CompletionSnapshot serializes");
        private_file::write_private_file(&Self::path(replica), &json)
            .map_err(|e| eyre!("could not write the completion snapshot: {e}"))
    }

    /// Whether this snapshot already says what a rebuild after `report` and with
    /// `probe` would: the same lines, no reset, and the same probed names.
    fn is_current(&self, report: &SyncReport, probe: &Probe) -> bool {
        !report.reset
            && self.lines == report.total
            && same_list(&self.probe.playlists, &probe.playlists)
            && same_list(&self.probe.sources, &probe.sources)
    }

    /// Rebuild `completion.json` after a sync (the caller holds the sync lock).
    ///
    /// Leaves the file alone (returning `false`) unless lines were added, the replica
    /// was refetched, the probed names changed or there is no readable snapshot.
    /// Comparing the line count with the one the snapshot was built from also
    /// catches a sync that was cut off between committing lines and writing this.
    ///
    /// A line the fold cannot read (a node newer than this client) is not an error and
    /// does not touch the replica: the names come from the lines that parse, and the
    /// count of the others is kept in the snapshot for `replica status` to report.
    /// Discarding the replica instead would refetch the whole log at every sync.
    pub fn refresh(replica: &Replica, report: &SyncReport, probe: &Probe) -> Result<bool> {
        if Self::load(replica).is_some_and(|existing| existing.is_current(report, probe)) {
            return Ok(false);
        }
        let lines = replica.lines().map_err(|e| eyre!("{e}"))?;
        let projection = project_lines(lines.iter().map(String::as_str));
        Self::build(&projection, probe, report.total).save(replica)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fact_replica::test_support::FakeNode;
    use fact_replica::{NodeId, SyncLock, SyncOutcome};
    use pretty_assertions::assert_eq;
    use std::path::Path;

    fn secs(n: u64) -> UnixSecs {
        UnixSecs::new(n)
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    /// One fact line: `["entity", {"t": .., "v": ..}, time, source, op]`.
    fn line(entity: &str, value: &str, origin: &str, op: &str) -> String {
        let source = format!(r#"{{"tool":"t","version":"1","origin":{{"type":"{origin}"}}}}"#);
        format!(r#"["{entity}",{value},"2026-01-01T12:00:00Z",{source},"{op}"]"#)
    }

    /// An assertion of a plain-text field by `origin`.
    fn fact_line(entity: &str, t: &str, v: &str, origin: &str) -> String {
        let value = format!(r#"{{"t":"{t}","v":"{v}"}}"#);
        line(entity, &value, origin, "Assert")
    }

    fn assert_line(entity: &str, t: &str, v: &str) -> String {
        fact_line(entity, t, v, "User")
    }

    fn mark_line(entity: &str, scope: Option<&str>) -> String {
        let scope = scope.map_or("null".to_string(), |s| format!(r#""{s}""#));
        let value = format!(
            r#"{{"t":"Bookmarked","v":{{"scope":{scope},"timestamp":"2026-01-01T12:00:00Z"}}}}"#
        );
        line(entity, &value, "User", "Assert")
    }

    /// Two tracks, 13 lines.
    fn small_log() -> Vec<String> {
        vec![
            assert_line("sha256:a1", "Title", "Blue Monday"),
            assert_line("sha256:a1", "Artist", "New Order"),
            assert_line("sha256:a1", "Album", "Power"),
            assert_line("sha256:a1", "Label", "Factory"),
            assert_line("sha256:a1", "MainGenre", "Electronic"),
            assert_line("sha256:a1", "StyleDescriptor", "Synth-pop"),
            mark_line("sha256:a1", Some("banger")),
            assert_line("sha256:b2", "Title", "Strobe"),
            assert_line("sha256:b2", "Artist", "deadmau5"),
            assert_line("sha256:b2", "Album", "Power"),
            assert_line("sha256:b2", "StyleDescriptor", "Progressive"),
            fact_line("sha256:b2", "Label", "mau5trap", "Rekordbox"),
            mark_line("sha256:b2", None),
        ]
    }

    fn node_with(lines: Vec<String>) -> FakeNode {
        let mut node = FakeNode::new("e1", 0);
        node.lines = lines;
        node
    }

    fn probe(playlists: &[&str], sources: &[&str], at: u64) -> Probe {
        let probed = |names: &[&str]| Probed {
            at: secs(at),
            names: names.iter().map(ToString::to_string).collect(),
        };
        Probe {
            playlists: Some(probed(playlists)),
            sources: Some(probed(sources)),
        }
    }

    /// A replica `n` under a directory, with its sync lock held for the whole test, as
    /// it is while a sync runs.
    struct Held {
        replica: Replica,
        lock: SyncLock,
    }

    impl Held {
        fn new(dir: &Path) -> Self {
            let replica = Replica::new(dir, &NodeId::new("n").unwrap());
            let lock = replica.try_lock().unwrap().unwrap();
            Self { replica, lock }
        }

        /// Sync from `node` at `now`.
        fn sync(&self, node: &FakeNode, now: u64) -> SyncReport {
            let mut read = |c, l, p| node.serve(c, l, p);
            self.replica
                .sync_locked(&self.lock, secs(now), None, &mut read)
                .unwrap()
        }

        /// Whether the refresh rebuilt the snapshot.
        fn refresh(&self, report: &SyncReport, probe: &Probe) -> bool {
            CompletionSnapshot::refresh(&self.replica, report, probe).unwrap()
        }

        fn snapshot(&self) -> Option<CompletionSnapshot> {
            CompletionSnapshot::load(&self.replica)
        }

        /// Overwrite the stored snapshot with a marker that is still "current", so a
        /// later rebuild shows as the marker disappearing.
        fn plant_marker(&self) -> CompletionSnapshot {
            let mut planted = self.snapshot().expect("a snapshot");
            planted.titles = strings(&["PLANTED"]);
            planted.save(&self.replica).unwrap();
            planted
        }
    }

    // --- build -----------------------------------------------------------------

    #[test]
    fn a_snapshot_is_built_from_a_small_projection() {
        let log = small_log();
        let projection = project_lines(log.iter().map(String::as_str));
        assert_eq!(projection.unparseable_lines, 0, "the fixture lines parse");
        let probed = probe(&["friday"], &["bandcamp"], 50);

        let snapshot = CompletionSnapshot::build(&projection, &probed, LineCount::new(13));

        assert_eq!(snapshot.lines, LineCount::new(13));
        assert_eq!(snapshot.marks, strings(&["banger", "liked"]));
        assert_eq!(snapshot.artists, strings(&["New Order", "deadmau5"]));
        assert_eq!(snapshot.titles, strings(&["Blue Monday", "Strobe"]));
        assert_eq!(snapshot.albums, strings(&["Power"]), "distinct");
        assert_eq!(snapshot.labels, strings(&["Factory", "mau5trap"]));
        assert_eq!(snapshot.genres, strings(&["Electronic"]));
        assert_eq!(snapshot.styles, strings(&["Progressive", "Synth-pop"]));
        assert_eq!(snapshot.origins, strings(&["Rekordbox", "User"]));
        let names = |list: &Option<Probed>| list.as_ref().unwrap().names.clone();
        assert_eq!(names(&snapshot.probe.playlists), strings(&["friday"]));
        assert_eq!(names(&snapshot.probe.sources), strings(&["bandcamp"]));
    }

    #[test]
    fn liked_is_a_mark_even_when_nothing_is_marked() {
        let projection = project_lines(std::iter::empty());
        let none = Probe::default();

        let snapshot = CompletionSnapshot::build(&projection, &none, LineCount::new(0));

        assert_eq!(snapshot.marks, strings(&["liked"]));
        assert!(snapshot.titles.is_empty());
    }

    #[test]
    fn a_snapshot_is_written_atomically_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        let report = held.sync(&node_with(small_log()), 100);
        let empty = probe(&[], &[], 1);

        assert!(held.refresh(&report, &empty));

        let snapshot = held.snapshot().expect("written");
        assert_eq!(snapshot.titles, strings(&["Blue Monday", "Strobe"]));
        let mut left: Vec<String> = std::fs::read_dir(held.replica.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["completion.json", "facts.jsonl", "meta.json", "sync.lock"],
            "no temporary file is left behind"
        );
    }

    #[test]
    fn an_unreadable_snapshot_file_counts_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        held.sync(&node_with(small_log()), 100);
        assert_eq!(held.snapshot(), None);

        std::fs::write(held.replica.dir().join(SNAPSHOT_FILE), b"{ not json").unwrap();

        assert_eq!(held.snapshot(), None);
    }

    #[test]
    fn probed_names_past_their_ttl_are_dropped_on_read() {
        let snapshot = CompletionSnapshot {
            probe: probe(&["p"], &["s"], 1_000),
            ..CompletionSnapshot::default()
        };
        let fresh = snapshot.clone().live(secs(1_000 + PROBE_TTL_SECS - 1));
        assert!(fresh.probe.playlists.is_some() && fresh.probe.sources.is_some());
        let expired = snapshot.live(secs(1_000 + PROBE_TTL_SECS));
        assert_eq!(expired.probe, Probe::default());
    }

    // --- refresh: only when something changed ----------------------------------

    #[test]
    fn the_snapshot_is_rebuilt_only_when_lines_or_the_probe_changed() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        let mut node = node_with(small_log());
        let first = held.sync(&node, 100);
        let probe_a = probe(&["friday"], &["bandcamp"], 100);
        assert!(held.refresh(&first, &probe_a));
        let planted = held.plant_marker();

        // A sync that found nothing new, with the same probed names (re-probed later).
        let again = held.sync(&node, 200);
        assert_eq!(again.added, LineCount::new(0));
        let re_probed = probe(&["friday"], &["bandcamp"], 200);
        assert!(!held.refresh(&again, &re_probed), "nothing changed");
        assert_eq!(held.snapshot(), Some(planted));

        // New lines.
        node.lines.push(assert_line("sha256:c3", "Title", "Acid"));
        let grown = held.sync(&node, 300);
        assert_eq!(grown.added, LineCount::new(1));
        assert!(held.refresh(&grown, &re_probed));
        let snapshot = held.snapshot().unwrap();
        let titles = strings(&["Acid", "Blue Monday", "Strobe"]);
        assert_eq!(snapshot.titles, titles);
        assert_eq!(snapshot.lines, LineCount::new(14));

        // A changed probe, no new lines.
        let planted = held.plant_marker();
        let idle = held.sync(&node, 400);
        let renamed = probe(&["friday", "saturday"], &["bandcamp"], 400);
        assert!(held.refresh(&idle, &renamed));
        let rebuilt = held.snapshot().unwrap();
        assert_ne!(rebuilt.titles, planted.titles);
        let playlists = rebuilt.probe.playlists.unwrap().names;
        assert_eq!(playlists, strings(&["friday", "saturday"]));
    }

    #[test]
    fn a_missing_snapshot_is_built_even_when_the_sync_found_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        let node = node_with(small_log());
        let none = Probe::default();
        let report = held.sync(&node, 100);
        assert!(held.refresh(&report, &none));
        std::fs::remove_file(held.replica.dir().join(SNAPSHOT_FILE)).unwrap();

        let idle = held.sync(&node, 200);

        assert!(held.refresh(&idle, &none));
        assert!(held.snapshot().is_some());
    }

    #[test]
    fn a_snapshot_behind_the_replica_is_caught_up_after_a_sync_cut_off_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        let mut node = node_with(small_log());
        let none = Probe::default();
        let report = held.sync(&node, 100);
        assert!(held.refresh(&report, &none));
        // The next sync committed a new line, then died before writing the snapshot.
        node.lines.push(assert_line("sha256:c3", "Title", "Acid"));
        let grown = held.sync(&node, 200);
        assert_eq!(grown.added, LineCount::new(1));
        // The following sync adds nothing, but the snapshot still lacks the line.
        let idle = held.sync(&node, 300);
        assert_eq!(idle.added, LineCount::new(0));

        assert!(held.refresh(&idle, &none));

        let titles = held.snapshot().unwrap().titles;
        assert!(titles.contains(&"Acid".to_string()), "{titles:?}");
    }

    #[test]
    fn a_probe_that_keeps_its_names_is_still_rewritten_before_it_would_expire() {
        let snapshot = CompletionSnapshot {
            lines: LineCount::new(3),
            probe: probe(&["a"], &["s"], 1_000),
            ..CompletionSnapshot::default()
        };
        let report = SyncReport {
            added: LineCount::new(0),
            total: LineCount::new(3),
            reset: false,
        };
        let refetched = SyncReport {
            reset: true,
            ..report
        };
        let later = |at| probe(&["a"], &["s"], at);

        let almost = later(1_000 + PROBE_TTL_SECS / 2 - 1);
        assert!(snapshot.is_current(&report, &almost));
        let expiring = later(1_000 + PROBE_TTL_SECS / 2);
        assert!(!snapshot.is_current(&report, &expiring));
        let soon = later(1_001);
        assert!(
            !snapshot.is_current(&refetched, &soon),
            "a refetched replica is rebuilt"
        );
    }

    // --- an unreadable log -----------------------------------------------------

    #[test]
    fn a_line_the_fold_cannot_read_is_kept_counted_and_does_not_disturb_the_sync() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        let mut log = small_log();
        log.push(r#"["from-a-newer-node"]"#.to_string());
        let mut node = node_with(log);
        let report = held.sync(&node, 100);
        let none = Probe::default();

        assert!(held.refresh(&report, &none));

        // The replica is kept whole, and the snapshot has what parses, and the count.
        assert_eq!(held.replica.lines().unwrap(), node.lines);
        let snapshot = held.snapshot().unwrap();
        assert_eq!(snapshot.titles, strings(&["Blue Monday", "Strobe"]));
        assert_eq!(snapshot.unreadable, 1);
        assert_eq!(snapshot.lines, LineCount::new(14));

        // The next sync is incremental, and it is not counted as a failure.
        node.forget_requests();
        node.lines.push(assert_line("sha256:c3", "Title", "Acid"));
        let next = held.sync(&node, 200);
        assert!(!next.reset);
        assert_eq!(next.added, LineCount::new(1));
        assert_eq!(node.requests()[0].cursor.as_deref(), Some("line:14"));
        assert!(held.refresh(&next, &none));
        assert_eq!(held.snapshot().unwrap().unreadable, 1);
        let meta = held.replica.meta().unwrap().unwrap();
        assert_eq!(meta.failures, 0, "so the backoff does not grow");
        assert_eq!(meta.lines(), LineCount::new(15));
    }

    #[test]
    fn reading_a_snapshot_needs_no_lock() {
        // `load` takes no lock: completion reads while a sync is running.
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(dir.path());
        let report = held.sync(&node_with(small_log()), 100);
        held.refresh(&report, &Probe::default());

        assert!(held.snapshot().is_some());
        let outcome = held
            .replica
            .sync(secs(1), None, &mut |_, _, _| panic!("busy"));
        assert!(matches!(outcome, Ok(SyncOutcome::Busy)));
    }

    // --- how long it takes -----------------------------------------------------

    #[test]
    fn building_for_a_log_of_24k_lines_is_quick() {
        // About the size of the production log: 8 000 tracks of 3 facts each, with
        // full-length hashes. The third is a genre, or a play for every other track.
        let log: Vec<String> = (0..8_000)
            .flat_map(|i| {
                let hash = format!("sha256:{i:08x}{:056}", 0);
                let third = if i % 2 == 0 {
                    assert_line(&hash, "MainGenre", &format!("Genre {}", i % 20))
                } else {
                    assert_line(&hash, "TrackStarted", "OnRequest")
                };
                [
                    assert_line(&hash, "Title", &format!("Track number {i}")),
                    assert_line(&hash, "Artist", &format!("Artist {}", i % 900)),
                    third,
                ]
            })
            .collect();
        assert_eq!(log.len(), 24_000);
        let none = Probe::default();

        let started = std::time::Instant::now();
        let projection = project_lines(log.iter().map(String::as_str));
        let folded = started.elapsed();
        let snapshot = CompletionSnapshot::build(&projection, &none, LineCount::new(log.len()));
        let total = started.elapsed();

        assert_eq!(projection.unparseable_lines, 0);
        assert_eq!(snapshot.titles.len(), 8_000);
        assert_eq!(snapshot.artists.len(), 900);
        // A generous bound for an unoptimised test build; the point is "not seconds".
        assert!(total.as_secs() < 10, "fold {folded:?}, total {total:?}");

        // The plays of every track, and the latest of them, as `mdma plays` reads them.
        let started = std::time::Instant::now();
        let histories = projection.lineage.histories(&projection.tracks);
        let recent = projection.lineage.recent(&projection.tracks, 20);
        let plays = started.elapsed();
        assert_eq!(histories.iter().filter(|h| h.count() == 1).count(), 4_000);
        assert_eq!(recent.len(), 20);
        assert!(plays.as_secs() < 10, "plays {plays:?}");
    }
}
