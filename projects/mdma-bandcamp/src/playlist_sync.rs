//! The three-way sync of a linked playlist (ADR-032).
//!
//! theirs is the Bandcamp list now, base the list as last synced and ours the
//! local playlist. All three are put in one key space, `SyncKey`, merged with
//! `sequence_merge::merge3`, and the result is written as ADR-027 edit ops.
//!
//! The steps are plain functions over link state and snapshots (`settle`,
//! `plan`, `record`); only `open`, `stage` and `run` touch the store or the
//! library, so a test can stop between any two steps where a crash would.
//!
//! # Crash window
//!
//! The new base can only be saved after the edit is applied, and the edit
//! cannot be undone by us. Before it writes, a sync therefore saves the edit as
//! `pending_base`: the entries the playlist will hold, and the new base. The next sync settles a leftover
//! `pending_base` against the current entries before it merges, by one rule:
//!
//! - the entries are exactly the target: the edit landed, so adopt `new_base`;
//! - anything else: discard it and keep the old base.
//!
//! A sound rule would need the library's version history to tell "our edit
//! landed, then the user changed it" from "our edit never landed"; that was
//! rejected for a two-fault case. Discarding after a landed edit is safe: the
//! merge only adds items ours does not have, so it adds no duplicate. What it
//! loses is the memory of what the sync added, so an item the user deleted in
//! the meantime comes back once.

use std::collections::{BTreeMap, HashMap, HashSet};

use library_ipc_client::{
    CanonicalHash, ClientError, EditOp, EditOutcome, LibraryClient, PlaylistEntry, PlaylistName,
    PlaylistSnapshot, ProtocolError,
};
use playlist_edit::{ops_between, OpsBetweenError};
use playlist_import::TrackOutcome;
use sequence_merge::{merge3, occurrences};
use source_protocol::{PlaylistSyncReport, PlaylistUndo, SourcePlaylistTrack};
use thiserror::Error;

use crate::playlist_links::{
    BandcampTrackId, LinkStore, LinkStoreError, PendingBase, PendingTrack, PlaylistLink,
    PlaylistLinks,
};

/// How often a sync tries again when the playlist changed under it.
pub const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Error)]
pub enum PlaylistSyncError {
    #[error("playlist {0} is not linked")]
    NotLinked(PlaylistName),
    #[error("playlist {0} does not exist")]
    PlaylistMissing(PlaylistName),
    #[error("the link of {0} changed while it was syncing")]
    LinkChanged(PlaylistName),
    #[error("library: {0}")]
    Library(String),
    #[error("link state: {0}")]
    Store(#[from] LinkStoreError),
    #[error("the merged playlist cannot be written: {0}")]
    Merge(#[from] OpsBetweenError),
    #[error("the playlist kept changing: gave up after {0} attempts")]
    Conflicts(usize),
}

/// A library error about the playlist `name`: a missing playlist is its own
/// error, anything else a failed library call.
pub fn library_error(name: &PlaylistName, e: ClientError) -> PlaylistSyncError {
    match e {
        ClientError::Protocol(ProtocolError::PlaylistNotFound { .. }) => {
            PlaylistSyncError::PlaylistMissing(name.clone())
        }
        other => PlaylistSyncError::Library(other.to_string()),
    }
}

/// One item of the Bandcamp list as the library sees it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedItem {
    pub id: BandcampTrackId,
    pub title: String,
    pub artist: String,
    /// The library track it matched; `None` for a pending item (Missing,
    /// Ambiguous, DurationMismatch, or a track with null fields).
    pub hash: Option<CanonicalHash>,
}

/// The Bandcamp list in order, with what the matcher made of each item. A
/// track whose id is not a Bandcamp track id cannot be keyed: that is an
/// error, as leaving it out would read as a removal on Bandcamp.
pub fn resolve_items(
    tracks: &[SourcePlaylistTrack],
    outcomes: &[TrackOutcome],
) -> Result<Vec<ResolvedItem>, std::num::ParseIntError> {
    debug_assert_eq!(tracks.len(), outcomes.len(), "one outcome per track");
    tracks
        .iter()
        .zip(outcomes)
        .map(|(track, outcome)| {
            Ok(ResolvedItem {
                id: track.source_track_id.parse()?,
                title: track.title.clone(),
                artist: track.artist.clone(),
                hash: match outcome {
                    TrackOutcome::Matched(hash) => CanonicalHash::try_from(hash.clone()).ok(),
                    _ => None,
                },
            })
        })
        .collect()
}

/// An item of a list being merged: a Bandcamp track, or a playlist entry that
/// is not one (local-only tracks and unresolved lines). merge3 never removes
/// what is not in base, so the second kind survives in place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SyncKey {
    Bandcamp(BandcampTrackId),
    Local(PlaylistEntry),
}

/// What one merge decided.
#[derive(Debug)]
pub struct Plan {
    target: Vec<PlaylistEntry>,
    ops: Vec<EditOp>,
    new_base: Vec<BandcampTrackId>,
    /// The match cache to keep: the tracks of theirs and of base.
    known: BTreeMap<BandcampTrackId, CanonicalHash>,
    pending: Vec<PendingTrack>,
}

impl Plan {
    /// (added, removed, moved) by the ops.
    fn counts(&self) -> (u32, u32, u32) {
        self.ops
            .iter()
            .fold((0, 0, 0), |(added, removed, moved), op| match op {
                EditOp::Insert { hashes, .. } => (added + hashes.len() as u32, removed, moved),
                EditOp::Remove { .. } => (added, removed + 1, moved),
                EditOp::Move { .. } => (added, removed, moved + 1),
            })
    }

    fn report(&self, local: &PlaylistName, undo: Option<PlaylistUndo>) -> PlaylistSyncReport {
        let (added, removed, moved) = self.counts();
        PlaylistSyncReport {
            local: local.clone(),
            added,
            removed,
            moved,
            pending: self.pending.len() as u32,
            undo,
        }
    }
}

/// Settle the edit a crashed sync left behind (see the module docs). True
/// when there was one, so the link needs saving.
pub fn settle(link: &mut PlaylistLink, entries: &[PlaylistEntry]) -> bool {
    let Some(left) = link.pending_base.take() else {
        return false;
    };
    if entries == left.target {
        link.base = left.new_base;
    }
    true
}

/// Merge theirs (`items`) and base (`link`) into `entries`.
///
/// An item without a fresh match that is in base keeps its cached match: it
/// was applied before, and a track that only turned ambiguous or went missing
/// must not read as removed on Bandcamp. Any other item without a match is
/// pending and left out of theirs, so it is never recorded as applied.
pub fn plan(
    link: &PlaylistLink,
    items: &[ResolvedItem],
    entries: &[PlaylistEntry],
) -> Result<Plan, OpsBetweenError> {
    let mut known: BTreeMap<BandcampTrackId, CanonicalHash> = BTreeMap::new();
    let mut theirs: Vec<BandcampTrackId> = Vec::new();
    let mut pending: Vec<PendingTrack> = Vec::new();
    for item in items {
        let cached = || {
            link.base
                .contains(&item.id)
                .then(|| link.matches.get(&item.id).cloned())
                .flatten()
        };
        match item.hash.clone().or_else(cached) {
            Some(hash) => {
                known.insert(item.id, hash);
                theirs.push(item.id);
            }
            None => pending.push(PendingTrack {
                id: item.id,
                title: item.title.clone(),
                artist: item.artist.clone(),
            }),
        }
    }
    // Base items Bandcamp dropped still tell which local entries were theirs.
    for id in &link.base {
        if let Some(hash) = link.matches.get(id) {
            known.entry(*id).or_insert_with(|| hash.clone());
        }
    }

    // The Bandcamp tracks of each library track, in theirs-then-base order,
    // each occurrence once: the n-th local entry of a hash is the n-th of them.
    let mut by_hash: HashMap<&CanonicalHash, Vec<BandcampTrackId>> = HashMap::new();
    let mut seen = HashSet::new();
    for occurrence in occurrences(&theirs)
        .into_iter()
        .chain(occurrences(&link.base))
    {
        if let Some(hash) = known.get(&occurrence.key) {
            if seen.insert(occurrence.clone()) {
                by_hash.entry(hash).or_default().push(occurrence.key);
            }
        }
    }

    let base_keys: Vec<SyncKey> = link.base.iter().copied().map(SyncKey::Bandcamp).collect();
    let theirs_keys: Vec<SyncKey> = theirs.iter().copied().map(SyncKey::Bandcamp).collect();
    let mut taken: HashMap<&CanonicalHash, usize> = HashMap::new();
    let ours_keys: Vec<SyncKey> = entries
        .iter()
        .map(|entry| match entry {
            PlaylistEntry::Track { hash } => {
                let nth = taken.entry(hash).or_insert(0);
                let id = by_hash.get(hash).and_then(|ids| ids.get(*nth));
                *nth += 1;
                id.map_or_else(
                    || SyncKey::Local(entry.clone()),
                    |id| SyncKey::Bandcamp(*id),
                )
            }
            PlaylistEntry::Unresolved { .. } => SyncKey::Local(entry.clone()),
        })
        .collect();

    // Every Bandcamp key of the result comes from theirs or from a local
    // entry mapped through `by_hash`, and both are in `known`.
    let target: Vec<PlaylistEntry> = merge3(&base_keys, &theirs_keys, &ours_keys)
        .into_iter()
        .map(|key| match key {
            SyncKey::Local(entry) => entry,
            SyncKey::Bandcamp(id) => PlaylistEntry::Track {
                hash: known
                    .get(&id)
                    .expect("a Bandcamp key of the merge is a known track")
                    .clone(),
            },
        })
        .collect();
    let ops = ops_between(entries, &target)?;
    Ok(Plan {
        target,
        ops,
        new_base: theirs,
        known,
        pending,
    })
}

/// The link once `plan` is done: its base is theirs.
fn commit(link: &mut PlaylistLink, plan: &Plan, now: u64) {
    link.base = plan.new_base.clone();
    link.pending_base = None;
    link.last_synced = Some(now);
}

/// What follows an answer to the edit.
#[derive(Debug)]
pub enum Next {
    /// The sync is complete.
    Done(PlaylistSyncReport),
    /// The playlist changed under the edit: plan again from this snapshot.
    Retry(PlaylistSnapshot),
}

/// Take the library's answer to the edit of `plan`: on Applied the new base is
/// recorded, on Conflict nothing is.
pub fn record(link: &mut PlaylistLink, plan: &Plan, outcome: EditOutcome, now: u64) -> Next {
    match outcome {
        EditOutcome::Applied {
            snapshot, previous, ..
        } => {
            commit(link, plan, now);
            Next::Done(plan.report(
                &link.local,
                previous.map(|previous| PlaylistUndo {
                    version: snapshot.version,
                    previous,
                }),
            ))
        }
        EditOutcome::Conflict { current } => Next::Retry(current),
    }
}

fn save(
    store: &LinkStore,
    links: &mut PlaylistLinks,
    link: &PlaylistLink,
) -> Result<(), PlaylistSyncError> {
    links.upsert(link.clone());
    Ok(store.save(links)?)
}

/// Read the link of `local`, check it is still the link to `source` (the
/// playlist that was fetched), read the playlist and settle what a crashed
/// sync left.
pub fn open(
    client: &LibraryClient,
    store: &LinkStore,
    local: &PlaylistName,
    source: &str,
) -> Result<(PlaylistLinks, PlaylistLink, PlaylistSnapshot), PlaylistSyncError> {
    let mut links = store.load()?;
    let mut link = links
        .get(local)
        .cloned()
        .ok_or_else(|| PlaylistSyncError::NotLinked(local.clone()))?;
    if link.bandcamp != source {
        return Err(PlaylistSyncError::LinkChanged(local.clone()));
    }
    let snapshot = client
        .playlist_snapshot(local)
        .map_err(|e| library_error(local, e))?;
    if settle(&mut link, &snapshot.entries) {
        save(store, &mut links, &link)?;
    }
    Ok((links, link, snapshot))
}

/// Plan against `snapshot` and update the link: its match cache and pending
/// list always, and when there is an edit to write, `pending_base` too, saved
/// before the edit is sent.
pub fn stage(
    store: &LinkStore,
    links: &mut PlaylistLinks,
    link: &mut PlaylistLink,
    items: &[ResolvedItem],
    snapshot: &PlaylistSnapshot,
) -> Result<Plan, PlaylistSyncError> {
    let merged = plan(link, items, &snapshot.entries)?;
    link.matches = merged.known.clone();
    link.pending = merged.pending.clone();
    if !merged.ops.is_empty() {
        link.pending_base = Some(PendingBase {
            target: merged.target.clone(),
            new_base: merged.new_base.clone(),
        });
        save(store, links, link)?;
    }
    Ok(merged)
}

/// Sync the linked playlist `local` with `items`, the Bandcamp list now,
/// fetched from `source`.
///
/// The caller serialises syncs: the link state is read and written here.
pub fn sync_playlist(
    client: &LibraryClient,
    store: &LinkStore,
    local: &PlaylistName,
    source: &str,
    items: &[ResolvedItem],
    now: u64,
) -> Result<PlaylistSyncReport, PlaylistSyncError> {
    run(client, store, local, source, items, now, &|_| {})
}

/// `sync_playlist`, calling `before_edit` with the attempt number after the
/// edit is staged and before it is sent (a test races a writer there).
fn run(
    client: &LibraryClient,
    store: &LinkStore,
    local: &PlaylistName,
    source: &str,
    items: &[ResolvedItem],
    now: u64,
    before_edit: &dyn Fn(usize),
) -> Result<PlaylistSyncReport, PlaylistSyncError> {
    let (mut links, mut link, mut snapshot) = open(client, store, local, source)?;
    for attempt in 0..MAX_ATTEMPTS {
        let merged = stage(store, &mut links, &mut link, items, &snapshot)?;
        if merged.ops.is_empty() {
            commit(&mut link, &merged, now);
            save(store, &mut links, &link)?;
            return Ok(merged.report(local, None));
        }
        before_edit(attempt);
        let outcome = client
            .playlist_edit(local, &snapshot.version, merged.ops.clone())
            .map_err(|e| library_error(local, e))?;
        match record(&mut link, &merged, outcome, now) {
            Next::Done(report) => {
                save(store, &mut links, &link)?;
                return Ok(report);
            }
            Next::Retry(current) => snapshot = current,
        }
    }
    // Nothing was written: the staged edit is void.
    link.pending_base = None;
    save(store, &mut links, &link)?;
    Err(PlaylistSyncError::Conflicts(MAX_ATTEMPTS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::{ContentHash, Duplicates, FactSource};
    use music_facts::{Artist, ContentHash as MusicContentHash, MusicValue, Title};
    use pretty_assertions::assert_eq;
    use stainless_facts::{Fact, FactStreamWriter, Operation};
    use std::cell::Cell;

    /// The library tracks of the tests: hex digits '1'..'9' and 'a'..'f'.
    const TAGS: &str = "123456789abcdef";

    /// The Bandcamp playlist the test links point at.
    const SOURCE: &str = "bc";

    fn hash(tag: char) -> CanonicalHash {
        CanonicalHash::try_from(ContentHash::new(format!(
            "sha256:{}",
            tag.to_string().repeat(64)
        )))
        .expect("a full hash")
    }

    fn tag_of(entry: &PlaylistEntry) -> char {
        match entry {
            PlaylistEntry::Track { hash } => hash.as_str().chars().nth("sha256:".len()).unwrap(),
            PlaylistEntry::Unresolved { .. } => '?',
        }
    }

    fn seed_library(dir: &std::path::Path) {
        let mut writer = FactStreamWriter::open(dir.join("facts.jsonl")).expect("fact stream");
        let source = FactSource::new("test-seed", "0.0.0", "Unknown");
        let now = chrono::Utc::now();
        let mut facts = Vec::new();
        for tag in TAGS.chars() {
            let hash = MusicContentHash::new(format!("sha256:{}", tag.to_string().repeat(64)));
            facts.push(Fact::new(
                hash.clone(),
                MusicValue::Title(Title::new(format!("Track {tag}"))),
                now,
                source.clone(),
                Operation::Assert,
            ));
            facts.push(Fact::new(
                hash,
                MusicValue::Artist(Artist::new("Artist")),
                now,
                source.clone(),
                Operation::Assert,
            ));
        }
        writer.write_batch(&facts).expect("seed facts");
    }

    struct World {
        _dir: tempfile::TempDir,
        client: LibraryClient,
        store: LinkStore,
        local: PlaylistName,
    }

    /// A stub library with tracks '1'..'f', an empty playlist `mine` linked
    /// to the Bandcamp playlist `SOURCE` with no base.
    fn world(name: &str) -> World {
        use library_service_stub::service::{run_ipc_server, LibraryService};
        let dir = tempfile::TempDir::new().unwrap();
        seed_library(dir.path());
        let stub = LibraryService::from_metadata_dir(dir.path().to_path_buf());
        let address = format!(
            "ipc://{}",
            dir.path().join(format!("{name}.sock")).display()
        );
        {
            let address = address.clone();
            std::thread::spawn(move || {
                run_ipc_server(stub, &address).ok();
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let client = LibraryClient::connect(&address).expect("connect");
        let local = PlaylistName::new("mine").unwrap();
        client.playlist_new(&local, String::new()).unwrap();
        let store = LinkStore::new(dir.path().join("playlist-links.json"));
        let mut links = PlaylistLinks::default();
        links.link(local.clone(), SOURCE.to_string());
        store.save(&links).unwrap();
        World {
            _dir: dir,
            client,
            store,
            local,
        }
    }

    /// The Bandcamp track id of the item with library tag `tag`.
    fn id_of(tag: char) -> BandcampTrackId {
        BandcampTrackId::new(u64::from(tag.to_digit(16).unwrap()))
    }

    /// Bandcamp item `id`, matching the library track `tag` if `matched`.
    fn item_as(id: u64, tag: char, matched: bool) -> ResolvedItem {
        ResolvedItem {
            id: BandcampTrackId::new(id),
            title: format!("Track {tag}"),
            artist: "Artist".to_string(),
            hash: matched.then(|| hash(tag)),
        }
    }

    /// A Bandcamp list: one item per tag, its id the tag's hex value; a `-`
    /// before a tag makes that item pending (unmatched).
    fn list(spec: &str) -> Vec<ResolvedItem> {
        let mut items = Vec::new();
        let mut chars = spec.chars();
        while let Some(c) = chars.next() {
            let (tag, matched) = if c == '-' {
                (chars.next().unwrap(), false)
            } else {
                (c, true)
            };
            items.push(item_as(u64::from(tag.to_digit(16).unwrap()), tag, matched));
        }
        items
    }

    impl World {
        fn run_with(
            &self,
            items: &[ResolvedItem],
            before_edit: &dyn Fn(usize),
        ) -> Result<PlaylistSyncReport, PlaylistSyncError> {
            run(
                &self.client,
                &self.store,
                &self.local,
                SOURCE,
                items,
                42,
                before_edit,
            )
        }

        fn sync(&self, spec: &str) -> PlaylistSyncReport {
            sync_playlist(
                &self.client,
                &self.store,
                &self.local,
                SOURCE,
                &list(spec),
                42,
            )
            .expect("sync")
        }

        fn order(&self) -> String {
            self.client
                .playlist_snapshot(&self.local)
                .unwrap()
                .entries
                .iter()
                .map(tag_of)
                .collect()
        }

        /// A local edit, as the console makes it.
        fn edit(&self, ops: Vec<EditOp>) {
            let version = self.client.playlist_snapshot(&self.local).unwrap().version;
            self.client
                .playlist_edit(&self.local, &version, ops)
                .unwrap();
        }

        fn link(&self) -> PlaylistLink {
            self.store.load().unwrap().get(&self.local).unwrap().clone()
        }

        fn base(&self) -> Vec<BandcampTrackId> {
            self.link().base
        }

        /// A sync of `items` that dies where a crash would: after the edit is
        /// staged and saved, either before it reaches the library (`lands`
        /// false) or after the library applied it but before the sync heard.
        fn crash(&self, items: &[ResolvedItem], lands: bool) {
            let (mut links, mut link, snapshot) =
                open(&self.client, &self.store, &self.local, SOURCE).unwrap();
            let merged = stage(&self.store, &mut links, &mut link, items, &snapshot).unwrap();
            if lands {
                self.client
                    .playlist_edit(&self.local, &snapshot.version, merged.ops)
                    .unwrap();
            }
        }
    }

    fn ids(tags: &str) -> Vec<BandcampTrackId> {
        tags.chars().map(id_of).collect()
    }

    fn insert(at: usize, tag: char) -> EditOp {
        EditOp::Insert {
            at,
            hashes: vec![hash(tag).into()],
            duplicates: Duplicates::Keep,
        }
    }

    #[test]
    fn first_sync_adds_everything_in_bandcamp_order() {
        let w = world("first");

        let report = w.sync("123");

        assert_eq!(w.order(), "123");
        assert_eq!((report.added, report.removed, report.moved), (3, 0, 0));
        assert!(report.undo.is_some(), "the sync wrote the playlist");
        assert_eq!(w.base(), ids("123"));
        assert_eq!(w.link().last_synced, Some(42));
        assert_eq!(w.link().pending_base, None);
    }

    #[test]
    fn first_sync_skips_what_the_playlist_already_has_and_keeps_local_tracks() {
        let w = world("first-existing");
        w.edit(vec![insert(0, '2'), insert(1, 'a')]);

        let report = w.sync("123");

        assert_eq!(w.order(), "123a");
        assert_eq!(report.added, 2, "2 was there already");
    }

    #[test]
    fn bandcamp_additions_removals_and_moves_reach_the_playlist() {
        let w = world("changes");
        w.sync("1234");

        let report = w.sync("1345");
        assert_eq!(w.order(), "1345");
        assert_eq!((report.added, report.removed, report.moved), (1, 1, 0));

        let report = w.sync("4135");
        assert_eq!(w.order(), "4135");
        assert_eq!((report.added, report.removed, report.moved), (0, 0, 1));
        assert_eq!(w.base(), ids("4135"));
    }

    #[test]
    fn a_local_deletion_stays_deleted() {
        let w = world("deleted");
        w.sync("123");
        w.edit(vec![EditOp::Remove { at: 1 }]);

        let report = w.sync("123");
        assert_eq!(w.order(), "13");
        assert_eq!((report.added, report.removed, report.moved), (0, 0, 0));

        w.sync("1234");
        assert_eq!(w.order(), "134", "2 stays deleted, 4 is new");
    }

    #[test]
    fn linking_the_same_playlist_again_keeps_a_local_deletion_deleted() {
        let w = world("relink");
        w.sync("123");
        w.edit(vec![EditOp::Remove { at: 1 }]);

        let mut links = w.store.load().unwrap();
        links.link(w.local.clone(), SOURCE.to_string());
        w.store.save(&links).unwrap();
        w.sync("123");

        assert_eq!(w.order(), "13");
    }

    #[test]
    fn a_local_reorder_is_kept() {
        let w = world("reorder");
        w.sync("1234");
        w.edit(vec![EditOp::Move { from: 0, to: 3 }]);
        assert_eq!(w.order(), "2341");

        let report = w.sync("1234");

        assert_eq!(w.order(), "2341");
        assert_eq!((report.added, report.removed, report.moved), (0, 0, 0));
    }

    #[test]
    fn a_pending_item_is_added_once_it_resolves() {
        let w = world("pending");

        let report = w.sync("1-23");
        assert_eq!(w.order(), "13");
        assert_eq!(report.pending, 1);
        assert_eq!(w.base(), ids("13"), "pending is never in base");
        let link = w.link();
        assert_eq!(link.pending.len(), 1);
        assert_eq!(link.pending[0].id, id_of('2'));
        assert_eq!(link.pending[0].title, "Track 2");

        let report = w.sync("123");
        assert_eq!(w.order(), "123");
        assert_eq!((report.added, report.pending), (1, 0));
        assert_eq!(w.link().pending, vec![]);
    }

    #[test]
    fn an_applied_item_that_turns_pending_is_not_removed() {
        let w = world("turns-pending");
        w.sync("123");

        // 2 turned ambiguous: the matcher no longer answers, the cache does.
        let report = w.sync("1-23");

        assert_eq!(w.order(), "123");
        assert_eq!((report.added, report.removed, report.moved), (0, 0, 0));
        assert_eq!(report.pending, 0, "it is applied, not waiting");
    }

    #[test]
    fn the_same_track_under_two_bandcamp_ids_is_removed_one_at_a_time() {
        let w = world("same-track");
        let both = [
            item_as(101, '1', true),
            item_as(102, '1', true),
            item_as(3, '3', true),
        ];
        let sync = |items: &[ResolvedItem]| {
            sync_playlist(&w.client, &w.store, &w.local, SOURCE, items, 42).unwrap()
        };

        sync(&both);
        assert_eq!(w.order(), "113");

        // Bandcamp drops the second copy: one local copy goes, not both.
        let report = sync(&[both[0].clone(), both[2].clone()]);
        assert_eq!(w.order(), "13");
        assert_eq!(report.removed, 1);

        let report = sync(&[both[0].clone(), both[2].clone()]);
        assert_eq!((report.added, report.removed, report.moved), (0, 0, 0));
    }

    #[test]
    fn unresolved_lines_survive_in_place() {
        let w = world("unresolved");
        w.client
            .playlist_replace(
                &w.local,
                format!("{}\ndeadbeef  # gone\n{}\n", hash('1'), hash('a')),
            )
            .unwrap();

        w.sync("12");

        let snapshot = w.client.playlist_snapshot(&w.local).unwrap();
        let order: String = snapshot.entries.iter().map(tag_of).collect();
        assert_eq!(order, "12?a");
        let text = match w
            .client
            .request(&library_ipc_client::LibraryRequest::PlaylistGet {
                name: w.local.clone(),
            }) {
            Ok(library_ipc_client::LibraryResponse::PlaylistContent(text)) => text,
            other => panic!("expected content, got {other:?}"),
        };
        assert!(text.contains("deadbeef  # gone\n"), "line kept: {text:?}");
    }

    #[test]
    fn an_unchanged_sync_makes_no_edit() {
        let w = world("unchanged");
        w.sync("123");
        let before = w.client.playlist_snapshot(&w.local).unwrap();

        let report = w.sync("123");

        assert_eq!(report.undo, None);
        assert_eq!(w.client.playlist_snapshot(&w.local).unwrap(), before);
        assert_eq!(w.link().pending_base, None);
    }

    #[test]
    fn a_conflict_is_retried_against_the_fresh_snapshot() {
        let w = world("conflict");
        let edits = Cell::new(0);

        // A writer gets in before the first edit.
        let report = w
            .run_with(&list("12"), &|attempt| {
                edits.set(edits.get() + 1);
                if attempt == 0 {
                    w.edit(vec![insert(0, 'a')]);
                }
            })
            .unwrap();

        assert_eq!(edits.get(), 2, "one conflict, one retry");
        assert_eq!(w.order(), "12a", "the racing writer's track is kept");
        assert_eq!(report.added, 2);
        assert_eq!(w.base(), ids("12"));
        assert_eq!(w.link().pending_base, None);
    }

    #[test]
    fn a_playlist_that_keeps_changing_fails_after_three_attempts_and_saves_no_base() {
        let w = world("conflict-forever");
        let edits = Cell::new(0);

        let err = w
            .run_with(&list("12"), &|attempt| {
                edits.set(edits.get() + 1);
                w.edit(vec![insert(0, TAGS.chars().nth(8 + attempt).unwrap())]);
            })
            .unwrap_err();

        assert!(matches!(err, PlaylistSyncError::Conflicts(3)), "{err}");
        assert_eq!(edits.get(), 3);
        assert_eq!(w.base(), vec![], "base is saved only on Applied");
        assert_eq!(w.link().pending_base, None, "the voided edit is cleared");
    }

    #[test]
    fn a_crash_after_the_edit_landed_is_adopted_and_adds_no_duplicate() {
        let w = world("crash-after");
        w.crash(&list("123"), true);
        assert_eq!(w.order(), "123", "the edit landed");
        assert_eq!(w.base(), vec![], "the new base was not saved");
        assert!(
            w.link().pending_base.is_some(),
            "the edit was recorded first"
        );

        // Bandcamp dropped 3 meanwhile. Only an adopted base knows that the
        // sync added it, so that it is Bandcamp's to take away.
        let report = w.sync("12");

        assert_eq!(w.order(), "12");
        assert_eq!((report.added, report.removed), (0, 1));
        assert_eq!(w.base(), ids("12"));
        assert_eq!(w.link().pending_base, None);
    }

    #[test]
    fn a_crash_after_the_edit_with_nothing_new_syncs_to_no_change() {
        let w = world("crash-after-same");
        let items = [
            item_as(1, '1', true),
            item_as(1, '1', true),
            item_as(2, '2', true),
            item_as(2, '2', true),
        ];
        w.crash(&items, true);
        assert_eq!(w.order(), "1122");

        let report = sync_playlist(&w.client, &w.store, &w.local, SOURCE, &items, 42).unwrap();

        assert_eq!(w.order(), "1122", "no duplicate copy");
        assert_eq!((report.added, report.removed, report.moved), (0, 0, 0));
        assert_eq!(report.undo, None);
    }

    #[test]
    fn a_crash_before_the_edit_landed_is_discarded() {
        let w = world("crash-before");
        w.crash(&list("123"), false);
        assert_eq!(w.order(), "", "nothing was written");
        assert!(w.link().pending_base.is_some());

        let report = w.sync("123");

        assert_eq!(w.order(), "123");
        assert_eq!(report.added, 3);
        assert_eq!(w.link().pending_base, None);
        assert_eq!(w.base(), ids("123"));
    }

    #[test]
    fn a_crash_after_the_edit_and_a_later_local_addition_is_discarded_without_duplicates() {
        let w = world("crash-then-edit");
        w.crash(&list("123"), true);
        w.edit(vec![insert(3, 'a')]);

        w.sync("123");

        assert_eq!(w.order(), "123a");
        assert_eq!(w.link().pending_base, None);
    }

    /// The documented two-fault case (module docs): after a crash that landed
    /// the edit, the user's deletion of an added item looks like "the edit did
    /// not land", so the next sync adds the item once more.
    #[test]
    fn a_crash_then_a_local_deletion_brings_the_deleted_item_back_once() {
        let w = world("crash-then-delete");
        w.crash(&list("123"), true);
        w.edit(vec![EditOp::Remove { at: 1 }]);
        assert_eq!(w.order(), "13");

        w.sync("123");
        assert_eq!(w.order(), "123", "2 came back, once");

        // From here the deletion sticks as usual.
        w.edit(vec![EditOp::Remove { at: 1 }]);
        w.sync("123");
        assert_eq!(w.order(), "13");
    }

    #[test]
    fn settle_adopts_only_when_the_entries_are_the_target() {
        let target = vec![PlaylistEntry::Track { hash: hash('1') }];
        let mut link = PlaylistLink::new(PlaylistName::new("mine").unwrap(), SOURCE.into());
        link.pending_base = Some(PendingBase {
            target: target.clone(),
            new_base: ids("1"),
        });

        assert!(settle(&mut link, &[]));
        assert_eq!(link.base, vec![], "different entries: discarded");
        assert_eq!(link.pending_base, None);

        link.pending_base = Some(PendingBase {
            target: target.clone(),
            new_base: ids("1"),
        });
        assert!(settle(&mut link, &target));
        assert_eq!(link.base, ids("1"));
        assert!(!settle(&mut link, &target), "nothing left to settle");
    }

    #[test]
    fn a_sync_of_a_link_that_changed_since_the_fetch_is_refused() {
        let w = world("link-changed");
        let mut links = w.store.load().unwrap();
        links.link(w.local.clone(), "another".to_string());
        w.store.save(&links).unwrap();
        let before = std::fs::read(w.store.path()).unwrap();

        let err = sync_playlist(&w.client, &w.store, &w.local, SOURCE, &list("1"), 1).unwrap_err();

        assert!(matches!(err, PlaylistSyncError::LinkChanged(_)), "{err}");
        assert_eq!(w.order(), "");
        assert_eq!(std::fs::read(w.store.path()).unwrap(), before);
    }

    #[test]
    fn an_unlinked_playlist_is_not_synced() {
        let w = world("unlinked");
        let other = PlaylistName::new("other").unwrap();
        let err = sync_playlist(&w.client, &w.store, &other, SOURCE, &list("1"), 1).unwrap_err();
        assert!(matches!(err, PlaylistSyncError::NotLinked(_)));
    }

    #[test]
    fn a_missing_local_playlist_is_reported_as_missing_and_leaves_the_link_alone() {
        let w = world("missing");
        let gone = PlaylistName::new("gone").unwrap();
        let mut links = w.store.load().unwrap();
        links.link(gone.clone(), SOURCE.to_string());
        w.store.save(&links).unwrap();
        let before = std::fs::read(w.store.path()).unwrap();

        let err = sync_playlist(&w.client, &w.store, &gone, SOURCE, &list("1"), 1).unwrap_err();

        assert!(
            matches!(err, PlaylistSyncError::PlaylistMissing(_)),
            "{err}"
        );
        assert_eq!(std::fs::read(w.store.path()).unwrap(), before);
    }

    #[test]
    fn resolve_items_keeps_only_definitive_full_hashes_and_rejects_a_bad_id() {
        let track = |id: &str| SourcePlaylistTrack {
            position: 1,
            source_track_id: id.to_string(),
            title: "T".to_string(),
            artist: "A".to_string(),
            album: None,
            duration_secs: 1.0,
            owned: true,
        };
        let mut tracks = vec![track("10"), track("11"), track("12"), track("13")];
        let mut outcomes = vec![
            TrackOutcome::Matched(hash('1').into()),
            TrackOutcome::Matched(ContentHash::new("abcd")),
            TrackOutcome::Ambiguous(vec![hash('2').into()]),
            TrackOutcome::Missing,
        ];

        let items = resolve_items(&tracks, &outcomes).unwrap();

        let matched: Vec<bool> = items.iter().map(|i| i.hash.is_some()).collect();
        assert_eq!(matched, vec![true, false, false, false]);
        assert_eq!(items[0].id, BandcampTrackId::new(10));

        tracks.push(track("x"));
        outcomes.push(TrackOutcome::Matched(hash('3').into()));
        assert!(resolve_items(&tracks, &outcomes).is_err(), "no silent drop");
    }
}
