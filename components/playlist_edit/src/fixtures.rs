//! Test tracks shared by the tests of this crate.

use library_ipc_protocol::{ContentHash, TrackInfo};
use library_projection::HashedTrack;

pub fn info(hash: String, title: &str) -> TrackInfo {
    TrackInfo {
        title: Some(title.to_string()),
        artist: Some("Artist".to_string()),
        ..TrackInfo::new(ContentHash::new(hash))
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
