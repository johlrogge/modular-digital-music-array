//! Duplicate scan and merge (ADR-013).
//!
//! A duplicate group is a connected component of live tracks that share an
//! AudioHash v1 or a PcmHash v1. `merge_group` merges its losers into the winner
//! through `merge_into`, the generalised ADR-004 replace that `track replace` uses
//! too. Facts are made durable first and files are deleted afterwards, so a crash
//! leaves either a still-detectable group or a merge orphan that the cleanup
//! removes.

use super::*;
use crate::ipc::{
    ConfirmationReason, DedupApplyReport, DedupDisposition, DedupGroup, DedupMember,
    DedupScanReport,
};
use chrono::{DateTime, Utc};
use music_facts::{FactOrigin, FactSource};
use stainless_facts::Operation;
use std::collections::BTreeSet;
use std::mem::{discriminant, Discriminant};
use std::path::Path;

/// `FactSource::tool` of the facts a dedup merge writes itself (`Replaces(loser)`).
const DEDUP_TOOL: &str = "mdma-dedup";

pub(super) fn dedup_fact_source() -> FactSource {
    FactSource::new(DEDUP_TOOL, env!("CARGO_PKG_VERSION"), FactOrigin::Unknown)
}

/// The `FactSource` of facts a user action writes (ADR-004 replace, soft delete).
pub(super) fn user_fact_source() -> FactSource {
    FactSource::new("mdma", env!("CARGO_PKG_VERSION"), FactOrigin::User)
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
/// order. A retraction cancels the first assertion with the same value and source,
/// exactly like the in-memory projection does.
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
                if let Some(i) = live
                    .iter()
                    .position(|l| &l.value == f.value() && &l.source == f.source())
                {
                    live.remove(i);
                }
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
/// bookmark is the same bookmark when it has the same scope.
fn same_member(a: &MusicValue, b: &MusicValue) -> bool {
    match (a, b) {
        (MusicValue::Bookmarked { scope: x, .. }, MusicValue::Bookmarked { scope: y, .. }) => {
            x == y
        }
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

/// Only plain components below `dir` may be deleted: a fact must never steer a
/// deletion outside the blob and cover-art stores.
fn is_store_path(rel: &Path, dir: &str) -> bool {
    rel.starts_with(dir)
        && rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

impl LibraryService {
    /// Every duplicate group in the index, automatic groups first, then by winner.
    fn scan_duplicate_groups(&self) -> Vec<DedupGroup> {
        let tracks: Vec<ScanTrack> = self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .map(|t| ScanTrack {
                hash: t.content_hash.clone(),
                title: t.title.clone(),
                album: t.album.clone(),
                added_at: t.added_at,
                item_id: t.item_id.clone(),
                item_ids: t.item_ids().map(str::to_owned).collect(),
                deleted: t.is_hidden(),
                identities: t
                    .provenance
                    .iter()
                    .filter_map(|(v, _)| match v {
                        MusicValue::AudioHash(h) => Some(format!("audio:{}", h.as_str())),
                        MusicValue::PcmHash(h) => Some(format!("pcm:{}", h.as_str())),
                        _ => None,
                    })
                    .collect(),
            })
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
            report.blobs_removed += merged.blobs_removed;
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
    /// service delete the blob of an unrelated track.
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

    /// Merge every loser into the winner, one after the other, and remove the
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
                    // The facts are retracted (and projected): now the files may go.
                    self.remove_retired_files(&outcome.retired, &mut report);
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
    // File removal
    // =====================================================================

    fn remove_store_file(&self, rel: &Path, report: &mut DedupApplyReport) -> bool {
        match std::fs::remove_file(self.music_dir.join(rel)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                report
                    .errors
                    .push(format!("failed to remove {}: {e}", rel.display()));
                false
            }
        }
    }

    /// Remove cover art that no live track references any more.
    fn remove_cover_art_if_unused(&self, rel: &Path, report: &mut DedupApplyReport) {
        if !is_store_path(rel, "cover-art") {
            return;
        }
        let in_use = self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.cover_art_path.as_deref() == Some(rel));
        if !in_use {
            self.remove_store_file(rel, report);
        }
    }

    /// Delete the files of a track whose facts are already retracted.
    pub(super) fn remove_retired_files(
        &self,
        retired: &RetiredFiles,
        report: &mut DedupApplyReport,
    ) {
        if let Some(blob) = retired
            .blob
            .as_deref()
            .filter(|b| is_store_path(b, "blobs"))
        {
            if self.remove_store_file(blob, report) {
                report.blobs_removed += 1;
            }
        }
        if let Some(cover) = &retired.cover_art {
            self.remove_cover_art_if_unused(cover, report);
        }
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

    /// Delete the blob and cover art of every merge orphan. Idempotent, and it only
    /// ever touches hashes named by a live `Replaces` from `mdma-dedup`: other
    /// `NoLiveFacts` orphans (ADR-004 replace leftovers) are not its business.
    pub(super) fn cleanup_merge_orphans(&self, report: &mut DedupApplyReport) {
        for key in self.merge_orphan_candidates() {
            // Liveness is re-checked under the retire gate right before the files
            // go, so a track that came back since the candidate list was made keeps
            // its blob.
            let gate = self.retire_gate.lock().unwrap();
            self.remove_orphan_files_if_still_gone(&gate, &key, report);
        }
    }

    /// Remove the files of `key` unless it came back to life. The gate is the proof
    /// that nothing can revive it between the check and the removal.
    fn remove_orphan_files_if_still_gone(
        &self,
        _gate: &super::RetireGuard<'_>,
        key: &str,
        report: &mut DedupApplyReport,
    ) {
        let is_live = self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .any(|t| hash_key(t.content_hash.as_str()) == key);
        if is_live {
            return;
        }
        for blob in self.store_files_named("blobs", key.get(..2), key) {
            if self.remove_store_file(&blob, report) {
                report.blobs_removed += 1;
            }
        }
        for art in self.store_files_named("cover-art", None, key) {
            self.remove_cover_art_if_unused(&art, report);
        }
    }
}
