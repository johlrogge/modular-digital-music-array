//! Shared test fixtures: a fake library lookup and sample playlist tracks.

use library_ipc_client::ContentHash;
use track_matcher::TrackLookup;

/// (artist, title) normalised -> candidates (hash, duration secs)
type FakeLookupEntry = (&'static str, &'static str, Vec<(ContentHash, Option<u32>)>);

pub(crate) struct FakePlaylistLookup {
    pub(crate) entries: Vec<FakeLookupEntry>,
}

impl TrackLookup for FakePlaylistLookup {
    fn find_by_isrc(&self, _isrc: &str) -> Vec<ContentHash> {
        vec![]
    }
    fn find_by_artist_title(&self, artist: &str, title: &str) -> Vec<(ContentHash, Option<u32>)> {
        self.entries
            .iter()
            .find(|(a, t, _)| *a == artist && *t == title)
            .map(|(_, _, c)| c.clone())
            .unwrap_or_default()
    }
}

pub(crate) fn pl_track(
    pos: u32,
    artist: &str,
    title: &str,
    secs: f64,
) -> source_protocol::SourcePlaylistTrack {
    source_protocol::SourcePlaylistTrack {
        position: pos,
        source_track_id: format!("t{pos}"),
        title: title.to_string(),
        artist: artist.to_string(),
        album: None,
        duration_secs: secs,
        owned: true,
    }
}

pub(crate) fn hash(s: &str) -> ContentHash {
    ContentHash::new(format!("sha256:{s}"))
}

pub(crate) fn sample_lookup() -> FakePlaylistLookup {
    FakePlaylistLookup {
        entries: vec![
            ("a", "one", vec![(hash("1111111111111111"), Some(300))]),
            (
                "b",
                "two",
                vec![
                    (hash("2222222222222222"), Some(200)),
                    (hash("3333333333333333"), Some(200)),
                ],
            ),
            ("d", "four", vec![(hash("4444444444444444"), Some(100))]),
        ],
    }
}

pub(crate) fn sample_tracks() -> Vec<source_protocol::SourcePlaylistTrack> {
    vec![
        pl_track(1, "A", "One", 300.2),
        pl_track(2, "B", "Two", 200.0),
        pl_track(3, "C", "Three", 150.0),
        pl_track(4, "D", "Four", 100.0),
    ]
}
