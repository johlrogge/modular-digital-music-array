//! Test tracks shared by the tests of this crate.

use library_ipc_protocol::{ContentHash, TrackInfo};
use library_projection::HashedTrack;

pub fn info(hash: String, title: &str) -> TrackInfo {
    TrackInfo {
        content_hash: ContentHash::new(hash),
        title: Some(title.to_string()),
        artist: Some("Artist".to_string()),
        album: None,
        duration: None,
        bpm: None,
        key: None,
        blob_path: None,
        cover_art_path: None,
        track_number: None,
        disc_number: None,
        added: None,
        started: None,
        stopped: None,
        memory_cues: vec![],
        beat_grid: None,
        role: None,
        energy: None,
        genre: None,
        label: None,
        styles: vec![],
        marks: vec![],
    }
}

pub struct TestTrack {
    pub info: TrackInfo,
    pub hidden: bool,
}

impl HashedTrack for TestTrack {
    fn content_hash(&self) -> &str {
        self.info.content_hash.as_str()
    }

    fn title(&self) -> Option<&str> {
        self.info.title.as_deref()
    }

    fn is_hidden(&self) -> bool {
        self.hidden
    }
}

pub fn test_track(hash: String, title: &str) -> TestTrack {
    TestTrack {
        info: info(hash, title),
        hidden: false,
    }
}
