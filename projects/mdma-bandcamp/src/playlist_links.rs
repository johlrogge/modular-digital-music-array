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
use source_protocol::{PendingPlaylistTrack, PlaylistLinkInfo};
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
        }
    }
}

/// Every link, in the order they were made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistLinks {
    pub links: Vec<PlaylistLink>,
}

impl PlaylistLinks {
    pub fn get(&self, local: &PlaylistName) -> Option<&PlaylistLink> {
        self.links.iter().find(|l| l.local == *local)
    }

    /// Link `local` to the Bandcamp playlist `url` (ADR-032 §8). Linking what
    /// `local` is already linked to keeps the link, with its base: a reset
    /// would bring back every track the user deleted. A different playlist
    /// replaces the link and starts without a base.
    pub fn link(&mut self, local: PlaylistName, url: String) -> PlaylistLinkInfo {
        if let Some(existing) = self.get(&local).filter(|l| l.bandcamp == url) {
            return existing.info();
        }
        let link = PlaylistLink::new(local, url);
        let info = link.info();
        self.upsert(link);
        info
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
    fn linking_the_same_playlist_again_keeps_the_base_and_another_resets_it() {
        let mut links = sample();
        let url = "https://bandcamp.com/x/playlist/f".to_string();

        let info = links.link(name("friday"), url);
        assert_eq!(info.pending.len(), 1);
        assert_eq!(links.get(&name("friday")).unwrap().base.len(), 2);

        links.link(
            name("friday"),
            "https://bandcamp.com/x/playlist/g".to_string(),
        );
        let link = links.get(&name("friday")).unwrap();
        assert_eq!(link.bandcamp, "https://bandcamp.com/x/playlist/g");
        assert!(link.base.is_empty());
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
