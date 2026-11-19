//! Link state of the Bandcamp playlist sync (ADR-032 §5).
//!
//! `mdma-bandcamp` owns `playlist-links.json`, next to its download cache. For
//! each linked local playlist it keeps the Bandcamp list as last synced (base),
//! the items the library could not match yet (pending), a cache of which
//! library track each Bandcamp track matched, and the edit a sync was about to
//! write (`pending_base`, ADR-032 §6). These are not facts: playlists are not
//! fact entities, and base lives in Bandcamp-track space.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use library_ipc_client::{CanonicalHash, PlaylistEntry, PlaylistName};
use serde::{Deserialize, Serialize};
use source_protocol::{
    BandcampPlaylistId, PendingPlaylistTrack, PlaylistLinkInfo, SourcePlaylistSummary,
};
use thiserror::Error;

/// File name of the link state, in the directory of the download cache.
pub const LINKS_FILE: &str = "playlist-links.json";

/// A track on a Bandcamp playlist, by Bandcamp's track id (the id type of
/// `bandcamp_api`'s playlist tracks). Not the purchase level `ItemId` of a
/// collection item: one album item holds many tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BandcampTrackId(u64);

impl BandcampTrackId {
    #[cfg(test)]
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl std::str::FromStr for BandcampTrackId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

impl std::fmt::Display for BandcampTrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A Bandcamp item the library cannot match yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTrack {
    pub id: BandcampTrackId,
    pub title: String,
    pub artist: String,
}

/// The edit a sync is about to write, saved before it is written so that a
/// crash between the edit and the save of the new base cannot be mistaken for
/// "nothing happened" (ADR-032 §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBase {
    /// The entries the playlist holds once the edit is applied.
    pub target: Vec<PlaylistEntry>,
    /// The base once the edit is applied.
    pub new_base: Vec<BandcampTrackId>,
}

/// One linked local playlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistLink {
    pub local: PlaylistName,
    /// The Bandcamp playlist: a URL or a slug.
    pub bandcamp: String,
    /// The playlist's stable id, which the fan's listing joins on whatever
    /// spelling `bandcamp` has. Links made before ids were kept have none
    /// until their next link or sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandcamp_id: Option<BandcampPlaylistId>,
    /// The matched Bandcamp tracks as last synced, in order. Never holds a
    /// pending item.
    pub base: Vec<BandcampTrackId>,
    /// Items the library cannot match yet, in Bandcamp order.
    pub pending: Vec<PendingTrack>,
    /// Which library track each Bandcamp track matched.
    pub matches: BTreeMap<BandcampTrackId, CanonicalHash>,
    /// Unix seconds of the last completed sync.
    pub last_synced: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_base: Option<PendingBase>,
}

impl PlaylistLink {
    /// A link with no base: its first sync adds everything Bandcamp has.
    pub fn new(local: PlaylistName, bandcamp: String) -> Self {
        Self {
            local,
            bandcamp,
            bandcamp_id: None,
            base: Vec::new(),
            pending: Vec::new(),
            matches: BTreeMap::new(),
            last_synced: None,
            pending_base: None,
        }
    }

    /// The link as the wire shows it.
    pub fn info(&self) -> PlaylistLinkInfo {
        PlaylistLinkInfo {
            local: self.local.clone(),
            source: self.bandcamp.clone(),
            pending: self
                .pending
                .iter()
                .map(|p| PendingPlaylistTrack {
                    source_track_id: p.id.to_string(),
                    title: p.title.clone(),
                    artist: p.artist.clone(),
                })
                .collect(),
            last_synced: self.last_synced,
            bandcamp_id: self.bandcamp_id,
        }
    }
}

/// Every link, in the order they were made.
///
/// Links are keyed by playlist name; orphans are pruned on read (see
/// `retain_existing`) until ADR-035 (service-owned facts on library subjects) gives a link the life of its playlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistLinks {
    pub links: Vec<PlaylistLink>,
}

impl PlaylistLinks {
    pub fn get(&self, local: &PlaylistName) -> Option<&PlaylistLink> {
        self.links.iter().find(|l| l.local == *local)
    }

    /// Link `local` to the Bandcamp playlist `url` (ADR-032 §8). A link always
    /// starts fresh, also when `local` is linked to the same playlist already:
    /// no base, no pending items, no match cache. The next sync is then a
    /// first sync, which adds what is missing and skips what is present, so a
    /// stale base can never read as local deletions.
    pub fn link(
        &mut self,
        local: PlaylistName,
        url: String,
        id: Option<BandcampPlaylistId>,
    ) -> PlaylistLinkInfo {
        let mut link = PlaylistLink::new(local, url);
        link.bandcamp_id = id;
        let info = link.info();
        self.upsert(link);
        info
    }

    /// Drop the link of `local`, with its base. True when it had one.
    pub fn unlink(&mut self, local: &PlaylistName) -> bool {
        let before = self.links.len();
        self.links.retain(|l| l.local != *local);
        self.links.len() != before
    }

    /// Drop the links of playlists that are not in `existing`: an orphan is
    /// unlinked. True when a link was dropped. The one place orphans go.
    /// A safety net: ADR-035 (service-owned facts on library subjects) replaces it.
    pub fn retain_existing(&mut self, existing: &[PlaylistName]) -> bool {
        let before = self.links.len();
        self.links.retain(|l| existing.contains(&l.local));
        self.links.len() != before
    }

    /// Record the id of the playlist `source`, if `local` is still linked to
    /// it and does not have that id yet. True when the link changed.
    pub fn set_playlist_id(
        &mut self,
        local: &PlaylistName,
        source: &str,
        id: BandcampPlaylistId,
    ) -> bool {
        match self
            .links
            .iter_mut()
            .find(|l| l.local == *local && l.bandcamp == source)
        {
            Some(link) if link.bandcamp_id != Some(id) => {
                link.bandcamp_id = Some(id);
                true
            }
            _ => false,
        }
    }

    /// Give every link that has no id the id of the listed playlist whose URL
    /// it holds (links made before ids were kept). True when a link changed.
    pub fn backfill_ids(&mut self, listed: &[SourcePlaylistSummary]) -> bool {
        let mut changed = false;
        for link in self.links.iter_mut().filter(|l| l.bandcamp_id.is_none()) {
            let id = listed
                .iter()
                .find(|p| p.url == link.bandcamp)
                .and_then(|p| p.id.parse().ok());
            if id.is_some() {
                link.bandcamp_id = id;
                changed = true;
            }
        }
        changed
    }

    /// Store `link`, replacing the link of the same local playlist.
    pub fn upsert(&mut self, link: PlaylistLink) {
        match self.links.iter_mut().find(|l| l.local == link.local) {
            Some(slot) => *slot = link,
            None => self.links.push(link),
        }
    }
}

#[derive(Debug, Error)]
pub enum LinkStoreError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid link state: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// The link state file.
#[derive(Debug, Clone)]
pub struct LinkStore {
    path: PathBuf,
}

impl LinkStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The store that lives next to the download cache file `cache_path`.
    pub fn beside(cache_path: &Path) -> Self {
        let dir = cache_path.parent().unwrap_or_else(|| Path::new("."));
        Self::new(dir.join(LINKS_FILE))
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved links; a missing file is no links.
    pub fn load(&self) -> Result<PlaylistLinks, LinkStoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| LinkStoreError::Parse {
                path: self.path.clone(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PlaylistLinks::default()),
            Err(source) => Err(LinkStoreError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Replace the saved links, atomically: a reader (or a crash) sees the old
    /// file or the new one, never a partial one.
    pub fn save(&self, links: &PlaylistLinks) -> Result<(), LinkStoreError> {
        let bytes = serde_json::to_vec_pretty(links).map_err(|source| LinkStoreError::Parse {
            path: self.path.clone(),
            source,
        })?;
        private_file::write_file_atomic(&self.path, &bytes, 0o644).map_err(|source| {
            LinkStoreError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::ContentHash;
    use pretty_assertions::assert_eq;

    pub(crate) fn canonical(tag: char) -> CanonicalHash {
        CanonicalHash::try_from(ContentHash::new(format!(
            "sha256:{}",
            tag.to_string().repeat(64)
        )))
        .expect("a full hash")
    }

    fn name(s: &str) -> PlaylistName {
        PlaylistName::new(s).unwrap()
    }

    fn sample() -> PlaylistLinks {
        let mut link =
            PlaylistLink::new(name("friday"), "https://bandcamp.com/x/playlist/f".into());
        link.base = vec![BandcampTrackId::new(1), BandcampTrackId::new(2)];
        link.pending = vec![PendingTrack {
            id: BandcampTrackId::new(3),
            title: "Three".into(),
            artist: "C".into(),
        }];
        link.matches.insert(BandcampTrackId::new(1), canonical('a'));
        link.matches.insert(BandcampTrackId::new(2), canonical('b'));
        link.last_synced = Some(1_700_000_000);
        link.pending_base = Some(PendingBase {
            target: vec![PlaylistEntry::Track {
                hash: canonical('a'),
            }],
            new_base: vec![BandcampTrackId::new(1)],
        });
        PlaylistLinks { links: vec![link] }
    }

    #[test]
    fn a_missing_file_is_no_links() {
        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::new(dir.path().join(LINKS_FILE));
        assert_eq!(store.load().unwrap(), PlaylistLinks::default());
    }

    #[test]
    fn links_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::new(dir.path().join(LINKS_FILE));
        let links = sample();

        store.save(&links).unwrap();

        assert_eq!(store.load().unwrap(), links);
    }

    #[test]
    fn the_store_sits_beside_the_cache_file() {
        let store = LinkStore::beside(Path::new("/var/lib/mdma/bandcamp.cache"));
        assert_eq!(store.path(), Path::new("/var/lib/mdma/playlist-links.json"));
    }

    #[test]
    fn a_save_leaves_only_the_file_and_replaces_it_whole() {
        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::new(dir.path().join(LINKS_FILE));
        store.save(&sample()).unwrap();
        store.save(&PlaylistLinks::default()).unwrap();

        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, vec![LINKS_FILE.to_string()], "no temp file is left");
        assert_eq!(store.load().unwrap(), PlaylistLinks::default());
    }

    #[test]
    fn a_failed_save_leaves_the_old_file_and_no_partial_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::new(dir.path().join(LINKS_FILE));
        store.save(&sample()).unwrap();
        let before = std::fs::read(store.path()).unwrap();

        // A store whose directory is gone cannot write; the other one is untouched.
        let gone = LinkStore::new(dir.path().join("gone").join(LINKS_FILE));
        assert!(matches!(
            gone.save(&PlaylistLinks::default()),
            Err(LinkStoreError::Write { .. })
        ));
        assert!(!dir.path().join("gone").exists());
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
    }

    #[test]
    fn garbage_is_a_parse_error_not_no_links() {
        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::new(dir.path().join(LINKS_FILE));
        std::fs::write(store.path(), b"{ not json").unwrap();
        assert!(matches!(store.load(), Err(LinkStoreError::Parse { .. })));
    }

    #[test]
    fn a_track_id_parses_from_its_wire_form() {
        assert_eq!(
            "1150569772".parse::<BandcampTrackId>().unwrap(),
            BandcampTrackId::new(1150569772)
        );
        assert!("t1".parse::<BandcampTrackId>().is_err());
    }

    #[test]
    fn linking_always_starts_fresh_even_the_same_playlist_again() {
        let mut links = sample();
        let url = "https://bandcamp.com/x/playlist/f".to_string();

        // The same playlist again: base, pending, matches and the unfinished
        // edit are all gone, so the next sync is a first sync.
        let info = links.link(name("friday"), url, Some(BandcampPlaylistId::new(7)));
        assert!(info.pending.is_empty());
        assert_eq!(info.last_synced, None);
        assert_eq!(info.bandcamp_id, Some(BandcampPlaylistId::new(7)));
        let link = links.get(&name("friday")).unwrap();
        assert!(link.base.is_empty());
        assert!(link.matches.is_empty());
        assert_eq!(link.pending_base, None);

        // Another playlist: the same.
        links.link(
            name("friday"),
            "https://bandcamp.com/x/playlist/g".to_string(),
            None,
        );
        let link = links.get(&name("friday")).unwrap();
        assert_eq!(link.bandcamp, "https://bandcamp.com/x/playlist/g");
        assert!(link.base.is_empty());
        assert_eq!(links.links.len(), 1);
    }

    #[test]
    fn unlinking_drops_the_link_and_its_base() {
        let mut links = sample();
        assert!(!links.unlink(&name("other")));
        assert!(links.unlink(&name("friday")));
        assert!(links.get(&name("friday")).is_none());
        assert!(!links.unlink(&name("friday")));
    }

    #[test]
    fn a_link_to_a_playlist_that_does_not_exist_is_pruned() {
        let mut links = sample();
        links.upsert(PlaylistLink::new(name("heavier"), "h".into()));

        assert!(!links.retain_existing(&[name("friday"), name("heavier")]));
        assert_eq!(links.links.len(), 2);

        assert!(links.retain_existing(&[name("friday")]));
        assert_eq!(links.links.len(), 1);
        assert!(links.get(&name("heavier")).is_none());
    }

    #[test]
    fn a_link_keeps_the_playlist_id_and_an_old_link_without_one_still_loads() {
        let mut links = PlaylistLinks::default();
        let info = links.link(
            name("friday"),
            "https://bandcamp.com/x/playlist/f".into(),
            Some(BandcampPlaylistId::new(982341)),
        );
        assert_eq!(info.bandcamp_id, Some(BandcampPlaylistId::new(982341)));

        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::new(dir.path().join(LINKS_FILE));
        store.save(&links).unwrap();
        assert_eq!(store.load().unwrap(), links);

        // A file written before ids were kept has no such field.
        let mut old: serde_json::Value = serde_json::to_value(&links).unwrap();
        old["links"][0]
            .as_object_mut()
            .unwrap()
            .remove("bandcamp_id");
        std::fs::write(store.path(), serde_json::to_vec(&old).unwrap()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.get(&name("friday")).unwrap().bandcamp_id, None);
    }

    fn listed(id: &str, url: &str) -> SourcePlaylistSummary {
        SourcePlaylistSummary {
            id: id.to_string(),
            title: "F".to_string(),
            url: url.to_string(),
            slug: "f".to_string(),
            track_count: 1,
            is_private: false,
        }
    }

    #[test]
    fn a_listing_fills_the_id_of_an_id_less_link_with_the_same_url() {
        let mut links = sample();
        let other = listed("8", "https://bandcamp.com/x/playlist/other");
        let same = listed("7", "https://bandcamp.com/x/playlist/f");

        assert!(!links.backfill_ids(std::slice::from_ref(&other)));
        assert_eq!(links.get(&name("friday")).unwrap().bandcamp_id, None);

        assert!(links.backfill_ids(&[other, same.clone()]));
        let link = links.get(&name("friday")).unwrap();
        assert_eq!(link.bandcamp_id, Some(BandcampPlaylistId::new(7)));
        assert_eq!(link.info().bandcamp_id, Some(BandcampPlaylistId::new(7)));
        assert_eq!(link.base.len(), 2, "the base is untouched");

        assert!(!links.backfill_ids(&[same]), "nothing left to fill");
    }

    #[test]
    fn set_playlist_id_only_touches_the_link_still_pointing_at_the_source() {
        let mut links = sample();
        let id = BandcampPlaylistId::new(7);

        assert!(!links.set_playlist_id(&name("friday"), "elsewhere", id));
        assert_eq!(links.get(&name("friday")).unwrap().bandcamp_id, None);

        assert!(links.set_playlist_id(&name("friday"), "https://bandcamp.com/x/playlist/f", id));
        assert!(!links.set_playlist_id(&name("friday"), "https://bandcamp.com/x/playlist/f", id));
        assert_eq!(links.get(&name("friday")).unwrap().bandcamp_id, Some(id));
    }

    #[test]
    fn upsert_replaces_the_link_of_the_same_playlist() {
        let mut links = sample();
        links.upsert(PlaylistLink::new(name("friday"), "other".into()));
        links.upsert(PlaylistLink::new(name("saturday"), "sat".into()));

        assert_eq!(links.links.len(), 2);
        assert_eq!(links.get(&name("friday")).unwrap().bandcamp, "other");
        assert!(links.get(&name("friday")).unwrap().base.is_empty());
    }
}
