//! Cucumber World — holds clients, last results, and test state.

use crate::harness::{self, SeedTrack, TestEnv};
use mdma_client::{ContentHash, LibraryBackend, PlaybackBackend, TrackInfo};

/// The cucumber World. Each scenario gets a fresh instance.
#[derive(cucumber::World)]
#[world(init = Self::new)]
pub struct MdmaWorld {
    /// Test environment (services + temp dirs). None until Background seeds it.
    env: Option<TestEnv>,

    /// Last search results for assertions.
    pub last_search_results: Vec<TrackInfo>,

    /// Last queue listing for assertions.
    pub last_queue: Vec<ContentHash>,

    /// Last error message, if any.
    pub last_error: Option<String>,

    /// Tracks seeded in Background, before the env is booted.
    pub pending_tracks: Vec<SeedTrack>,
}

impl std::fmt::Debug for MdmaWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdmaWorld")
            .field("has_env", &self.env.is_some())
            .field("search_results", &self.last_search_results.len())
            .field("queue_len", &self.last_queue.len())
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl MdmaWorld {
    fn new() -> Self {
        Self {
            env: None,
            last_search_results: vec![],
            last_queue: vec![],
            last_error: None,
            pending_tracks: vec![],
        }
    }

    /// Boot the test environment with the tracks accumulated in Background steps.
    pub fn ensure_env(&mut self) {
        if self.env.is_none() {
            let tracks = std::mem::take(&mut self.pending_tracks);
            self.env = Some(harness::boot_test_env(&tracks));
        }
    }

    /// Get a reference to the library client. Panics if env not booted.
    pub fn library(&mut self) -> &LibraryBackend {
        self.ensure_env();
        &self.env.as_ref().unwrap().library
    }

    /// Get a reference to the playback client. Panics if env not booted.
    pub fn playback(&mut self) -> &PlaybackBackend {
        self.ensure_env();
        &self.env.as_ref().unwrap().playback
    }
}
