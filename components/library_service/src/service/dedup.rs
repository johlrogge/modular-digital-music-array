//! Duplicate scan and merge (ADR-013).
//!
//! A duplicate group is a connected component of live tracks that share an
//! AudioHash v1 or a PcmHash v1. `merge_group` merges its losers into the winner
//! through `merge_into`, the generalised ADR-004 replace that `track replace` uses
//! too. Facts are made durable first and the losers' files are moved to
//! `<music_dir>/archive/` afterwards (never deleted; `archive_empty` is the only
//! deletion), so a crash leaves either a still-detectable group or a merge orphan
//! that the cleanup archives.

use super::*;
use crate::ipc::{
    ArchiveEmptyReport, ArchiveEntry, ArchiveListing, ConfirmationReason, DedupApplyReport,
    DedupDisposition, DedupGroup, DedupMember, DedupScanReport,
};
use chrono::{DateTime, Utc};
use music_facts::{FactOrigin, FactSource};
use stainless_facts::Operation;
use std::collections::BTreeSet;
use std::mem::{discriminant, Discriminant};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// `FactSource::tool` of the facts a dedup merge writes itself (`Replaces(loser)`).
const DEDUP_TOOL: &str = "mdma-dedup";

pub(super) fn dedup_fact_source() -> FactSource {
    FactSource::new(DEDUP_TOOL, env!("CARGO_PKG_VERSION"), FactOrigin::Unknown)
}

// =========================================================================
// Fact rules (pure)
// =========================================================================

/// One live assertion of an entity, as read back from ACID.
pub(super) struct LiveFact {
    pub value: MusicValue,
    pub source: FactSource,
    /// The ACID timestamp of the assertion.
    pub timestamp: DateTime<Utc>,
}

/// The assertions of an entity that no later retraction has cancelled, in stream
/// order. A retraction cancels every earlier assertion with an equal value and the
/// same source (`FactSource::same_source`), through the same `remove_cancelled` the
/// in-memory projection uses. That holds for provenance-tracked values only: in memory
/// the retracts of TrackStarted, TrackStopped, FilePath and AddedAt are ignored,
/// whereas this replay cancels them too.
pub(super) fn live_facts(lines: &[String]) -> Vec<LiveFact> {
    let mut live: Vec<LiveFact> = Vec::new();
    for line in lines {
        let Some(f) = parse_fact_line(line) else {
            continue;
        };
        match f.operation() {
            Operation::Assert => live.push(LiveFact {
                value: f.value().clone(),
                source: f.source().clone(),
                timestamp: *f.timestamp(),
            }),
            Operation::Retract => {
                remove_cancelled(&mut live, f.value(), f.source(), |l| (&l.value, &l.source));
            }
        }
    }
    live
}

/// How a fact is treated when a loser is merged into a winner (ADR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarryRule {
    /// Never copied: file-derived, play history, lifecycle, or written by the merge
    /// itself (`Replaces`).
    Skip,
    /// The latest value across the group wins.
    Scalar,
    /// The union of the members' values.
    Union,
    /// The latest value across the group wins, as the projection does for every track.
    AddedAt,
}

/// Exhaustive on purpose: a new `MusicValue` variant must be given a rule here.
fn carry_rule(v: &MusicValue) -> CarryRule {
    use MusicValue as V;
    match v {
        // File-derived: always the winner's own, they describe its file.
        V::FilePath(_)
        | V::AudioHash(_)
        | V::PcmHash(_)
        | V::Format(_)
        | V::DurationSeconds(_)
        | V::Bitrate(_)
        | V::FileSizeBytes(_)
        | V::HasAlbumArt(_)
        | V::EncoderSoftware(_)
        | V::EncodedBy(_)
        | V::CoverArtPath(_)
        | V::BitDepth(_)
        | V::Channels(_)
        | V::SampleRate(_) => CarryRule::Skip,
        // Copying would restamp the events; the projection follows `Replaces` instead.
        V::TrackStarted(_) | V::TrackStopped(_) => CarryRule::Skip,
        // `Replaces` is written by the merge; a soft-delete belongs to the copy it hid.
        V::Replaces(_) | V::Deleted { .. } => CarryRule::Skip,
        V::AddedAt(_) => CarryRule::AddedAt,
        // Every purchase the group stands for stays known to the source: a source's
        // `has_facts("ItemId")` must not report a retired copy's purchase as new.
        V::StyleDescriptor(_) | V::MemoryCue { .. } | V::Bookmarked { .. } | V::ItemId(_) => {
            CarryRule::Union
        }
        V::Title(_)
        | V::Artist(_)
        | V::Album(_)
        | V::AlbumArtist(_)
        | V::TrackNumber(_)
        | V::DiscNumber(_)
        | V::Year(_)
        | V::Bpm(_)
        | V::Key(_)
        | V::MainGenre(_)
        | V::FullGenre(_)
        | V::Role(_)
        | V::Energy(_)
        | V::BeatGrid { .. }
        | V::Isrc(_)
        | V::Label(_)
        | V::RecordingYear(_)
        | V::RecordingDate(_)
        | V::BeatportTrackUrl(_)
        | V::BeatportLabelUrl(_)
        | V::BandcampUrl(_)
        | V::Source(_)
        | V::Comment(_)
        | V::BeatportTrackId(_) => CarryRule::Scalar,
    }
}

/// Members of a multi-valued attribute are the same when they are equal; a
/// bookmark is the same bookmark when it carries the same mark (legacy scopes
/// compare by their raw text).
pub(super) fn same_member(a: &MusicValue, b: &MusicValue) -> bool {
    match (a.stored_mark(), b.stored_mark()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn latest_added<'a>(
    facts: impl Iterator<Item = &'a LiveFact>,
) -> Option<(DateTime<Utc>, &'a FactSource)> {
    facts
        .filter_map(|f| match &f.value {
            MusicValue::AddedAt(d) => Some((*d, &f.source)),
            _ => None,
        })
        .max_by_key(|(d, _)| *d)
}

/// The facts to assert on the winner so that it shows what one entity would show if
/// every member's facts had been asserted on it (ADR-013), each with its original
/// `FactSource`.
///
/// For a scalar attribute the value with the latest ACID timestamp across the whole
/// group wins. Nothing is written when the winner's own newest assertion is that
/// one, or when the winner already shows that value, so a rerun adds nothing.
/// Multi-valued attributes are united. Planning the whole group at once is what
/// makes the latest value win across more than one loser: a carried copy is stamped
/// "now" and would outrank every later-planned loser.
fn plan_carry(winner: &[LiveFact], losers: &[Vec<LiveFact>]) -> Vec<(MusicValue, FactSource)> {
    let mut out: Vec<(MusicValue, FactSource)> = Vec::new();
    let loser_facts = || losers.iter().flatten();

    // Scalars, in order of first appearance.
    let mut attributes: Vec<Discriminant<MusicValue>> = Vec::new();
    for f in loser_facts().filter(|f| carry_rule(&f.value) == CarryRule::Scalar) {
        let d = discriminant(&f.value);
        if !attributes.contains(&d) {
            attributes.push(d);
        }
    }
    for d in attributes {
        let own: Vec<&LiveFact> = winner
            .iter()
            .filter(|f| discriminant(&f.value) == d)
            .collect();
        let newest = loser_facts().filter(|f| discriminant(&f.value) == d).fold(
            None::<&LiveFact>,
            |best, f| match best {
                Some(b) if b.timestamp >= f.timestamp => Some(b),
                _ => Some(f),
            },
        );
        let Some(newest) = newest else { continue };
        let winner_is_newest = own.iter().any(|w| w.timestamp >= newest.timestamp);
        let already_shown = own.last().is_some_and(|w| w.value == newest.value);
        if !winner_is_newest && !already_shown {
            out.push((newest.value.clone(), newest.source.clone()));
        }
    }

    // Multi-valued: what the winner lacks, newest first.
    let mut candidates: Vec<&LiveFact> = loser_facts()
        .filter(|f| carry_rule(&f.value) == CarryRule::Union)
        .collect();
    candidates.sort_by_key(|a| std::cmp::Reverse(a.timestamp));
    for f in candidates {
        let held = winner
            .iter()
            .map(|w| &w.value)
            .chain(out.iter().map(|(v, _)| v))
            .any(|v| same_member(v, &f.value));
        if !held {
            out.push((f.value.clone(), f.source.clone()));
        }
    }

    // AddedAt: the latest across the group.
    if let Some((added, source)) = latest_added(loser_facts()) {
        if latest_added(winner.iter()).is_none_or(|(own, _)| added > own) {
            out.push((MusicValue::AddedAt(added), source.clone()));
        }
    }

    out
}

// =========================================================================
// Scan
// =========================================================================

/// What the scan needs to know about one indexed track.
struct ScanTrack {
    hash: ContentHash,
    title: Option<String>,
    album: Option<String>,
    added_at: Option<DateTime<Utc>>,
    /// Display only: the last-wins scalar. Matching uses `item_ids`.
    item_id: Option<String>,
    /// Every ItemId in the provenance (a merge unions them).
    item_ids: BTreeSet<String>,
    deleted: bool,
    /// Identity keys. The hash strings carry their version prefix, so equal keys are
    /// equal hashes of the same version; the kind keeps AudioHash and PcmHash apart.
    identities: Vec<String>,
}

impl From<&IndexedTrackInfo> for ScanTrack {
    fn from(t: &IndexedTrackInfo) -> Self {
        ScanTrack {
            hash: t.content_hash.clone(),
            title: t.title.clone(),
            album: t.album.clone(),
            added_at: t.added_at,
            item_id: t.item_id.clone(),
            item_ids: t.item_ids().map(str::to_owned).collect(),
            deleted: t.is_hidden(),
            identities: t
                .audio_hashes()
                .map(|h| format!("audio:{h}"))
                .chain(t.provenance.iter().filter_map(|(v, _)| match v {
                    MusicValue::PcmHash(h) => Some(format!("pcm:{}", h.as_str())),
                    _ => None,
                }))
                .collect(),
        }
    }
}

/// Connected components (of two or more tracks) of the "shares an identity key" relation.
fn duplicate_components(tracks: &[ScanTrack]) -> Vec<Vec<usize>> {
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    let mut parent: Vec<usize> = (0..tracks.len()).collect();
    let mut owner: HashMap<&str, usize> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        for key in &t.identities {
            match owner.get(key.as_str()) {
                Some(&j) => {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    owner.insert(key, i);
                }
            }
        }
    }

    let mut components: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..tracks.len() {
        let root = find(&mut parent, i);
        components.entry(root).or_default().push(i);
    }
    components.into_values().filter(|c| c.len() > 1).collect()
}

fn build_group(tracks: &[ScanTrack], component: &[usize]) -> DedupGroup {
    let mut members: Vec<&ScanTrack> = component.iter().map(|&i| &tracks[i]).collect();
    // Newest first; ties go to the lowest hash. The first member is the winner.
    members.sort_by(|a, b| {
        b.added_at
            .cmp(&a.added_at)
            .then_with(|| a.hash.as_str().cmp(b.hash.as_str()))
    });

    // A track's ItemIds are a set (a confirmed merge unions them), so the purchases
    // agree when one id is common to every member that has any.
    let mut with_ids = members.iter().filter(|m| !m.item_ids.is_empty());
    let shared: BTreeSet<&str> = match with_ids.next() {
        Some(first) => with_ids.fold(
            first.item_ids.iter().map(String::as_str).collect(),
            |common, m| {
                common
                    .into_iter()
                    .filter(|id| m.item_ids.contains(*id))
                    .collect()
            },
        ),
        None => BTreeSet::new(),
    };
    let mut reasons = Vec::new();
    if members.iter().any(|m| !m.item_ids.is_empty()) && shared.is_empty() {
        reasons.push(ConfirmationReason::DifferentItemIds);
    }
    if members.iter().any(|m| m.item_ids.is_empty()) {
        reasons.push(ConfirmationReason::NoItemId);
    }
    if members.iter().any(|m| m.deleted) {
        reasons.push(ConfirmationReason::SoftDeleted);
    }

    DedupGroup {
        winner: members[0].hash.clone(),
        disposition: if reasons.is_empty() {
            DedupDisposition::Automatic
        } else {
            DedupDisposition::NeedsConfirmation { reasons }
        },
        members: members
            .iter()
            .map(|m| DedupMember {
                hash: m.hash.clone(),
                title: m.title.clone(),
                album: m.album.clone(),
                added: m.added_at.map(|d| d.to_rfc3339()),
                item_id: m.item_id.clone(),
                deleted: m.deleted,
            })
            .collect(),
    }
}

/// Where merged duplicates' files go, below `music_dir`. Only `archive_empty`
/// deletes from it.
const ARCHIVE_DIR: &str = "archive";

/// Only plain components below `dir` may be archived: a fact must never steer a
/// move outside the blob and cover-art stores.
pub(super) fn is_store_path(rel: &Path, dir: &str) -> bool {
    rel.starts_with(dir) && is_archive_path(rel)
}

/// A non-empty path of plain components: no `..`, no root, no `.` prefix. Anything
/// the archive walk yields satisfies it; the check keeps deletion confined if it
/// ever did not.
pub(super) fn is_archive_path(rel: &Path) -> bool {
    rel.components().next().is_some()
        && rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Why nothing is walked or deleted when `archive/` is not a real directory.
const NOT_A_PLAIN_DIR: &str = "archive/ is not a plain directory; refusing";

/// What a walk of `archive/` found.
#[derive(Default)]
struct ArchiveWalk {
    files: Vec<(PathBuf, std::fs::Metadata)>,
    dirs: Vec<PathBuf>,
    /// Refusals and entries the walk could not cover.
    problems: Vec<String>,
}

/// `dst`, or the first of `stem.1.ext`, `stem.2.ext`, ... that does not exist yet.
fn free_archive_path(dst: &Path) -> PathBuf {
    if dst.symlink_metadata().is_err() {
        return dst.to_path_buf();
    }
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_owned();
    let (stem, ext) = match name.split_once('.') {
        Some((stem, ext)) => (stem.to_owned(), format!(".{ext}")),
        None => (name, String::new()),
    };
    (1u32..)
        .map(|n| dst.with_file_name(format!("{stem}.{n}{ext}")))
        .find(|p| p.symlink_metadata().is_err())
        .expect("an unbounded counter finds a free name")
}

impl LibraryService {
    /// Every duplicate group in the index, automatic groups first, then by winner.
    fn scan_duplicate_groups(&self) -> Vec<DedupGroup> {
        let tracks: Vec<ScanTrack> = self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .map(ScanTrack::from)
            .collect();

        let mut groups: Vec<DedupGroup> = duplicate_components(&tracks)
            .iter()
            .map(|c| build_group(&tracks, c))
            .collect();
        groups.sort_by(|a, b| {
            b.is_automatic()
                .cmp(&a.is_automatic())
                .then_with(|| a.winner.as_str().cmp(b.winner.as_str()))
        });
        groups
    }

    /// Self-healing merge at ingest (ADR-013): once `new` holds an AudioHash v1, group
    /// it with every other live (not soft-deleted) track that shares an AudioHash,
    /// using the scan's own grouping rule (`build_group`), and merge the group only if
    /// the scan would merge it automatically. The winner is the group's newest copy,
    /// per the scan rule: on a fresh ingest that is the new copy, but an old copy that
    /// has just gained an ItemId can lose to a newer one. PcmHash-only matches and
    /// soft-deleted copies are left for the scan.
    ///
    /// Call it after the facts of `new` are durable and projected, and again whenever
    /// an ItemId lands on an existing track, so the order of the two does not matter.
    /// Ingests without an ItemId (`track replace`, uploads, Beatport) reach it too but
    /// never merge: such a group always needs confirmation (`NoItemId`). It holds no
    /// lock when it calls `merge_group` (which takes the retire gate itself): the
    /// group is decided under `tracks` alone. A failure is logged and never reaches
    /// the caller: the scan picks the group up later.
    ///
    /// Returns the hash that is live afterwards: the winner if a merge happened,
    /// otherwise `new`.
    pub(super) fn self_heal_after_ingest(&self, new: &ContentHash) -> ContentHash {
        let Some((winner, losers)) = self.self_heal_group(new) else {
            return new.clone();
        };

        let mut report = self.merge_group(&winner, &losers);
        if report.merged > 0 {
            self.cleanup_merge_orphans(&mut report);
        }
        if report.errors.is_empty() {
            tracing::info!(
                winner = %winner.as_str(),
                merged = report.merged,
                blobs_archived = report.blobs_archived,
                "Ingest merged a re-downloaded copy into its predecessor"
            );
        } else if report.merged > 0 {
            tracing::warn!(
                winner = %winner.as_str(),
                merged = report.merged,
                errors = ?report.errors,
                "Ingest merged a re-downloaded copy, but archiving its files (or merging \
                 another copy) had errors"
            );
        } else {
            tracing::warn!(
                winner = %winner.as_str(),
                errors = ?report.errors,
                "Ingest self-heal merge failed; the dedup scan will pick the group up"
            );
        }
        if report.merged > 0 {
            winner
        } else {
            new.clone()
        }
    }

    /// The winner and losers of the automatic duplicate group `new` belongs to, if any.
    fn self_heal_group(&self, new: &ContentHash) -> Option<(ContentHash, Vec<ContentHash>)> {
        let key = hash_key(new.as_str());
        let tracks = self.tracks.lock().unwrap();
        let entry = tracks
            .iter()
            .find(|t| hash_key(t.content_hash.as_str()) == key)
            .filter(|t| !t.is_hidden())?;

        let new_audio: BTreeSet<&str> = entry.audio_hashes().collect();
        if new_audio.is_empty() {
            return None;
        }

        let scan: Vec<ScanTrack> = std::iter::once(entry)
            .chain(
                tracks
                    .iter()
                    .filter(|t| hash_key(t.content_hash.as_str()) != key && !t.is_hidden())
                    .filter(|t| t.audio_hashes().any(|h| new_audio.contains(h))),
            )
            .map(ScanTrack::from)
            .collect();
        drop(tracks);
        if scan.len() < 2 {
            return None;
        }

        let component: Vec<usize> = (0..scan.len()).collect();
        let group = build_group(&scan, &component);
        if !group.is_automatic() {
            return None;
        }
        let losers: Vec<ContentHash> = group.losers().map(|m| m.hash.clone()).collect();
        Some((group.winner, losers))
    }

    /// Handle `DedupScan`. With `apply`, merge the automatic groups and run the
    /// merge-orphan cleanup, then report the groups that are left.
    pub(super) fn dedup_scan(&self, apply: bool) -> DedupScanReport {
        if !apply {
            return DedupScanReport {
                groups: self.scan_duplicate_groups(),
                applied: None,
            };
        }

        let mut report = DedupApplyReport::default();
        for group in self
            .scan_duplicate_groups()
            .into_iter()
            .filter(|g| g.is_automatic())
        {
            let losers: Vec<ContentHash> = group.losers().map(|m| m.hash.clone()).collect();
            let merged = self.merge_group(&group.winner, &losers);
            report.merged += merged.merged;
            report.blobs_archived += merged.blobs_archived;
            report.errors.extend(merged.errors);
        }
        self.cleanup_merge_orphans(&mut report);

        DedupScanReport {
            groups: self.scan_duplicate_groups(),
            applied: Some(report),
        }
    }

    /// Handle `DedupMerge`: a user-confirmed merge. The tracks must form (part of)
    /// one duplicate group as the scan finds it, so a mistyped hash cannot make the
    /// service archive the blob of an unrelated track.
    pub(super) fn dedup_merge_request(
        &self,
        winner: &ContentHash,
        losers: &[ContentHash],
    ) -> LibraryResponse {
        let refuse = |message: String| LibraryResponse::Error(ProtocolError::Internal { message });

        let winner = match self.resolve_hash(winner) {
            Ok(h) => h,
            Err(e) => return LibraryResponse::Error(e),
        };
        let mut resolved: Vec<ContentHash> = Vec::new();
        for l in losers {
            match self.resolve_hash(l) {
                Ok(h) if !resolved.contains(&h) => resolved.push(h),
                Ok(_) => {}
                Err(e) => return LibraryResponse::Error(e),
            }
        }
        if resolved.is_empty() {
            return refuse("nothing to merge: no losers given".into());
        }
        if resolved.contains(&winner) {
            return refuse(format!("{winner} cannot be merged into itself"));
        }

        let groups = self.scan_duplicate_groups();
        let Some(group) = groups
            .iter()
            .find(|g| g.members.iter().any(|m| m.hash == winner))
        else {
            return refuse(format!("{winner} has no duplicates"));
        };
        if let Some(stray) = resolved
            .iter()
            .find(|l| !group.members.iter().any(|m| m.hash == **l))
        {
            return refuse(format!("{stray} is not a duplicate of {winner}"));
        }

        LibraryResponse::DedupMerged(self.merge_group(&winner, &resolved))
    }

    /// Merge every loser into the winner, one after the other, and archive the
    /// losers' files once their facts are retracted. Stops at the first failure:
    /// the rest of the group stays detectable and a rerun completes it.
    pub(super) fn merge_group(
        &self,
        winner: &ContentHash,
        losers: &[ContentHash],
    ) -> DedupApplyReport {
        let mut report = DedupApplyReport::default();

        // Planned for the whole group up front and written with the first merge (see
        // `plan_carry`); later merges only add their `Replaces` links.
        let mut carry = match self.plan_group_carry(winner, losers) {
            Ok(c) => Some(c),
            Err(e) => {
                report.errors.push(e);
                return report;
            }
        };
        let source = dedup_fact_source();

        for loser in losers {
            match self.merge_into(winner, loser, &source, carry.take().unwrap_or_default()) {
                Ok(outcome) => {
                    report.merged += 1;
                    // The facts are retracted (and projected): now the files may be
                    // moved to the archive, unless the loser came back meanwhile.
                    self.archive_retired_files_if_still_gone(loser, &outcome.retired, &mut report);
                }
                Err(e) => {
                    report.errors.push(format!("{loser} into {winner}: {e}"));
                    break;
                }
            }
        }
        report
    }

    /// Read the winner and the losers from ACID and plan what the winner must carry.
    pub(super) fn plan_group_carry(
        &self,
        winner: &ContentHash,
        losers: &[ContentHash],
    ) -> Result<Vec<(MusicValue, FactSource)>, String> {
        let read = |h: &ContentHash| {
            self.acid_client
                .read_entity(h.as_str())
                .map(|lines| live_facts(&lines))
                .map_err(|e| format!("Failed to read {h} from ACID: {e}"))
        };
        let winner_facts = read(winner)?;
        let loser_facts = losers.iter().map(read).collect::<Result<Vec<_>, _>>()?;
        Ok(plan_carry(&winner_facts, &loser_facts))
    }

    // =====================================================================
    // File archiving (a merge never deletes; only `archive_empty` does)
    // =====================================================================

    /// Move `rel` (relative to `music_dir`, in `blobs` or `cover-art`) to the same
    /// place under `archive/`. `true` when a file was moved; a file that is already
    /// gone counts as nothing to do. A name already taken in the archive is never
    /// overwritten: the file gets a numbered name instead. The gate proves that the
    /// caller serialises with merges and the backfill; it has re-checked that the
    /// file's track is still gone.
    fn archive_store_file(
        &self,
        _gate: &super::RetireGuard<'_>,
        rel: &Path,
        report: &mut DedupApplyReport,
    ) -> bool {
        if !is_store_path(rel, "blobs") && !is_store_path(rel, "cover-art") {
            return false;
        }
        let src = self.music_dir.join(rel);
        let src_exists = || src.symlink_metadata().is_ok();
        if !src_exists() {
            return false;
        }
        // `rename` keeps the mtime and the archive lists when a file was archived, so
        // stamp it BEFORE the move: there is then no moment at which an archived file
        // carries its old ingest-time mtime (`archive empty` could delete it unseen).
        // Opened read-only: the owner may set times without write permission.
        let stamped = match src.symlink_metadata() {
            Ok(meta) if meta.is_file() => {
                std::fs::File::open(&src).and_then(|f| f.set_modified(std::time::SystemTime::now()))
            }
            Ok(_) => Err(std::io::Error::other("not a regular file")),
            Err(e) => Err(e),
        };
        if let Err(e) = stamped {
            tracing::warn!(
                path = %src.display(),
                error = %e,
                "Could not set the archived time; archiving anyway"
            );
        }
        let dst = free_archive_path(&self.music_dir.join(ARCHIVE_DIR).join(rel));
        let try_move = || {
            dst.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::rename(&src, &dst))
        };
        let mut moved = try_move();
        // `NotFound` can be the source or the destination directory (e.g. a
        // concurrent `archive empty` pruned it): retry once while the source is there.
        if matches!(&moved, Err(e) if e.kind() == std::io::ErrorKind::NotFound) && src_exists() {
            moved = try_move();
        }
        match moved {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !src_exists() => false,
            Err(e) => {
                report
                    .errors
                    .push(format!("failed to archive {}: {e}", rel.display()));
                false
            }
        }
    }

    /// Whether a live track shows the cover art `rel`: as its own, or through
    /// `album_cover_cache`, which hands it to the album's tracks that have none. A
    /// cache entry that points at `rel` but is not needed is repointed at another
    /// cover of the album, or evicted, so it never names an archived file.
    fn cover_art_in_use(&self, rel: &Path) -> bool {
        let tracks = self.tracks.lock().unwrap();
        if tracks
            .iter()
            .any(|t| t.cover_art_path.as_deref() == Some(rel))
        {
            return true;
        }
        let rel_str = rel.to_string_lossy();
        let mut cache = self.album_cover_cache.lock().unwrap();
        let albums: Vec<String> = cache
            .iter()
            .filter(|(_, path)| path.as_str() == rel_str)
            .map(|(album, _)| album.clone())
            .collect();
        let mut serving = false;
        for album in albums {
            let of_album = || {
                tracks
                    .iter()
                    .filter(|t| t.album.as_deref() == Some(album.as_str()))
            };
            if let Some(other) = of_album().find_map(|t| t.cover_art_path.as_ref()) {
                cache.insert(album, other.to_string_lossy().into_owned());
            } else if of_album().next().is_some() {
                // Tracks of the album without a cover of their own show this one.
                serving = true;
            } else {
                cache.remove(&album);
            }
        }
        serving
    }

    /// Archive cover art that no live track references any more.
    fn archive_cover_art_if_unused(
        &self,
        gate: &super::RetireGuard<'_>,
        rel: &Path,
        report: &mut DedupApplyReport,
    ) {
        if !is_store_path(rel, "cover-art") {
            return;
        }
        if !self.cover_art_in_use(rel) {
            self.archive_store_file(gate, rel, report);
        }
    }

    /// Archive the files of a track whose facts are already retracted.
    pub(super) fn archive_retired_files(
        &self,
        gate: &super::RetireGuard<'_>,
        retired: &RetiredFiles,
        report: &mut DedupApplyReport,
    ) {
        if let Some(blob) = retired
            .blob
            .as_deref()
            .filter(|b| is_store_path(b, "blobs"))
        {
            if self.archive_store_file(gate, blob, report) {
                report.blobs_archived += 1;
            }
        }
        if let Some(cover) = &retired.cover_art {
            self.archive_cover_art_if_unused(gate, cover, report);
        }
    }

    /// Archive the files of the merged-away `loser` unless it came back to life since
    /// the merge (a re-ingest of its bytes). Takes the retire gate for the check and
    /// the moves. Ingest does not take the gate, so the check closes all but a tiny
    /// window; a file archived in it is recoverable, because it is archived, not
    /// deleted.
    fn archive_retired_files_if_still_gone(
        &self,
        loser: &ContentHash,
        retired: &RetiredFiles,
        report: &mut DedupApplyReport,
    ) {
        #[cfg(test)]
        if let Some(hook) = self.merge_pre_archive_hook.lock().unwrap().as_ref() {
            hook(self, loser);
        }
        let gate = self.retire_gate.lock().unwrap();
        if self.is_live(loser) {
            return;
        }
        self.archive_retired_files(&gate, retired, report);
    }

    // =====================================================================
    // The archive: listing and explicit emptying
    // =====================================================================

    /// Walk `archive/`: every file (symlinks are listed, never followed) and every
    /// directory, both relative to the archive. Nothing is walked unless `archive/`
    /// is a real directory, so a symlink or file in its place can never steer a
    /// walk (and an emptying) somewhere else; an entry on another filesystem
    /// is not descended into. Whatever is skipped or unreadable is in
    /// `problems`, so a result is never silently partial.
    fn walk_archive(&self) -> ArchiveWalk {
        let root = self.music_dir.join(ARCHIVE_DIR);
        let mut walk = ArchiveWalk::default();
        let root_dev = match root.symlink_metadata() {
            Ok(meta) if meta.is_dir() => meta.dev(),
            Ok(_) => {
                walk.problems.push(NOT_A_PLAIN_DIR.to_owned());
                return walk;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return walk,
            Err(e) => {
                walk.problems.push(format!("{NOT_A_PLAIN_DIR}: {e}"));
                return walk;
            }
        };

        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            let entries = match std::fs::read_dir(root.join(&rel)) {
                Ok(entries) => entries,
                Err(e) => {
                    walk.problems
                        .push(format!("cannot read archive/{}: {e}", rel.display()));
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        walk.problems.push(format!(
                            "cannot read an entry of archive/{}: {e}",
                            rel.display()
                        ));
                        continue;
                    }
                };
                let rel = rel.join(entry.file_name());
                match entry.path().symlink_metadata() {
                    Ok(meta) if meta.is_dir() && meta.dev() != root_dev => {
                        let shown = rel.display();
                        walk.problems
                            .push(format!("skipping archive/{shown}: another filesystem"));
                    }
                    Ok(meta) if meta.is_dir() => {
                        walk.dirs.push(rel.clone());
                        pending.push(rel);
                    }
                    Ok(meta) => walk.files.push((rel, meta)),
                    Err(e) => walk
                        .problems
                        .push(format!("cannot stat archive/{}: {e}", rel.display())),
                }
            }
        }
        walk
    }

    /// Handle `ArchiveList`: what merges moved out of the library, oldest first.
    pub(super) fn archive_listing(&self) -> ArchiveListing {
        let walk = self.walk_archive();
        for problem in &walk.problems {
            tracing::warn!(problem = %problem, "The archive listing is incomplete");
        }
        let mut entries: Vec<(std::time::SystemTime, ArchiveEntry)> = walk
            .files
            .into_iter()
            .filter(|(rel, _)| is_archive_path(rel))
            .map(|(rel, meta)| {
                let modified = meta.modified().ok();
                let hash = rel
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.split('.').next())
                    .unwrap_or_default()
                    .to_owned();
                (
                    modified.unwrap_or(std::time::UNIX_EPOCH),
                    ArchiveEntry {
                        path: rel.to_string_lossy().into_owned(),
                        hash,
                        size_bytes: meta.len(),
                        archived_at: modified.map(|t| DateTime::<Utc>::from(t).to_rfc3339()),
                    },
                )
            })
            .collect();
        entries.sort_by(|a, b| (a.0, &a.1.path).cmp(&(b.0, &b.1.path)));
        let entries: Vec<ArchiveEntry> = entries.into_iter().map(|(_, e)| e).collect();
        ArchiveListing {
            total_bytes: entries.iter().map(|e| e.size_bytes).sum(),
            entries,
        }
    }

    /// Handle `ArchiveEmpty`: permanently delete the archive's files. Confined to
    /// `archive/`: only plain relative paths found by walking it are touched, and a
    /// symlink is removed itself, never followed. Emptied directories are pruned
    /// (the `archive/` directory itself stays). With `up_to` (RFC 3339), only files
    /// archived at or before it go: what a user confirmed from a listing. Anything
    /// the walk could not cover is in `errors`.
    pub(super) fn archive_empty(&self, up_to: Option<&str>) -> ArchiveEmptyReport {
        let root = self.music_dir.join(ARCHIVE_DIR);
        let mut report = ArchiveEmptyReport::default();
        let cutoff = match up_to.map(DateTime::parse_from_rfc3339).transpose() {
            Ok(cutoff) => cutoff.map(|c| c.with_timezone(&Utc)),
            Err(e) => {
                report
                    .errors
                    .push(format!("invalid cutoff {up_to:?}, nothing deleted: {e}"));
                return report;
            }
        };
        // Serialised against archive moves (which hold the gate); nothing else is held.
        let _gate = self.retire_gate.lock().unwrap();
        let ArchiveWalk {
            files,
            mut dirs,
            problems,
        } = self.walk_archive();
        report.errors.extend(problems);
        let archived_by_cutoff = |meta: &std::fs::Metadata| {
            cutoff.is_none_or(|cutoff| {
                let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                DateTime::<Utc>::from(at) <= cutoff
            })
        };
        for (rel, meta) in files
            .into_iter()
            .filter(|(rel, _)| is_archive_path(rel))
            .filter(|(_, meta)| archived_by_cutoff(meta))
        {
            match std::fs::remove_file(root.join(&rel)) {
                Ok(()) => {
                    report.files_deleted += 1;
                    report.bytes_freed += meta.len();
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => report
                    .errors
                    .push(format!("failed to delete {}: {e}", rel.display())),
            }
        }
        // Deepest first; `remove_dir` only ever removes an empty directory.
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for dir in dirs.into_iter().filter(|d| is_archive_path(d)) {
            let _ = std::fs::remove_dir(root.join(dir));
        }
        report
    }

    // =====================================================================
    // Merge-orphan cleanup
    // =====================================================================

    /// Hashes that a live `Replaces` fact from `mdma-dedup` names and that have no
    /// live facts: the blobs a merge may have left behind after a crash.
    fn merge_orphan_candidates(&self) -> Vec<String> {
        let tracks = self.tracks.lock().unwrap();
        let live: HashSet<String> = tracks
            .iter()
            .map(|t| hash_key(t.content_hash.as_str()))
            .collect();
        let mut orphans: Vec<String> = tracks
            .iter()
            .flat_map(|t| t.provenance.iter())
            .filter_map(|(v, s)| match v {
                MusicValue::Replaces(h) if s.tool == DEDUP_TOOL => Some(hash_key(h.as_str())),
                _ => None,
            })
            .filter(|key| !live.contains(key))
            // The key becomes a file name: plain names only.
            .filter(|key| {
                key.len() >= 2 && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .collect();
        orphans.sort();
        orphans.dedup();
        orphans
    }

    /// Files in `dir` whose stem is `stem`, with their path relative to `music_dir`.
    fn store_files_named(&self, dir: &str, sub: Option<&str>, stem: &str) -> Vec<PathBuf> {
        let rel_dir = match sub {
            Some(sub) => Path::new(dir).join(sub),
            None => PathBuf::from(dir),
        };
        std::fs::read_dir(self.music_dir.join(&rel_dir))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().file_stem().and_then(|s| s.to_str()) == Some(stem))
            .map(|e| rel_dir.join(e.file_name()))
            .collect()
    }

    /// Archive the blob and cover art of every merge orphan. Idempotent, and it only
    /// ever touches hashes named by a live `Replaces` from `mdma-dedup`: other
    /// `NoLiveFacts` orphans (ADR-004 replace leftovers) are not its business.
    pub(super) fn cleanup_merge_orphans(&self, report: &mut DedupApplyReport) {
        for key in self.merge_orphan_candidates() {
            // Liveness is re-checked under the retire gate right before the files
            // move, so a track that came back since the candidate list was made
            // keeps its blob.
            let gate = self.retire_gate.lock().unwrap();
            self.archive_orphan_files_if_still_gone(&gate, &key, report);
        }
    }

    /// Archive the files of `key` unless it came back to life. The gate serialises
    /// against merges and the backfill. Ingest does not take it, so the check closes
    /// all but a tiny window, which is recoverable: files are archived, not deleted.
    fn archive_orphan_files_if_still_gone(
        &self,
        gate: &super::RetireGuard<'_>,
        key: &str,
        report: &mut DedupApplyReport,
    ) {
        if self.is_live(&ContentHash::new(key)) {
            return;
        }
        for blob in self.store_files_named("blobs", key.get(..2), key) {
            if self.archive_store_file(gate, &blob, report) {
                report.blobs_archived += 1;
            }
        }
        for art in self.store_files_named("cover-art", None, key) {
            self.archive_cover_art_if_unused(gate, &art, report);
        }
    }
}
