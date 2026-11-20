//! The missing-tracks report of `rekordbox import --report`.
//!
//! Built from the match results the import already has (nothing is matched
//! twice); this module only turns them into rows.
//!
//! The point of the report is completeness: nothing rekordbox has may silently fail
//! to reach MDMA. Every collection track is exactly one of matched, weak, shared,
//! ambiguous or unmatched ([`Summary`]); every playlist entry is either written or
//! dropped for a stated cause ([`Dropped`]).
//!
//! Format: TSV, one header row, one schema ([`Row`]).
//! - `unmatched`, `ambiguous`, `weak` and `shared` rows are per rekordbox track. A
//!   row with several candidates repeats once per candidate (the `candidate_*`
//!   columns), so no cell holds a nested list that would need its own escaping.
//!   `weak`: the library has the track but with another duration (a probable other
//!   version); it is still written to playlists. `shared`: several rekordbox tracks
//!   matched the same library track; one row per rekordbox track, the candidate
//!   being the shared library track.
//! - `dropped-from-playlist` rows are per playlist, with the counts in `reason`.
//! - `skipped-playlist` rows: a playlist that is not imported at all (bad name).
//! - `playlist-name-collision` rows: several rekordbox playlists become one MDMA
//!   playlist, each replacing the last. The track columns are empty on playlist rows.
//!
//! A tab, CR or LF inside a field is replaced by a single space (TSV has no
//! quoting), so every row stays one line of exactly [`COLUMNS`] cells.

use crate::args::SanitizedPlaylistName;
use library_ipc_client::{ContentHash, PlaylistName, TrackInfo};
use rekordbox_xml::{RekordboxPlaylist, RekordboxTrack};
use std::collections::HashMap;
use track_matcher::{AmbiguityReason, MatchResult, NoMatchReason};

/// What the matcher said about each collection track.
pub(crate) type Results<'a> = [(&'a RekordboxTrack, MatchResult)];

/// The tracks that go into playlists and get enriched: definitive and weak matches.
pub(crate) fn matched<'a>(results: &Results<'a>) -> Vec<(&'a RekordboxTrack, ContentHash)> {
    results
        .iter()
        .filter_map(|(track, result)| match result {
            MatchResult::Definitive(hash, _) | MatchResult::Weak { hash, .. } => {
                Some((*track, hash.clone()))
            }
            MatchResult::Ambiguous(..) | MatchResult::NoMatch(_) => None,
        })
        .collect()
}

/// How many collection tracks are in each class. A track is in exactly one, so
/// [`Summary::total`] is the collection size. A weak match that is also shared is
/// counted as shared (its report has both rows).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) matched: usize,
    pub(crate) weak: usize,
    /// Tracks that matched a library track another rekordbox track matched too.
    pub(crate) shared: usize,
    pub(crate) ambiguous: usize,
    pub(crate) unmatched: usize,
    /// The library tracks those `shared` tracks matched.
    pub(crate) shared_groups: usize,
}

impl Summary {
    pub(crate) fn total(&self) -> usize {
        self.matched + self.weak + self.shared + self.ambiguous + self.unmatched
    }
}

/// The rekordbox tracks that matched each library track.
fn matched_by_hash<'r, 'a>(
    results: &'r Results<'a>,
) -> HashMap<&'r ContentHash, Vec<&'a RekordboxTrack>> {
    let mut groups: HashMap<&ContentHash, Vec<&RekordboxTrack>> = HashMap::new();
    for &(track, ref result) in results {
        if let MatchResult::Definitive(hash, _) | MatchResult::Weak { hash, .. } = result {
            groups.entry(hash).or_default().push(track);
        }
    }
    groups
}

pub(crate) fn summarize(results: &Results<'_>) -> Summary {
    let groups = matched_by_hash(results);
    let mut summary = Summary {
        shared_groups: groups.values().filter(|g| g.len() >= 2).count(),
        ..Summary::default()
    };
    for (_, result) in results {
        match result {
            MatchResult::Definitive(hash, _) | MatchResult::Weak { hash, .. }
                if groups[hash].len() >= 2 =>
            {
                summary.shared += 1;
            }
            MatchResult::Definitive(..) => summary.matched += 1,
            MatchResult::Weak { .. } => summary.weak += 1,
            MatchResult::Ambiguous(..) => summary.ambiguous += 1,
            MatchResult::NoMatch(_) => summary.unmatched += 1,
        }
    }
    summary
}

/// A library track, as far as it is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) hash: ContentHash,
    pub(crate) artist: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) duration_secs: Option<u32>,
    /// The library's (relative) blob path, when the projection has one.
    pub(crate) path: Option<String>,
    /// Why the details could not be fetched; shown in the row, never swallowed.
    pub(crate) lookup_error: Option<String>,
}

impl Candidate {
    /// Only the hash: the details were not asked for.
    pub(crate) fn bare(hash: ContentHash) -> Self {
        Self {
            hash,
            artist: None,
            title: None,
            duration_secs: None,
            path: None,
            lookup_error: None,
        }
    }

    pub(crate) fn failed(hash: ContentHash, error: String) -> Self {
        Self {
            lookup_error: Some(error),
            ..Self::bare(hash)
        }
    }

    pub(crate) fn from_track(track: &TrackInfo) -> Self {
        Self {
            hash: track.content_hash.clone(),
            artist: track.artist.clone(),
            title: track.title.clone(),
            duration_secs: track.duration.map(|d| d.value()),
            path: track.blob_path.clone(),
            lookup_error: None,
        }
    }

    pub(crate) fn lookup_failed(&self) -> bool {
        self.lookup_error.is_some()
    }
}

/// A rekordbox track the report lists, with why. The status is the variant.
pub(crate) enum Problem<'a> {
    Unmatched {
        track: &'a RekordboxTrack,
        reason: NoMatchReason,
    },
    Ambiguous {
        track: &'a RekordboxTrack,
        reason: AmbiguityReason,
        candidates: Vec<Candidate>,
    },
    Weak {
        track: &'a RekordboxTrack,
        library_secs: u32,
        candidate: Candidate,
    },
    Shared {
        track: &'a RekordboxTrack,
        /// How many rekordbox tracks matched this library track.
        sharers: usize,
        /// They all have the same `Location`: rekordbox's own duplicate entries of
        /// one file, which loses nothing.
        same_file: bool,
        candidate: Candidate,
    },
}

impl<'a> Problem<'a> {
    pub(crate) fn track(&self) -> &'a RekordboxTrack {
        match self {
            Self::Unmatched { track, .. }
            | Self::Ambiguous { track, .. }
            | Self::Weak { track, .. }
            | Self::Shared { track, .. } => track,
        }
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Unmatched { .. } => "unmatched",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Weak { .. } => "weak",
            Self::Shared { .. } => "shared",
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Unmatched { reason, .. } => reason.to_string(),
            Self::Ambiguous {
                reason, candidates, ..
            } => reason.describe(candidates.len()),
            Self::Weak {
                track,
                library_secs,
                ..
            } => format!(
                "duration off by {} s (library {} s, rekordbox {} s)",
                library_secs.abs_diff(track.total_time),
                library_secs,
                track.total_time
            ),
            Self::Shared {
                sharers, same_file, ..
            } => format!(
                "{sharers} rekordbox tracks matched this library track{}",
                if *same_file {
                    " (same file in rekordbox)"
                } else {
                    ""
                }
            ),
        }
    }

    pub(crate) fn candidates(&self) -> &[Candidate] {
        match self {
            Self::Unmatched { .. } => &[],
            Self::Ambiguous { candidates, .. } => candidates,
            Self::Weak { candidate, .. } | Self::Shared { candidate, .. } => {
                std::slice::from_ref(candidate)
            }
        }
    }
}

/// The report's track rows, from the match results. `describe` gives the details of
/// a library track (the CLI fetches them for a report only). Ordered by `TrackID`.
pub(crate) fn problems<'a>(
    results: &Results<'a>,
    mut describe: impl FnMut(&ContentHash) -> Candidate,
) -> Vec<Problem<'a>> {
    let mut out = Vec::new();
    for &(track, ref result) in results {
        match result {
            MatchResult::NoMatch(reason) => out.push(Problem::Unmatched {
                track,
                reason: *reason,
            }),
            MatchResult::Ambiguous(hashes, reason) => out.push(Problem::Ambiguous {
                track,
                reason: *reason,
                candidates: hashes.iter().map(&mut describe).collect(),
            }),
            MatchResult::Weak { hash, library_secs } => out.push(Problem::Weak {
                track,
                library_secs: *library_secs,
                candidate: describe(hash),
            }),
            MatchResult::Definitive(..) => {}
        }
    }

    // Many to one: every rekordbox track of a library track that two or more share.
    let groups = matched_by_hash(results);
    let mut described: HashMap<&ContentHash, Candidate> = HashMap::new();
    for &(track, ref result) in results {
        if let MatchResult::Definitive(hash, _) | MatchResult::Weak { hash, .. } = result {
            let sharing = &groups[hash];
            if sharing.len() >= 2 {
                let candidate = described
                    .entry(hash)
                    .or_insert_with(|| describe(hash))
                    .clone();
                out.push(Problem::Shared {
                    track,
                    sharers: sharing.len(),
                    same_file: sharing.iter().all(|t| t.location == sharing[0].location),
                    candidate,
                });
            }
        }
    }

    out.sort_by_key(|p| p.track().track_id);
    out
}

/// What became of a collection track, as far as playlists care.
#[derive(Clone, Copy)]
enum Fate {
    Written,
    Unmatched,
    Ambiguous,
}

/// How the entries of one rekordbox playlist fared. `written + dropped() == total`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub(crate) playlist: String,
    pub(crate) total: usize,
    pub(crate) written: usize,
    pub(crate) unmatched: usize,
    pub(crate) ambiguous: usize,
    /// The entry's `TrackID` has no `<TRACK>` in the COLLECTION.
    pub(crate) not_in_collection: usize,
}

impl Dropped {
    pub(crate) fn dropped(&self) -> usize {
        self.unmatched + self.ambiguous + self.not_in_collection
    }

    /// `4 of 12 entries (2 unmatched, 1 ambiguous, 1 not in collection)`
    pub(crate) fn describe(&self) -> String {
        format!(
            "{} of {} entries ({} unmatched, {} ambiguous, {} not in collection)",
            self.dropped(),
            self.total,
            self.unmatched,
            self.ambiguous,
            self.not_in_collection
        )
    }
}

/// Every playlist's entries, each counted once as written or dropped by cause. A
/// track listed twice counts twice. Callers show the ones with `dropped() > 0`.
pub(crate) fn tally_playlists(
    playlists: &[RekordboxPlaylist],
    results: &Results<'_>,
) -> Vec<Dropped> {
    let fates: HashMap<u32, Fate> = results
        .iter()
        .map(|(track, result)| {
            let fate = match result {
                MatchResult::Definitive(..) | MatchResult::Weak { .. } => Fate::Written,
                MatchResult::Ambiguous(..) => Fate::Ambiguous,
                MatchResult::NoMatch(_) => Fate::Unmatched,
            };
            (track.track_id, fate)
        })
        .collect();
    playlists
        .iter()
        .map(|playlist| {
            let mut tally = Dropped {
                playlist: playlist.name.clone(),
                total: playlist.track_ids.len(),
                written: 0,
                unmatched: 0,
                ambiguous: 0,
                not_in_collection: 0,
            };
            for id in &playlist.track_ids {
                match fates.get(id) {
                    Some(Fate::Written) => tally.written += 1,
                    Some(Fate::Unmatched) => tally.unmatched += 1,
                    Some(Fate::Ambiguous) => tally.ambiguous += 1,
                    None => tally.not_in_collection += 1,
                }
            }
            tally
        })
        .collect()
}

/// A playlist that is not imported at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedPlaylist {
    pub(crate) playlist: String,
    pub(crate) reason: &'static str,
}

/// Several rekordbox playlists that become the same MDMA playlist: rekordbox folders
/// are flattened and names are sanitised, and each write replaces the playlist
/// before it, so only the last survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameCollision {
    pub(crate) name: String,
    pub(crate) playlists: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NameIssues {
    pub(crate) skipped: Vec<SkippedPlaylist>,
    pub(crate) collisions: Vec<NameCollision>,
}

/// A rekordbox playlist as an `--all-playlists` import writes it.
pub(crate) struct PlannedPlaylist<'a> {
    pub(crate) source: &'a RekordboxPlaylist,
    pub(crate) target: SanitizedPlaylistName,
    /// The matched entries, in playlist order.
    pub(crate) hashes: Vec<ContentHash>,
}

/// What an `--all-playlists` import does with the rekordbox playlists: the one
/// decision, used both to write them and to report what goes wrong.
pub(crate) struct PlaylistPlan<'a> {
    pub(crate) writes: Vec<PlannedPlaylist<'a>>,
    /// Playlists with no matched entry: nothing to write (their entries are in the
    /// [`Dropped`] tallies).
    pub(crate) nothing_matched: Vec<&'a RekordboxPlaylist>,
    pub(crate) issues: NameIssues,
}

/// Plan the playlists. `import_target` is the `--playlist NAME` the import writes as
/// well: a rekordbox playlist that becomes the same name collides with it.
pub(crate) fn plan_playlists<'a>(
    playlists: &'a [RekordboxPlaylist],
    results: &Results<'_>,
    import_target: Option<&PlaylistName>,
) -> PlaylistPlan<'a> {
    let hash_of: HashMap<u32, ContentHash> = matched(results)
        .into_iter()
        .map(|(track, hash)| (track.track_id, hash))
        .collect();
    let mut plan = PlaylistPlan {
        writes: Vec::new(),
        nothing_matched: Vec::new(),
        issues: NameIssues::default(),
    };
    let mut by_name: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(name) = import_target {
        by_name.push((
            name.as_str().to_string(),
            vec![format!("--playlist {}", name.as_str())],
        ));
    }
    for source in playlists {
        let hashes: Vec<ContentHash> = source
            .track_ids
            .iter()
            .filter_map(|id| hash_of.get(id).cloned())
            .collect();
        if hashes.is_empty() {
            plan.nothing_matched.push(source);
            continue;
        }
        match SanitizedPlaylistName::new(&source.name) {
            Err(reason) => plan.issues.skipped.push(SkippedPlaylist {
                playlist: source.name.clone(),
                reason,
            }),
            Ok(target) => {
                let name = target.name().as_str();
                match by_name.iter_mut().find(|entry| entry.0 == name) {
                    Some(entry) => entry.1.push(source.name.clone()),
                    None => by_name.push((name.to_string(), vec![source.name.clone()])),
                }
                plan.writes.push(PlannedPlaylist {
                    source,
                    target,
                    hashes,
                });
            }
        }
    }
    plan.issues.collisions = by_name
        .into_iter()
        .filter(|(_, raw)| raw.len() >= 2)
        .map(|(name, playlists)| NameCollision { name, playlists })
        .collect();
    plan
}

/// Everything the report lists.
pub(crate) struct Report<'a> {
    pub(crate) problems: Vec<Problem<'a>>,
    pub(crate) playlists: Vec<Dropped>,
    pub(crate) issues: NameIssues,
}

const COLUMNS: [&str; 14] = [
    "status",
    "reason",
    "playlist",
    "track_id",
    "artist",
    "title",
    "album",
    "duration_s",
    "location",
    "candidate_hash",
    "candidate_artist",
    "candidate_title",
    "candidate_duration_s",
    "candidate_path",
];

/// One line of the report. [`Row::to_cells`] returns a fixed-width array, so a column
/// added here that is not in the header, or the other way round, does not compile.
#[derive(Default)]
struct Row {
    status: String,
    reason: String,
    playlist: String,
    track_id: String,
    artist: String,
    title: String,
    album: String,
    duration_s: String,
    location: String,
    candidate_hash: String,
    candidate_artist: String,
    candidate_title: String,
    candidate_duration_s: String,
    candidate_path: String,
}

impl Row {
    fn of_track(status: &str, reason: String, t: &RekordboxTrack) -> Self {
        Self {
            status: status.to_string(),
            reason,
            track_id: t.track_id.to_string(),
            artist: t.artist.clone(),
            title: t.name.clone(),
            album: t.album.clone(),
            duration_s: t.total_time.to_string(),
            location: location_path(t),
            ..Self::default()
        }
    }

    fn of_playlist(status: &str, reason: String, playlist: &str) -> Self {
        Self {
            status: status.to_string(),
            reason,
            playlist: playlist.to_string(),
            ..Self::default()
        }
    }

    fn with_candidate(self, c: &Candidate) -> Self {
        Self {
            candidate_hash: c.hash.as_str().to_string(),
            candidate_artist: c.artist.clone().unwrap_or_default(),
            candidate_title: c.title.clone().unwrap_or_default(),
            candidate_duration_s: c.duration_secs.map(|d| d.to_string()).unwrap_or_default(),
            candidate_path: match &c.lookup_error {
                Some(error) => format!("<lookup failed: {error}>"),
                None => c.path.clone().unwrap_or_default(),
            },
            ..self
        }
    }

    /// In [`COLUMNS`] order, with tabs and line breaks made spaces.
    fn to_cells(&self) -> [String; 14] {
        let Self {
            status,
            reason,
            playlist,
            track_id,
            artist,
            title,
            album,
            duration_s,
            location,
            candidate_hash,
            candidate_artist,
            candidate_title,
            candidate_duration_s,
            candidate_path,
        } = self;
        [
            status,
            reason,
            playlist,
            track_id,
            artist,
            title,
            album,
            duration_s,
            location,
            candidate_hash,
            candidate_artist,
            candidate_title,
            candidate_duration_s,
            candidate_path,
        ]
        .map(|s| s.replace(['\t', '\r', '\n'], " "))
    }
}

/// The decoded file path of a rekordbox `Location`; the raw value if it is not a
/// `file://` URI.
pub(crate) fn location_path(track: &RekordboxTrack) -> String {
    rekordbox_xml::parse_location(&track.location).unwrap_or_else(|| track.location.clone())
}

/// The report: header, then the tracks, then the playlist rows.
pub(crate) fn render_tsv(report: &Report) -> String {
    let mut rows: Vec<Row> = Vec::new();

    for p in &report.problems {
        let base = || Row::of_track(p.status(), p.reason(), p.track());
        match p.candidates() {
            [] => rows.push(base()),
            candidates => rows.extend(candidates.iter().map(|c| base().with_candidate(c))),
        }
    }
    for d in report.playlists.iter().filter(|d| d.dropped() > 0) {
        rows.push(Row::of_playlist(
            "dropped-from-playlist",
            d.describe(),
            &d.playlist,
        ));
    }
    for s in &report.issues.skipped {
        rows.push(Row::of_playlist(
            "skipped-playlist",
            format!("not imported: {}", s.reason),
            &s.playlist,
        ));
    }
    for c in &report.issues.collisions {
        rows.push(Row::of_playlist(
            "playlist-name-collision",
            format!(
                "{} rekordbox playlists become this MDMA playlist, each replacing the last: {}",
                c.playlists.len(),
                c.playlists.join(", ")
            ),
            &c.name,
        ));
    }

    let mut out = COLUMNS.join("\t");
    out.push('\n');
    for row in rows {
        out.push_str(&row.to_cells().join("\t"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use track_matcher::MatchConfidence;

    fn rb(id: u32, artist: &str, name: &str) -> RekordboxTrack {
        RekordboxTrack {
            track_id: id,
            artist: artist.to_string(),
            name: name.to_string(),
            album: "Album".to_string(),
            total_time: 300,
            location: "file://localhost/music/A%20B/track.flac".to_string(),
            ..RekordboxTrack::default()
        }
    }

    fn playlist(name: &str, ids: &[u32]) -> RekordboxPlaylist {
        RekordboxPlaylist {
            name: name.to_string(),
            track_ids: ids.to_vec(),
        }
    }

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    fn definitive(s: &str) -> MatchResult {
        MatchResult::Definitive(hash(s), MatchConfidence::ArtistTitleDuration)
    }

    fn unmatched() -> MatchResult {
        MatchResult::NoMatch(NoMatchReason::NoTrackWithArtistTitle)
    }

    fn bare(h: &ContentHash) -> Candidate {
        Candidate::bare(h.clone())
    }

    fn rows(tsv: &str) -> Vec<Vec<&str>> {
        tsv.lines().map(|l| l.split('\t').collect()).collect()
    }

    fn report<'a>(problems: Vec<Problem<'a>>) -> Report<'a> {
        Report {
            problems,
            playlists: vec![],
            issues: NameIssues::default(),
        }
    }

    // ── rows ────────────────────────────────────────────────────────────────────

    #[test]
    fn unmatched_track_is_one_row_with_decoded_location() {
        let t = rb(7, "Artist", "Title");
        let results = [(&t, unmatched())];
        let tsv = render_tsv(&report(problems(&results, bare)));
        let r = rows(&tsv);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], COLUMNS);
        assert_eq!(
            r[1],
            [
                "unmatched",
                "no track with this artist+title",
                "",
                "7",
                "Artist",
                "Title",
                "Album",
                "300",
                "/music/A B/track.flac",
                "",
                "",
                "",
                "",
                ""
            ]
        );
    }

    #[test]
    fn ambiguous_track_repeats_its_row_per_candidate() {
        let t = rb(8, "A", "T");
        let results = [(
            &t,
            MatchResult::Ambiguous(
                vec![hash("sha256:aa"), hash("sha256:bb")],
                AmbiguityReason::WithinTolerance,
            ),
        )];
        let ps = problems(&results, |h| {
            if h.as_str() == "sha256:aa" {
                Candidate {
                    artist: Some("A".into()),
                    title: Some("T".into()),
                    duration_secs: Some(299),
                    path: Some("blobs/aa.flac".into()),
                    ..Candidate::bare(h.clone())
                }
            } else {
                Candidate::bare(h.clone())
            }
        });
        let tsv = render_tsv(&report(ps));
        let r = rows(&tsv);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1][0], "ambiguous");
        assert_eq!(r[1][1], "2 candidates within 2 s duration tolerance");
        assert_eq!(r[1][3], "8");
        assert_eq!(r[1][9..], ["sha256:aa", "A", "T", "299", "blobs/aa.flac"]);
        assert_eq!(r[2][3], "8");
        assert_eq!(r[2][9..], ["sha256:bb", "", "", "", ""]);
    }

    #[test]
    fn weak_track_names_both_durations_and_the_library_track() {
        let t = rb(3, "A", "T"); // 300 s in rekordbox
        let results = [(
            &t,
            MatchResult::Weak {
                hash: hash("sha256:aa"),
                library_secs: 210,
            },
        )];
        let tsv = render_tsv(&report(problems(&results, bare)));
        let r = rows(&tsv);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1][0], "weak");
        assert_eq!(
            r[1][1],
            "duration off by 90 s (library 210 s, rekordbox 300 s)"
        );
        assert_eq!(r[1][9], "sha256:aa");
    }

    #[test]
    fn tracks_sharing_a_library_track_each_get_a_shared_row() {
        let (a, b, c) = (rb(1, "a", "a"), rb(2, "a", "a"), rb(3, "c", "c"));
        let results = [
            (&a, definitive("sha256:one")),
            (&b, definitive("sha256:one")),
            (&c, definitive("sha256:two")),
        ];
        let ps = problems(&results, bare);
        let tsv = render_tsv(&report(ps));
        let r = rows(&tsv);
        assert_eq!(r.len(), 3, "{tsv}");
        for (row, id) in r[1..].iter().zip(["1", "2"]) {
            assert_eq!(row[0], "shared");
            assert_eq!(
                row[1],
                "2 rekordbox tracks matched this library track (same file in rekordbox)"
            );
            assert_eq!(row[3], id);
            assert_eq!(row[9], "sha256:one");
        }
    }

    #[test]
    fn a_weak_track_that_is_also_shared_has_both_rows() {
        let (a, b) = (rb(1, "a", "a"), rb(2, "a", "a"));
        let results = [
            (
                &a,
                MatchResult::Weak {
                    hash: hash("sha256:one"),
                    library_secs: 100,
                },
            ),
            (&b, definitive("sha256:one")),
        ];
        let statuses: Vec<_> = problems(&results, bare)
            .iter()
            .map(|p| p.status())
            .collect();
        assert_eq!(statuses, ["weak", "shared", "shared"]);
    }

    #[test]
    fn a_failed_candidate_lookup_is_marked_in_the_row() {
        let t = rb(1, "A", "T");
        let results = [(
            &t,
            MatchResult::Ambiguous(
                vec![hash("sha256:aa"), hash("sha256:bb")],
                AmbiguityReason::Isrc,
            ),
        )];
        let ps = problems(&results, |h| {
            Candidate::failed(h.clone(), "no\nroute".into())
        });
        assert!(ps
            .iter()
            .all(|p| p.candidates().iter().all(Candidate::lookup_failed)));
        let tsv = render_tsv(&report(ps));
        let r = rows(&tsv);
        assert_eq!(r[1][13], "<lookup failed: no route>");
        assert_eq!(r[1][9], "sha256:aa");
    }

    // ── escaping ────────────────────────────────────────────────────────────────

    #[test]
    fn tabs_and_newlines_in_fields_become_spaces() {
        let mut t = rb(9, "Art\tist", "Ti\ntle\r");
        t.album = "a\tb\nc".to_string();
        let results = [(&t, unmatched())];
        let tsv = render_tsv(&report(problems(&results, bare)));
        assert_eq!(tsv.lines().count(), 2);
        let r = rows(&tsv);
        assert!(r.iter().all(|row| row.len() == 14), "{r:?}");
        assert_eq!(r[1][4], "Art ist");
        assert_eq!(r[1][5], "Ti tle ");
        assert_eq!(r[1][6], "a b c");
    }

    #[test]
    fn candidate_fields_are_escaped_too() {
        let t = rb(1, "A", "T");
        let results = [(
            &t,
            MatchResult::Ambiguous(
                vec![hash("sha256:aa"), hash("sha256:bb")],
                AmbiguityReason::Isrc,
            ),
        )];
        let ps = problems(&results, |h| Candidate {
            artist: Some("x\ty".into()),
            title: Some("p\nq".into()),
            ..Candidate::bare(h.clone())
        });
        let tsv = render_tsv(&report(ps));
        let r = rows(&tsv);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|row| row.len() == 14));
        assert_eq!(r[1][10], "x y");
        assert_eq!(r[1][11], "p q");
    }

    #[test]
    fn location_that_is_not_a_file_uri_is_kept_raw() {
        let mut t = rb(1, "A", "T");
        t.location = "http://example.com/x.mp3".to_string();
        assert_eq!(location_path(&t), "http://example.com/x.mp3");
    }

    // ── playlists ───────────────────────────────────────────────────────────────

    #[test]
    fn playlist_entries_are_counted_by_cause() {
        let (a, b, c) = (rb(1, "a", "a"), rb(2, "b", "b"), rb(3, "c", "c"));
        let results = [
            (&a, unmatched()),
            (
                &b,
                MatchResult::Ambiguous(vec![hash("x"), hash("y")], AmbiguityReason::Isrc),
            ),
            (&c, definitive("sha256:c")),
        ];
        // 99 has no <TRACK> in the collection. Track 1 is listed twice: counts twice.
        let playlists = [
            playlist("set", &[1, 2, 3, 1, 99]),
            playlist("clean", &[3]),
            playlist("empty", &[]),
        ];
        let tallies = tally_playlists(&playlists, &results);
        assert_eq!(
            tallies[0],
            Dropped {
                playlist: "set".into(),
                total: 5,
                written: 1,
                unmatched: 2,
                ambiguous: 1,
                not_in_collection: 1
            }
        );
        assert_eq!(tallies[1].dropped(), 0);
        assert_eq!(tallies[2].total, 0);
        assert_eq!(
            tallies[0].describe(),
            "4 of 5 entries (2 unmatched, 1 ambiguous, 1 not in collection)"
        );
    }

    #[test]
    fn dropped_rows_use_the_same_columns_with_the_playlist_name() {
        let rep = Report {
            problems: vec![],
            playlists: vec![
                Dropped {
                    playlist: "My\tSet".into(),
                    total: 12,
                    written: 9,
                    unmatched: 2,
                    ambiguous: 1,
                    not_in_collection: 0,
                },
                Dropped {
                    playlist: "clean".into(),
                    total: 3,
                    written: 3,
                    unmatched: 0,
                    ambiguous: 0,
                    not_in_collection: 0,
                },
            ],
            issues: NameIssues::default(),
        };
        let tsv = render_tsv(&rep);
        let r = rows(&tsv);
        assert_eq!(r.len(), 2, "playlists that lose nothing have no row");
        assert!(r.iter().all(|row| row.len() == 14));
        assert_eq!(r[1][0], "dropped-from-playlist");
        assert_eq!(
            r[1][1],
            "3 of 12 entries (2 unmatched, 1 ambiguous, 0 not in collection)"
        );
        assert_eq!(r[1][2], "My Set");
        assert!(r[1][3..].iter().all(|c| c.is_empty()));
    }

    #[test]
    fn invalid_names_are_skipped_and_same_names_collide() {
        let a = rb(1, "a", "a");
        let results = [(&a, definitive("sha256:a"))];
        let playlists = [
            playlist("///", &[1]),              // sanitizes to nothing: skipped
            playlist("My Set", &[1]),           // two folders' playlists ...
            playlist("My-Set", &[1]),           // ... and a differently spelled one
            playlist("Other", &[1]),            // fine
            playlist("Unmatched only", &[404]), // nothing to write: not a name issue
            playlist("???", &[404]),            // same, and its name is invalid too
        ];
        let plan = plan_playlists(&playlists, &results, None);
        let issues = plan.issues;
        let written: Vec<_> = plan
            .writes
            .iter()
            .map(|w| w.target.name().as_str())
            .collect();
        assert_eq!(written, ["My-Set", "My-Set", "Other"]);
        let empty: Vec<_> = plan
            .nothing_matched
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(empty, ["Unmatched only", "???"]);
        assert_eq!(
            issues.skipped,
            [SkippedPlaylist {
                playlist: "///".into(),
                reason: "name sanitizes to empty"
            }]
        );
        assert_eq!(
            issues.collisions,
            [NameCollision {
                name: "My-Set".into(),
                playlists: vec!["My Set".into(), "My-Set".into()]
            }]
        );

        let rep = Report {
            problems: vec![],
            playlists: vec![],
            issues,
        };
        let tsv = render_tsv(&rep);
        let r = rows(&tsv);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1][0], "skipped-playlist");
        assert_eq!(r[1][1], "not imported: name sanitizes to empty");
        assert_eq!(r[1][2], "///");
        assert_eq!(r[2][0], "playlist-name-collision");
        assert_eq!(r[2][2], "My-Set");
        assert!(r[2][1].starts_with("2 rekordbox playlists become this MDMA playlist"));
        assert!(r[2][1].ends_with("My Set, My-Set"));
    }

    #[test]
    fn the_playlist_option_target_collides_with_a_rekordbox_playlist() {
        let a = rb(1, "a", "a");
        let results = [(&a, definitive("sha256:a"))];
        let playlists = [playlist("My Set", &[1]), playlist("Other", &[1])];
        let target = PlaylistName::new("My-Set").unwrap();
        let plan = plan_playlists(&playlists, &results, Some(&target));
        assert_eq!(
            plan.issues.collisions,
            [NameCollision {
                name: "My-Set".into(),
                playlists: vec!["--playlist My-Set".into(), "My Set".into()]
            }]
        );
        // Without it, nothing collides.
        assert!(plan.writes.len() == 2);
        assert!(plan_playlists(&playlists, &results, None)
            .issues
            .collisions
            .is_empty());
    }

    #[test]
    fn the_plan_keeps_matched_entries_in_playlist_order() {
        let (a, b, c) = (rb(1, "a", "a"), rb(2, "b", "b"), rb(3, "c", "c"));
        let results = [
            (&a, definitive("sha256:a")),
            (&b, unmatched()),
            (&c, definitive("sha256:c")),
        ];
        let playlists = [playlist("set", &[3, 2, 1, 99, 3])];
        let plan = plan_playlists(&playlists, &results, None);
        assert_eq!(
            plan.writes[0].hashes,
            [hash("sha256:c"), hash("sha256:a"), hash("sha256:c")]
        );
    }

    #[test]
    fn sharing_the_same_location_is_called_out() {
        let (a, mut b) = (rb(1, "a", "a"), rb(2, "a", "a"));
        let results = [
            (&a, definitive("sha256:one")),
            (&b, definitive("sha256:one")),
        ];
        let same = problems(&results, bare);
        assert_eq!(
            same[0].reason(),
            "2 rekordbox tracks matched this library track (same file in rekordbox)"
        );
        b.location = "file://localhost/music/other.flac".to_string();
        let results = [
            (&a, definitive("sha256:one")),
            (&b, definitive("sha256:one")),
        ];
        let different = problems(&results, bare);
        assert_eq!(
            different[0].reason(),
            "2 rekordbox tracks matched this library track"
        );
    }

    #[test]
    fn no_problems_is_just_the_header() {
        assert_eq!(
            render_tsv(&report(vec![])),
            format!("{}\n", COLUMNS.join("\t"))
        );
    }

    // ── completeness ────────────────────────────────────────────────────────────

    /// The report exists so that nothing rekordbox has silently fails to reach MDMA:
    /// every collection track is in exactly one class and every playlist entry is
    /// written or dropped for a counted cause.
    #[test]
    fn every_track_and_every_playlist_entry_is_accounted_for() {
        let tracks: Vec<RekordboxTrack> = (1..=8).map(|i| rb(i, "a", "t")).collect();
        let results = [
            (&tracks[0], definitive("sha256:one")), // matched
            (
                &tracks[1],
                MatchResult::Weak {
                    hash: hash("sha256:two"),
                    library_secs: 100,
                },
            ), // weak
            (&tracks[2], definitive("sha256:shared")), // shared ...
            (&tracks[3], definitive("sha256:shared")), // ... with this one
            (
                &tracks[4],
                MatchResult::Ambiguous(
                    vec![hash("x"), hash("y")],
                    AmbiguityReason::WithinTolerance,
                ),
            ),
            (&tracks[5], unmatched()),
            (
                &tracks[6],
                MatchResult::NoMatch(NoMatchReason::NoArtistOrTitle),
            ),
            (&tracks[7], definitive("sha256:eight")), // matched
        ];

        let summary = summarize(&results);
        assert_eq!(
            summary,
            Summary {
                matched: 2,
                weak: 1,
                shared: 2,
                ambiguous: 1,
                unmatched: 2,
                shared_groups: 1
            }
        );
        assert_eq!(summary.total(), tracks.len());
        assert_eq!(
            matched(&results).len(),
            5,
            "matched + weak + shared are written"
        );

        // Entry 404 has no <TRACK> in the collection.
        let playlists = [
            playlist("a", &[1, 2, 3, 4, 5, 6, 7, 8]),
            playlist("b", &[5, 5, 404, 1]),
            playlist("c", &[]),
        ];
        let tallies = tally_playlists(&playlists, &results);
        for (p, t) in playlists.iter().zip(&tallies) {
            assert_eq!(t.written + t.dropped(), p.track_ids.len(), "{}", p.name);
            assert_eq!(t.total, p.track_ids.len());
        }
        let entries: usize = playlists.iter().map(|p| p.track_ids.len()).sum();
        let written: usize = tallies.iter().map(|t| t.written).sum();
        let dropped: usize = tallies.iter().map(Dropped::dropped).sum();
        assert_eq!(written + dropped, entries);
        assert_eq!(tallies[1].not_in_collection, 1);
    }
}
