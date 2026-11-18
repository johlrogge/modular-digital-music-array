//! What `mdma rekordbox import` and `export` remember of the last exchange (ADR-034).
//!
//! Rekordbox XML has no modification time per field, so "the most recent value wins" is
//! decided against the last value exchanged: the stars, per track, as of the last import
//! or export. That base is a three-way merge's third leg (`rekordbox_roundtrip::decide`).
//! It lives in a state file of the CLI, never in the library (the library holds no
//! Pioneer data), and per machine and, within it, per node.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Context};
use color_eyre::Result;
use library_ipc_client::{
    rekordbox_source, ClientError, ContentHash, EnergyLevel, MusicValue, TrackInfo,
};
use mdma_client::LibraryBackend;
use rekordbox_roundtrip::{decide, Decision, Stars};
use rekordbox_xml::RekordboxTrack;
use serde::{Deserialize, Serialize};

/// Name of the state file, in `mdma/` of the XDG state directory.
const FILE_NAME: &str = "rekordbox-exchange.json";

/// The state file: what was last exchanged with each node. Another node's content
/// hashes are not ours, so the bases of the nodes never mix.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    nodes: BTreeMap<String, NodeState>,
}

/// What was last exchanged with one node.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NodeState {
    /// Content hash to the stars last exchanged.
    tracks: HashMap<ContentHash, Stars>,
    last_import: Option<String>,
    last_export: Option<String>,
}

/// The state of one node, as loaded from the state file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Exchange {
    node: String,
    state: NodeState,
}

/// `$XDG_STATE_HOME/mdma/rekordbox-exchange.json`, or `~/.local/state/mdma/...`.
pub(crate) fn state_file() -> Result<PathBuf> {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("state")))
        .map(|dir| dir.join("mdma").join(FILE_NAME))
        .ok_or_else(|| eyre!("cannot tell where to keep the exchange state: no home directory"))
}

/// What names the node in the state file: the lowercased host of a remote node, `local`
/// for the services on this machine. An error when the node cannot be resolved: a
/// guess would mix the bases of two nodes.
pub(crate) fn node_key(node: &mdma_client::Node) -> Result<String> {
    Ok(node
        .remote_host()?
        .map_or_else(|| "local".to_string(), |(host, _)| host.to_lowercase()))
}

impl Exchange {
    /// The state of `node` in `path`. Nothing there, or nothing of this node, is a fresh
    /// start (every base is 0 stars); a file that cannot be read, or holds a value that
    /// is not 0 to 5 stars, is an error, so that a damaged file never passes for
    /// "nothing exchanged".
    pub(crate) fn load(path: &Path, node: &str) -> Result<Self> {
        let mut file = read_file(path)?;
        Ok(Self {
            node: node.to_string(),
            state: file.nodes.remove(node).unwrap_or_default(),
        })
    }

    /// Write the state of this node atomically and for the owner only, leaving the
    /// entries of the other nodes as the file has them now.
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let mut file = read_file(path)?;
        file.nodes.insert(self.node.clone(), self.state.clone());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .wrap_err_with(|| format!("cannot create {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(&file)?;
        private_file::write_private_file(path, json.as_bytes())
            .wrap_err_with(|| format!("cannot write {}", path.display()))
    }

    /// The stars last exchanged for `hash`; none counts as 0.
    pub(crate) fn base(&self, hash: &ContentHash) -> Stars {
        self.state.tracks.get(hash).copied().unwrap_or(Stars::NONE)
    }

    pub(crate) fn set_base(&mut self, hash: &ContentHash, stars: Stars) {
        self.state.tracks.insert(hash.clone(), stars);
    }

    fn stamp() -> Option<String> {
        Some(chrono::Utc::now().to_rfc3339())
    }
}

/// The state file at `path`: empty when there is none, an error when it is damaged.
fn read_file(path: &Path) -> Result<StateFile> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StateFile::default()),
        Err(e) => return Err(e).wrap_err_with(|| format!("cannot read {}", path.display())),
    };
    serde_json::from_str(&text).wrap_err_with(|| {
        format!(
            "{} is not valid; remove it to start the exchange over",
            path.display()
        )
    })
}

/// Remember `exported` as the base of those tracks: what an export wrote is what was
/// last exchanged.
pub(crate) fn record_export(
    path: &Path,
    node: &str,
    exported: &[(ContentHash, Stars)],
) -> Result<()> {
    let mut exchange = Exchange::load(path, node)?;
    for (hash, stars) in exported {
        exchange.set_base(hash, *stars);
    }
    exchange.state.last_export = Exchange::stamp();
    exchange.save(path)
}

/// A track whose energy was left alone though Rekordbox changed its rating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conflict {
    /// `Artist - Title`.
    pub(crate) track: String,
    pub(crate) ours: EnergyLevel,
    pub(crate) theirs: Stars,
}

impl Conflict {
    /// `  Artist - Title: kept MDMA 7 (4★), rekordbox has 2★`
    pub(crate) fn line(&self) -> String {
        let stars = Stars::from_energy(Some(self.ours));
        format!(
            "  {}: kept MDMA {} ({stars}), rekordbox has {}",
            self.track, self.ours, self.theirs
        )
    }
}

/// What an import did, or with `--dry-run` would do, to the energy of the matched tracks.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct EnergyReport {
    pub(crate) applied: usize,
    pub(crate) cleared: usize,
    pub(crate) kept_local: usize,
    pub(crate) unchanged: usize,
    /// Tracks the library would not answer for or accept: nothing was done to them.
    pub(crate) failed: usize,
    pub(crate) conflicts: Vec<Conflict>,
    /// `Would ...` lines of a dry run.
    pub(crate) would: Vec<String>,
}

impl EnergyReport {
    /// The one line for stdout.
    pub(crate) fn summary(&self, dry_run: bool) -> String {
        let (apply, clear) = if dry_run {
            ("would apply", "would clear")
        } else {
            ("applied", "cleared")
        };
        format!(
            "Energy: {apply} {}, {clear} {}, kept local {} (conflict), unchanged {}, failed {}",
            self.applied, self.cleared, self.kept_local, self.unchanged, self.failed
        )
    }
}

/// The merge of Rekordbox ratings into the energy of the library, track by track
/// (ADR-034 §2): `load`, `track` for each matched track, then `finish`.
///
/// For each track the three stars are the base in the state file, the rating in Rekordbox
/// now, and the energy the library shows. A change taken from Rekordbox is written as
/// `rekordbox_source`, or retracted from every source that writes energy when it is a
/// clear. Afterwards the base of every matched track is the rating, conflicts included
/// so that each is reported once. A dry run writes neither facts nor base.
pub(crate) struct EnergySync {
    path: PathBuf,
    exchange: Exchange,
    dry_run: bool,
    report: EnergyReport,
}

impl EnergySync {
    /// Load the state of `node` from `path`. Done before anything is written, so that a
    /// damaged file stops the import before it has changed anything.
    pub(crate) fn load(path: &Path, node: &str, dry_run: bool) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            exchange: Exchange::load(path, node)?,
            dry_run,
            report: EnergyReport::default(),
        })
    }

    /// Merge the rating of one matched track. `shown` is the track as the library has
    /// it, which the caller already fetched; the error when it could not be.
    pub(crate) fn track(
        &mut self,
        library: &LibraryBackend,
        rb_track: &RekordboxTrack,
        hash: &ContentHash,
        shown: Result<&TrackInfo, &ClientError>,
    ) {
        let who = format!("{} - {}", rb_track.artist, rb_track.name);
        let shown = match shown {
            Ok(track) => track.energy,
            Err(e) => {
                eprintln!("Failed to read {who} from the library: {e}; its energy was not merged");
                self.report.failed += 1;
                return;
            }
        };
        let Self {
            exchange,
            dry_run,
            report,
            ..
        } = self;
        let dry_run = *dry_run;
        let version = env!("CARGO_PKG_VERSION");
        let source = rekordbox_source(version);
        let theirs = Stars::from_rating(rb_track.rating);
        let ours = Stars::from_energy(shown);
        let outcome = match (decide(exchange.base(hash), theirs, ours), shown) {
            (Decision::Keep | Decision::Converged, _) => {
                report.unchanged += 1;
                Ok(())
            }
            (Decision::Conflict, Some(ours)) => {
                report.kept_local += 1;
                report.conflicts.push(Conflict {
                    track: who.clone(),
                    ours,
                    theirs,
                });
                Ok(())
            }
            (Decision::Conflict, None) => {
                unreachable!("a conflict needs a local energy: an unrated local gives way")
            }
            (Decision::Take(stars), _) => match stars.to_energy() {
                None if dry_run => {
                    report.cleared += 1;
                    report.would.push(format!("Would clear energy for {who}"));
                    Ok(())
                }
                None => library
                    .retract_energy(hash, version)
                    .map(|_| report.cleared += 1),
                Some(energy) if dry_run => {
                    report.applied += 1;
                    report
                        .would
                        .push(format!("Would apply energy {energy} ({stars}) for {who}"));
                    Ok(())
                }
                Some(energy) => library
                    .write_fact_as(hash, MusicValue::Energy(energy), &source)
                    .map(|()| report.applied += 1),
            },
        };
        match outcome {
            Ok(()) if !dry_run => exchange.set_base(hash, theirs),
            Ok(()) => {}
            Err(e) => {
                eprintln!("Failed to write the energy of {who}: {e}");
                report.failed += 1;
            }
        }
    }

    /// Save the bases unless this is a dry run, and give the report.
    pub(crate) fn finish(mut self) -> Result<EnergyReport> {
        if !self.dry_run {
            self.exchange.state.last_import = Exchange::stamp();
            self.exchange.save(&self.path)?;
        }
        Ok(self.report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serve, track};
    use library_ipc_client::{
        energy_sources, user_fact_source, EnergyLevel, FactSource, LibraryRequest, LibraryResponse,
        ProtocolError, TrackInfo,
    };
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn hash(c: char) -> ContentHash {
        ContentHash::new(format!("sha256:{}", c.to_string().repeat(64)))
    }

    /// A library that keeps the energy asserted per track and source, shows the last
    /// live one, and remembers every request it was sent.
    struct FakeLibrary {
        backend: LibraryBackend,
        requests: Arc<Mutex<Vec<LibraryRequest>>>,
        _dir: tempfile::TempDir,
    }

    impl FakeLibrary {
        /// The tracks given, each with the energy the user has asserted (if any).
        fn with(tracks: &[(char, Option<u8>)]) -> Self {
            let user = user_fact_source("0");
            let state: Mutex<HashMap<String, Vec<(u8, FactSource)>>> = Mutex::new(
                tracks
                    .iter()
                    .map(|(c, e)| {
                        let asserted = e.iter().map(|n| (*n, user.clone())).collect();
                        (hash(*c).as_str().to_string(), asserted)
                    })
                    .collect(),
            );
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            let dir = tempfile::tempdir().expect("temp dir");
            let address = format!("ipc://{}/library.sock", dir.path().display());
            serve(&address, move |request| {
                let request: LibraryRequest = serde_json::from_slice(request).expect("a request");
                log.lock().unwrap().push(request.clone());
                let mut state = state.lock().unwrap();
                let response = match request {
                    LibraryRequest::GetTrack { hash } => match state.get(hash.as_str()) {
                        Some(asserted) => LibraryResponse::Track(Box::new(TrackInfo {
                            energy: asserted.last().map(|(n, _)| EnergyLevel::new(*n).unwrap()),
                            ..track(hash.as_str())
                        })),
                        None => LibraryResponse::Error(ProtocolError::TrackNotFound {
                            hash: hash.as_str().to_owned(),
                        }),
                    },
                    LibraryRequest::WriteFact {
                        hash,
                        fact: MusicValue::Energy(level),
                        source,
                    } => {
                        state.entry(hash.as_str().to_owned()).or_default().push((
                            level.value(),
                            source.unwrap_or_else(|| user_fact_source("0")),
                        ));
                        LibraryResponse::FactWritten
                    }
                    LibraryRequest::RetractFact {
                        hash,
                        fact: MusicValue::Energy(level),
                        source,
                    } => {
                        let by = source.unwrap_or_else(|| user_fact_source("0"));
                        let asserted = state.entry(hash.as_str().to_owned()).or_default();
                        let before = asserted.len();
                        asserted.retain(|(n, s)| *n != level.value() || !s.same_source(&by));
                        if asserted.len() < before {
                            LibraryResponse::FactRetracted
                        } else {
                            LibraryResponse::NothingToRetract
                        }
                    }
                    other => LibraryResponse::Error(ProtocolError::Internal {
                        message: format!("unexpected {other:?}"),
                    }),
                };
                serde_json::to_vec(&response).expect("a response")
            });
            Self {
                backend: LibraryBackend::connect_direct(&address).expect("connect"),
                requests,
                _dir: dir,
            }
        }

        fn writes(&self) -> Vec<LibraryRequest> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    matches!(
                        r,
                        LibraryRequest::WriteFact { .. } | LibraryRequest::RetractFact { .. }
                    )
                })
                .cloned()
                .collect()
        }

        fn energy(&self, c: char) -> Option<EnergyLevel> {
            self.backend.get_track(&hash(c)).unwrap().energy
        }
    }

    fn rb(name: &str, rating: u32) -> RekordboxTrack {
        RekordboxTrack {
            name: name.to_string(),
            artist: "Artist".to_string(),
            rating,
            ..RekordboxTrack::default()
        }
    }

    fn stars(n: u8) -> Stars {
        Stars::new(n).unwrap()
    }

    fn level(n: u8) -> EnergyLevel {
        EnergyLevel::new(n).unwrap()
    }

    /// One import of `tracks` (character, rating) into `lib`, state at `path`.
    fn import(
        lib: &FakeLibrary,
        path: &Path,
        tracks: &[(char, u32)],
        dry_run: bool,
    ) -> EnergyReport {
        let mut sync = EnergySync::load(path, "node.local", dry_run).unwrap();
        for (c, rating) in tracks {
            let shown = lib.backend.get_track(&hash(*c));
            sync.track(
                &lib.backend,
                &rb(&format!("Track {c}"), *rating),
                &hash(*c),
                shown.as_ref(),
            );
        }
        sync.finish().unwrap()
    }

    fn state_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("mdma").join(FILE_NAME)
    }

    // ── the decision applied to a library ────────────────────────────────────

    #[test]
    fn a_new_rating_is_applied_as_the_rekordbox_source() {
        let lib = FakeLibrary::with(&[('a', None)]);
        let dir = tempfile::tempdir().unwrap();
        let report = import(&lib, &state_in(&dir), &[('a', 204)], false);
        assert_eq!(
            (report.applied, report.cleared, report.unchanged),
            (1, 0, 0)
        );
        assert_eq!(lib.energy('a'), Some(level(8)));
        let writes = lib.writes();
        assert_eq!(writes.len(), 1);
        match &writes[0] {
            LibraryRequest::WriteFact { fact, source, .. } => {
                assert_eq!(*fact, MusicValue::Energy(level(8)));
                let source = source.as_ref().expect("the import states its source");
                assert!(source.same_source(&rekordbox_source("any")));
                assert!(!source.same_source(&user_fact_source("any")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_rating_cleared_in_rekordbox_clears_the_energy_of_every_source() {
        // The user asserted 6, Rekordbox's import 4 (3 stars was exchanged: base 3).
        let lib = FakeLibrary::with(&[('a', Some(6))]);
        lib.backend
            .write_fact_as(
                &hash('a'),
                MusicValue::Energy(level(5)),
                &rekordbox_source("0"),
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "node.local", &[(hash('a'), stars(3))]).unwrap();

        let report = import(&lib, &path, &[('a', 0)], false);
        assert_eq!((report.applied, report.cleared), (0, 1));
        assert_eq!(lib.energy('a'), None, "nothing resurfaces");
        // Retracted from every source that writes energy.
        let retracted_by: Vec<FactSource> = lib
            .writes()
            .into_iter()
            .filter_map(|r| match r {
                LibraryRequest::RetractFact { source, .. } => source,
                _ => None,
            })
            .collect();
        for source in energy_sources("any") {
            assert!(retracted_by.iter().any(|s| s.same_source(&source)));
        }
    }

    #[test]
    fn a_rating_that_matches_the_energy_writes_nothing() {
        // Energy 7 is 4 stars: Rekordbox's 4 stars (204) is already what we have.
        let lib = FakeLibrary::with(&[('a', Some(7))]);
        let dir = tempfile::tempdir().unwrap();
        let report = import(&lib, &state_in(&dir), &[('a', 204)], false);
        assert_eq!((report.applied, report.unchanged), (0, 1));
        assert!(lib.writes().is_empty());
        assert_eq!(lib.energy('a'), Some(level(7)), "7 is not rounded to 8");
    }

    #[test]
    fn a_rating_that_did_not_change_keeps_a_local_edit() {
        let lib = FakeLibrary::with(&[('a', Some(9))]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "node.local", &[(hash('a'), stars(2))]).unwrap();
        let report = import(&lib, &path, &[('a', 102)], false);
        assert_eq!((report.unchanged, report.kept_local), (1, 0));
        assert!(report.conflicts.is_empty());
        assert!(lib.writes().is_empty());
    }

    #[test]
    fn a_conflict_keeps_the_local_energy_and_is_reported_once() {
        let lib = FakeLibrary::with(&[('a', Some(7))]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "node.local", &[(hash('a'), stars(1))]).unwrap();

        // Rekordbox went 1 to 2 stars; the user had 7 (4 stars).
        let first = import(&lib, &path, &[('a', 102)], false);
        assert_eq!(first.kept_local, 1);
        assert_eq!(first.conflicts.len(), 1);
        assert_eq!(
            first.conflicts[0].line(),
            "  Artist - Track a: kept MDMA 7 (4★), rekordbox has 2★"
        );
        assert!(lib.writes().is_empty(), "local wins");
        assert_eq!(lib.energy('a'), Some(level(7)));

        // The base moved on to what Rekordbox has, so the next import is quiet.
        let second = import(&lib, &path, &[('a', 102)], false);
        assert!(second.conflicts.is_empty());
        assert_eq!((second.kept_local, second.unchanged), (0, 1));
    }

    #[test]
    fn importing_twice_changes_nothing_the_second_time() {
        let lib = FakeLibrary::with(&[('a', None), ('b', Some(3))]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        let ratings = [('a', 153), ('b', 0)];
        let first = import(&lib, &path, &ratings, false);
        assert_eq!(first.applied, 1);
        let writes_after_first = lib.writes().len();

        let second = import(&lib, &path, &ratings, false);
        assert_eq!(
            (second.applied, second.cleared, second.unchanged),
            (0, 0, 2)
        );
        assert_eq!(lib.writes().len(), writes_after_first);
    }

    #[test]
    fn a_local_clear_gives_way_to_a_later_rekordbox_change() {
        let lib = FakeLibrary::with(&[('a', None)]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "node.local", &[(hash('a'), stars(2))]).unwrap();
        let report = import(&lib, &path, &[('a', 255)], false);
        assert_eq!(report.applied, 1);
        assert_eq!(lib.energy('a'), Some(level(10)));
    }

    // ── dry run ──────────────────────────────────────────────────────────────

    #[test]
    fn a_dry_run_writes_neither_facts_nor_base() {
        let lib = FakeLibrary::with(&[('a', None), ('b', Some(3))]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "node.local", &[(hash('b'), stars(2))]).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let report = import(&lib, &path, &[('a', 204), ('b', 0)], true);
        assert_eq!((report.applied, report.cleared), (1, 1));
        assert_eq!(report.would.len(), 2);
        assert!(report.would.iter().all(|line| line.contains("Would")));
        assert!(lib.writes().is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "no base saved"
        );
        assert_eq!(lib.energy('a'), None);
        assert_eq!(lib.energy('b'), Some(level(3)));
    }

    // ── the state file ───────────────────────────────────────────────────────

    #[test]
    fn the_base_is_saved_after_an_import_and_an_export_and_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let lib = FakeLibrary::with(&[('a', None), ('b', Some(7))]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        import(&lib, &path, &[('a', 204), ('b', 204)], false);
        let saved = Exchange::load(&path, "node.local").unwrap();
        assert_eq!(saved.base(&hash('a')), stars(4));
        assert_eq!(saved.base(&hash('b')), stars(4));
        assert!(saved.state.last_import.is_some() && saved.state.last_export.is_none());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        record_export(&path, "node.local", &[(hash('a'), stars(5))]).unwrap();
        let saved = Exchange::load(&path, "node.local").unwrap();
        assert_eq!(saved.base(&hash('a')), stars(5));
        assert_eq!(saved.base(&hash('b')), stars(4), "other tracks keep theirs");
        assert!(saved.state.last_import.is_some() && saved.state.last_export.is_some());
    }

    #[test]
    fn the_file_has_the_node_tracks_and_times() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "node.local", &[(hash('a'), stars(3))]).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let node = &json["nodes"]["node.local"];
        assert_eq!(node["tracks"][hash('a').as_str()], 3);
        assert!(node["last_export"].is_string());
        assert!(node["last_import"].is_null());
    }

    #[test]
    fn the_base_of_another_node_is_not_ours_and_survives_our_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        record_export(&path, "one.local", &[(hash('a'), stars(3))]).unwrap();
        let other = Exchange::load(&path, "two.local").unwrap();
        assert_eq!(other.base(&hash('a')), Stars::NONE);

        record_export(&path, "two.local", &[(hash('a'), stars(5))]).unwrap();
        assert_eq!(
            Exchange::load(&path, "one.local").unwrap().base(&hash('a')),
            stars(3),
            "saving for two.local leaves one.local's base"
        );
        assert_eq!(
            Exchange::load(&path, "two.local").unwrap().base(&hash('a')),
            stars(5)
        );
    }

    #[test]
    fn a_stored_value_above_five_stars_is_an_error_not_no_stars() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let json = format!(
            r#"{{"nodes":{{"n":{{"tracks":{{"{}":6}}}}}}}}"#,
            hash('a').as_str()
        );
        std::fs::write(&path, json).unwrap();
        let err = format!("{:#}", Exchange::load(&path, "n").unwrap_err());
        assert!(
            err.contains("remove it to start the exchange over"),
            "{err}"
        );
    }

    #[test]
    fn a_track_the_library_cannot_give_is_counted_as_failed_and_keeps_no_base() {
        let lib = FakeLibrary::with(&[('a', None)]);
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        let report = import(&lib, &path, &[('a', 204), ('z', 204)], false);
        assert_eq!((report.applied, report.failed), (1, 1));
        assert!(report.summary(false).ends_with(", failed 1"));
        let saved = Exchange::load(&path, "node.local").unwrap();
        assert_eq!(saved.base(&hash('a')), stars(4));
        assert_eq!(saved.base(&hash('z')), Stars::NONE);
    }

    #[test]
    fn a_missing_file_is_a_fresh_start_and_a_damaged_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_in(&dir);
        assert_eq!(
            Exchange::load(&path, "n").unwrap().base(&hash('a')),
            Stars::NONE
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let err = format!("{:#}", Exchange::load(&path, "n").unwrap_err());
        assert!(err.contains("remove it"), "{err}");
    }

    #[test]
    fn the_state_file_follows_xdg_state_home() {
        let _lock = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut env = crate::test_support::EnvVars::default();
        env.set("XDG_STATE_HOME", dir.path());
        assert_eq!(
            state_file().unwrap(),
            dir.path().join("mdma").join(FILE_NAME)
        );
    }

    // ── reporting ────────────────────────────────────────────────────────────

    #[test]
    fn the_summary_is_one_line_with_the_counts() {
        let report = EnergyReport {
            applied: 12,
            cleared: 1,
            kept_local: 3,
            unchanged: 240,
            ..EnergyReport::default()
        };
        assert_eq!(
            report.summary(false),
            "Energy: applied 12, cleared 1, kept local 3 (conflict), unchanged 240, failed 0"
        );
        assert!(report
            .summary(true)
            .starts_with("Energy: would apply 12, would clear 1"));
    }
}
