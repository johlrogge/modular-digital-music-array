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
//! - `placeholder-grid` rows: a matched track whose only TEMPO is the placeholder that
//!   old MDMA exports wrote (one anchor at 0.000, beat 1); its grid is not imported.
//!   An annotation on a matched track, not a class of [`Summary`].
//! - `variable-tempo` rows: a matched track with several TEMPO anchors that the first
//!   anchor's grid does not reproduce (another Bpm or Metro, more than 5 ms off its
//!   beats, or a Battito that breaks the bar position); no grid is imported.
//! - `grid-conflict` rows: rekordbox tracks matching one library track have different
//!   real grids; none is imported. One row per library track, listing every candidate.
//! - `dropped-from-playlist` rows are per playlist, with the counts in `reason`.
//! - `skipped-playlist` rows: a playlist that is not imported at all (bad name).
//! - `playlist-name-collision` rows: several rekordbox playlists become one MDMA
//!   playlist, each replacing the last. The track columns are empty on playlist rows.
//!
//! A tab, CR or LF inside a field is replaced by a single space (TSV has no
//! quoting), so every row stays one line of exactly [`COLUMNS`] cells.

use crate::args::SanitizedPlaylistName;
use library_ipc_client::{Bpm, ContentHash, PlaylistName, TrackInfo};
use music_facts::MusicValue;
use rekordbox_xml::xml::{Deviation, DeviationCause, TempoKind};
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
///
/// `placeholder_grids`, `variable_tempos` and `grid_conflicts` are annotations, not
/// classes: a track with a placeholder grid is matched (or weak, or shared) like any
/// other and is in one of the classes above as well, so they are not part of
/// [`Summary::total`].
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
    /// Matched tracks (any of matched, weak, shared) whose only TEMPO is the
    /// placeholder of old MDMA exports; their grid is not imported.
    pub(crate) placeholder_grids: usize,
    /// Library tracks whose rekordbox copies disagree on the grid; no grid is imported.
    pub(crate) grid_conflicts: usize,
    /// Matched tracks with a variable tempo (several TEMPO anchors that the first
    /// anchor's grid does not reproduce); their grid is not imported.
    pub(crate) variable_tempos: usize,
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

/// The grid the import writes for a library track, decided once from every
/// rekordbox track that matched it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GridChoice<'a> {
    /// No usable grid: no TEMPO, or only placeholders. MDMA's grid stays.
    Keep,
    /// Exactly one distinct real grid among the copies.
    Write(MusicValue),
    /// Two or more distinct real grids: none is written. The rekordbox tracks that
    /// have a real grid, in collection order.
    Conflict(Vec<&'a RekordboxTrack>),
    /// At least one copy has a variable tempo that no single grid describes (the
    /// first anchor's grid would be a truncation): no grid is written, whatever the
    /// other copies say. The variable copies, in collection order.
    Variable(Vec<VariableCopy<'a>>),
}

/// A rekordbox track whose several TEMPO anchors are not one constant grid.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableCopy<'a> {
    pub(crate) track: &'a RekordboxTrack,
    pub(crate) deviation: Deviation,
    /// The grid its first anchor alone would give: what an earlier import may have
    /// written to the library, and what must go if it is still there.
    pub(crate) first_anchor_grid: Option<MusicValue>,
}

/// What one rekordbox track says about the grid.
enum TrackGrid<'a> {
    /// No TEMPO, or only the placeholder: no information.
    Nothing,
    /// One anchor, or several the first anchor's grid reproduces.
    Grid(MusicValue),
    Variable(VariableCopy<'a>),
}

fn first_anchor_grid(track: &RekordboxTrack) -> Option<MusicValue> {
    let average_bpm = track.average_bpm.and_then(|b| Bpm::from_f32(b).ok());
    rekordbox_roundtrip::grid_from_tempo(track.tempo.first()?, average_bpm)
}

fn track_grid(track: &RekordboxTrack) -> TrackGrid<'_> {
    match track.tempo.classify(track.average_bpm) {
        TempoKind::Absent | TempoKind::Placeholder => TrackGrid::Nothing,
        TempoKind::Constant => first_anchor_grid(track).map_or(TrackGrid::Nothing, TrackGrid::Grid),
        TempoKind::Variable(deviation) => TrackGrid::Variable(VariableCopy {
            track,
            deviation,
            first_anchor_grid: first_anchor_grid(track),
        }),
    }
}

fn has_placeholder_grid(track: &RekordboxTrack) -> bool {
    track.tempo.is_placeholder()
}

fn variable_deviation(track: &RekordboxTrack) -> Option<Deviation> {
    match track.tempo.classify(track.average_bpm) {
        TempoKind::Variable(deviation) => Some(deviation),
        _ => None,
    }
}

/// Placeholders are ignored. A variable-tempo copy means no grid. Otherwise one
/// distinct grid is used and several are a conflict.
pub(crate) fn choose_grid<'a>(copies: &[&'a RekordboxTrack]) -> GridChoice<'a> {
    let mut grids: Vec<(&'a RekordboxTrack, MusicValue)> = Vec::new();
    let mut variable: Vec<VariableCopy<'a>> = Vec::new();
    for &t in copies {
        match track_grid(t) {
            TrackGrid::Nothing => {}
            TrackGrid::Grid(g) => grids.push((t, g)),
            TrackGrid::Variable(v) => variable.push(v),
        }
    }
    if !variable.is_empty() {
        return GridChoice::Variable(variable);
    }
    let mut distinct: Vec<&MusicValue> = Vec::new();
    for (_, grid) in &grids {
        if !distinct.contains(&grid) {
            distinct.push(grid);
        }
    }
    match distinct.as_slice() {
        [] => GridChoice::Keep,
        [one] => GridChoice::Write((*one).clone()),
        _ => GridChoice::Conflict(grids.into_iter().map(|(t, _)| t).collect()),
    }
}

/// [`choose_grid`] for each library track the results matched, over the same
/// grouping as the `shared` rows.
pub(crate) fn grid_choices<'r, 'a>(
    results: &'r Results<'a>,
) -> HashMap<&'r ContentHash, GridChoice<'a>> {
    matched_by_hash(results)
        .into_iter()
        .map(|(hash, copies)| (hash, choose_grid(&copies)))
        .collect()
}

pub(crate) fn summarize(results: &Results<'_>) -> Summary {
    let groups = matched_by_hash(results);
    let mut summary = Summary {
        shared_groups: groups.values().filter(|g| g.len() >= 2).count(),
        grid_conflicts: groups
            .values()
            .filter(|g| matches!(choose_grid(g), GridChoice::Conflict(_)))
            .count(),
        ..Summary::default()
    };
    for (track, result) in results {
        if matches!(
            result,
            MatchResult::Definitive(..) | MatchResult::Weak { .. }
        ) {
            if has_placeholder_grid(track) {
                summary.placeholder_grids += 1;
            }
            if variable_deviation(track).is_some() {
                summary.variable_tempos += 1;
            }
        }
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
    /// A matched track whose only TEMPO is the placeholder of old MDMA exports. The
    /// track is still matched and enriched; only its grid is not imported.
    PlaceholderGrid { track: &'a RekordboxTrack },
    /// Rekordbox tracks matching one library track disagree on the grid; none is
    /// imported. One row per library track, on its first copy with a real grid.
    GridConflict {
        track: &'a RekordboxTrack,
        /// Every copy with a real grid, in collection order.
        copies: Vec<&'a RekordboxTrack>,
        candidate: Candidate,
    },
    /// A matched track with several TEMPO anchors that the first anchor's grid does
    /// not reproduce: no single grid describes it, so none is imported (and
    /// rekordbox keeps its own on export).
    VariableTempo {
        track: &'a RekordboxTrack,
        deviation: Deviation,
    },
}

/// `TrackID 12 /music/a.aiff (Inizio 0.018 Battito 1 Bpm 128.00)`: the first anchor.
fn describe_grid_copy(track: &RekordboxTrack) -> String {
    let anchor = track.tempo.first();
    let bpm = anchor
        .and_then(|a| a.bpm)
        .or(track.average_bpm)
        .map_or_else(|| "?".to_string(), |b| format!("{b:.2}"));
    format!(
        "TrackID {} {} (Inizio {:.3} Battito {} Bpm {})",
        track.track_id,
        location_path(track),
        anchor.map_or(0.0, |a| a.inizio_seconds),
        anchor.map_or(1, |a| a.beat_in_bar()),
        bpm
    )
}

fn describe_variable_tempo(track: &RekordboxTrack, deviation: &Deviation) -> String {
    let cause = match deviation.cause {
        DeviationCause::Bpm => "changes the Bpm",
        DeviationCause::Meter => "changes the Metro",
        DeviationCause::OffGrid => "is off the first anchor's beat grid",
        DeviationCause::BarPosition => "has a Battito that breaks the bar position",
    };
    format!(
        "variable tempo: {} TEMPO anchors, anchor {} {} (worst offset {:.1} ms from the \
         first anchor's grid); grid not imported",
        track.tempo.anchors().len(),
        deviation.anchor + 1,
        cause,
        deviation.worst_offset_ms
    )
}

impl<'a> Problem<'a> {
    pub(crate) fn track(&self) -> &'a RekordboxTrack {
        match self {
            Self::Unmatched { track, .. }
            | Self::Ambiguous { track, .. }
            | Self::Weak { track, .. }
            | Self::Shared { track, .. }
            | Self::PlaceholderGrid { track }
            | Self::GridConflict { track, .. }
            | Self::VariableTempo { track, .. } => track,
        }
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Unmatched { .. } => "unmatched",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Weak { .. } => "weak",
            Self::Shared { .. } => "shared",
            Self::PlaceholderGrid { .. } => "placeholder-grid",
            Self::GridConflict { .. } => "grid-conflict",
            Self::VariableTempo { .. } => "variable-tempo",
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
            Self::PlaceholderGrid { .. } => "single TEMPO at 0.000, beat 1 (MDMA export \
                                             placeholder); grid not imported"
                .to_string(),
            Self::GridConflict { copies, .. } => format!(
                "{} rekordbox tracks of this library track have different grids; no grid \
                 imported: {}",
                copies.len(),
                copies
                    .iter()
                    .map(|t| describe_grid_copy(t))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::VariableTempo { track, deviation } => describe_variable_tempo(track, deviation),
        }
    }

    pub(crate) fn candidates(&self) -> &[Candidate] {
        match self {
            Self::Unmatched { .. } | Self::PlaceholderGrid { .. } | Self::VariableTempo { .. } => {
                &[]
            }
            Self::Ambiguous { candidates, .. } => candidates,
            Self::Weak { candidate, .. }
            | Self::Shared { candidate, .. }
            | Self::GridConflict { candidate, .. } => std::slice::from_ref(candidate),
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
    let grids = grid_choices(results);
    let mut described: HashMap<&ContentHash, Candidate> = HashMap::new();
    for &(track, ref result) in results {
        if let MatchResult::Definitive(hash, _) | MatchResult::Weak { hash, .. } = result {
            if has_placeholder_grid(track) {
                out.push(Problem::PlaceholderGrid { track });
            }
            if let Some(deviation) = variable_deviation(track) {
                out.push(Problem::VariableTempo { track, deviation });
            }
            // One conflict row per library track, on its first copy with a real grid.
            if let GridChoice::Conflict(copies) = &grids[hash] {
                if std::ptr::eq(copies[0], track) {
                    let candidate = described
                        .entry(hash)
                        .or_insert_with(|| describe(hash))
                        .clone();
                    out.push(Problem::GridConflict {
                        track,
                        copies: copies.clone(),
                        candidate,
                    });
                }
            }
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
    use rekordbox_xml::xml::{TempoAnchor, TempoSet};
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

    /// The track with a single TEMPO (4/4, 128 BPM) at `inizio` on beat `battito`.
    fn with_tempo(t: RekordboxTrack, inizio: f64, battito: u8) -> RekordboxTrack {
        RekordboxTrack {
            tempo: TempoSet::single(anchor_at(inizio, battito)),
            ..t
        }
    }

    fn anchor_at(inizio: f64, battito: u8) -> TempoAnchor {
        TempoAnchor {
            inizio_seconds: inizio,
            bpm: Some(128.0),
            metro_numerator: std::num::NonZeroU8::new(4),
            battito: Some(battito),
        }
    }

    /// 128 BPM, 4/4: a beat is 468.75 ms, a bar 1.875 s. The second anchor sits 32
    /// bars (60 s) on, on the grid with bar position 1 when `drift_s` is 0.
    fn with_two_anchors(t: RekordboxTrack, drift_s: f64) -> RekordboxTrack {
        RekordboxTrack {
            tempo: TempoSet::new(vec![anchor_at(0.0, 1), anchor_at(60.0 + drift_s, 1)]),
            ..t
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

    // ── grids ───────────────────────────────────────────────────────────────────

    fn grid_at(ms: u32) -> MusicValue {
        MusicValue::BeatGrid {
            first_beat_ms: ms,
            bpm: Bpm::from_f32(128.0).unwrap(),
            beats_per_bar: 4,
        }
    }

    #[test]
    fn a_placeholder_grid_is_a_row_on_a_matched_track_and_nothing_more() {
        let t = with_tempo(rb(7, "A", "T"), 0.0, 1);
        let results = [(&t, definitive("sha256:one"))];
        let ps = problems(&results, bare);
        let tsv = render_tsv(&report(ps));
        let r = rows(&tsv);
        assert_eq!(r.len(), 2, "{tsv}");
        assert_eq!(r[1][0], "placeholder-grid");
        assert_eq!(
            r[1][1],
            "single TEMPO at 0.000, beat 1 (MDMA export placeholder); grid not imported"
        );
        assert_eq!(r[1][3], "7");
        assert_eq!(choose_grid(&[&t]), GridChoice::Keep);
    }

    #[test]
    fn a_real_grid_is_not_reported() {
        let t = with_tempo(rb(7, "A", "T"), 0.018, 1);
        let results = [(&t, definitive("sha256:one"))];
        assert!(problems(&results, bare).is_empty());
        assert_eq!(choose_grid(&[&t]), GridChoice::Write(grid_at(18)));
    }

    #[test]
    fn a_placeholder_copy_yields_to_the_real_copy_whatever_the_order() {
        let placeholder = with_tempo(rb(1, "a", "a"), 0.0, 1);
        let real = with_tempo(rb(2, "a", "a"), 0.018, 1);
        for copies in [[&placeholder, &real], [&real, &placeholder]] {
            assert_eq!(choose_grid(&copies), GridChoice::Write(grid_at(18)));
        }
        // No TEMPO at all is no information either.
        let none = rb(3, "a", "a");
        assert_eq!(choose_grid(&[&none, &real]), GridChoice::Write(grid_at(18)));
        assert_eq!(choose_grid(&[&none, &placeholder]), GridChoice::Keep);
    }

    #[test]
    fn copies_with_the_same_real_grid_agree() {
        let (a, b) = (
            with_tempo(rb(1, "a", "a"), 0.018, 1),
            with_tempo(rb(2, "a", "a"), 0.018, 1),
        );
        assert_eq!(choose_grid(&[&a, &b]), GridChoice::Write(grid_at(18)));
    }

    #[test]
    fn different_real_grids_are_a_conflict_with_one_row_listing_the_candidates() {
        let a = with_tempo(rb(1, "a", "a"), 0.018, 1);
        let placeholder = with_tempo(rb(2, "a", "a"), 0.0, 1);
        let b = with_tempo(rb(3, "a", "a"), 0.250, 1);
        let results = [
            (&a, definitive("sha256:one")),
            (&placeholder, definitive("sha256:one")),
            (&b, definitive("sha256:one")),
        ];
        let GridChoice::Conflict(copies) = choose_grid(&[&a, &placeholder, &b]) else {
            panic!("conflict expected");
        };
        let ids: Vec<u32> = copies.iter().map(|t| t.track_id).collect();
        assert_eq!(ids, [1, 3], "the placeholder is not a candidate");

        let ps = problems(&results, bare);
        let statuses: Vec<_> = ps
            .iter()
            .map(|p| (p.track().track_id, p.status()))
            .collect();
        assert_eq!(
            statuses,
            [
                (1, "grid-conflict"),
                (1, "shared"),
                (2, "placeholder-grid"),
                (2, "shared"),
                (3, "shared")
            ]
        );
        let reason = ps[0].reason();
        assert!(
            reason.contains("TrackID 1 /music/A B/track.flac (Inizio 0.018 Battito 1 Bpm 128.00)")
        );
        assert!(
            reason.contains("TrackID 3 /music/A B/track.flac (Inizio 0.250 Battito 1 Bpm 128.00)")
        );
        assert_eq!(ps[0].candidates()[0].hash, hash("sha256:one"));
        assert_eq!(summarize(&results).grid_conflicts, 1);
    }

    #[test]
    fn a_multi_anchor_set_on_one_grid_imports_as_the_first_anchors_grid() {
        let t = with_two_anchors(rb(1, "a", "a"), 0.0);
        let results = [(&t, definitive("sha256:one"))];
        assert!(problems(&results, bare).is_empty());
        assert_eq!(choose_grid(&[&t]), GridChoice::Write(grid_at(0)));
    }

    #[test]
    fn a_variable_tempo_writes_no_grid_and_has_a_row() {
        let t = with_two_anchors(rb(5, "a", "a"), 0.1);
        let results = [(&t, definitive("sha256:one"))];
        let GridChoice::Variable(copies) = choose_grid(&[&t]) else {
            panic!("variable expected");
        };
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].first_anchor_grid, Some(grid_at(0)));

        let ps = problems(&results, bare);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].status(), "variable-tempo");
        let reason = ps[0].reason();
        assert!(reason.contains("2 TEMPO anchors"), "{reason}");
        assert!(
            reason.contains("anchor 2 is off the first anchor's beat grid"),
            "{reason}"
        );
        assert!(reason.contains("worst offset 100.0 ms"), "{reason}");
        let summary = summarize(&results);
        assert_eq!(summary.variable_tempos, 1);
        assert_eq!(summary.total(), 1, "an annotation, not a class");
    }

    #[test]
    fn a_variable_copy_blocks_the_grid_of_the_other_copies() {
        let variable = with_two_anchors(rb(1, "a", "a"), 0.1);
        let real = with_tempo(rb(2, "a", "a"), 0.018, 1);
        let choice = choose_grid(&[&real, &variable]);
        assert!(
            matches!(choice, GridChoice::Variable(ref v) if v.len() == 1),
            "{choice:?}"
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
        let mut tracks: Vec<RekordboxTrack> = (1..=8).map(|i| rb(i, "a", "t")).collect();
        // Placeholder grids are an annotation on top of the classes below. On a
        // matched, a weak and a shared track they count; on an ambiguous one (never
        // imported) they do not. The other shared track has a real grid: no conflict.
        for i in [0, 1, 2, 4] {
            tracks[i] = with_tempo(tracks[i].clone(), 0.0, 1);
        }
        tracks[3] = with_tempo(tracks[3].clone(), 0.018, 1);
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
                shared_groups: 1,
                placeholder_grids: 3,
                grid_conflicts: 0,
                variable_tempos: 0
            }
        );
        assert_eq!(
            summary.total(),
            tracks.len(),
            "annotations are not classes: the partition still adds up"
        );
        let placeholder_rows = problems(&results, bare)
            .iter()
            .filter(|p| p.status() == "placeholder-grid")
            .count();
        assert_eq!(placeholder_rows, summary.placeholder_grids);
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
