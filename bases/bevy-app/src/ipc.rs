use std::sync::{
    mpsc::{self, Receiver, Sender},
    Mutex,
};

use bevy::prelude::*;
use mdma_client::{ContentHash, LibraryBackend, PlaylistName, TrackInfo, TrackQuery};

use crate::config::DjWorkspaceConfig;

// =========================================================================
// Public protocol types
// =========================================================================

/// Requests that the Bevy app sends to the worker thread.
#[derive(Debug, Clone)]
pub enum IpcRequest {
    ListPlaylists,
    GetPlaylistTracks(PlaylistName),
    Search(Box<TrackQuery>),
}

/// Events the worker thread sends back to the Bevy app.
#[derive(Debug)]
pub enum IpcEvent {
    Connected,
    ConnectionFailed(String),
    Playlists(Vec<PlaylistName>),
    PlaylistTracks {
        name: PlaylistName,
        tracks: Vec<TrackInfo>,
    },
    SearchResults(Vec<TrackInfo>),
    RequestFailed {
        context: String,
        message: String,
    },
}

// =========================================================================
// LibraryPort abstraction (allows unit testing without a real backend)
// =========================================================================

pub(crate) trait LibraryPort {
    fn playlist_list(&self) -> Result<Vec<PlaylistName>, String>;
    fn playlist_get(&self, name: &PlaylistName) -> Result<Vec<ContentHash>, String>;
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, String>;
    fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, String>;
}

impl LibraryPort for LibraryBackend {
    fn playlist_list(&self) -> Result<Vec<PlaylistName>, String> {
        self.playlist_list().map_err(|e| e.to_string())
    }

    fn playlist_get(&self, name: &PlaylistName) -> Result<Vec<ContentHash>, String> {
        self.playlist_get(name).map_err(|e| e.to_string())
    }

    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, String> {
        self.get_track(hash).map_err(|e| e.to_string())
    }

    fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, String> {
        self.search(query).map_err(|e| e.to_string())
    }
}

// =========================================================================
// Pure request→event mapping
// =========================================================================

/// Map a single IpcRequest to an IpcEvent using the given port.
///
/// GetPlaylistTracks resolves each hash individually; hashes that fail
/// get_track are skipped with a tracing::warn! (matching the pattern in
/// LibraryBackend::resolve_and_format_playlist).
pub(crate) fn handle_request(port: &impl LibraryPort, req: IpcRequest) -> IpcEvent {
    match req {
        IpcRequest::ListPlaylists => match port.playlist_list() {
            Ok(names) => IpcEvent::Playlists(names),
            Err(e) => IpcEvent::RequestFailed {
                context: "ListPlaylists".to_string(),
                message: e,
            },
        },

        IpcRequest::GetPlaylistTracks(name) => match port.playlist_get(&name) {
            Err(e) => IpcEvent::RequestFailed {
                context: format!("GetPlaylistTracks({})", name.as_str()),
                message: e,
            },
            Ok(hashes) => {
                let tracks: Vec<TrackInfo> = hashes
                    .iter()
                    .filter_map(|h| match port.get_track(h) {
                        Ok(t) => Some(t),
                        Err(e) => {
                            tracing::warn!(
                                hash = h.as_str(),
                                error = %e,
                                "Skipping unresolvable track hash in playlist"
                            );
                            None
                        }
                    })
                    .collect();
                IpcEvent::PlaylistTracks { name, tracks }
            }
        },

        IpcRequest::Search(query) => match port.search(&query) {
            Ok(tracks) => IpcEvent::SearchResults(tracks),
            Err(e) => IpcEvent::RequestFailed {
                context: "Search".to_string(),
                message: e,
            },
        },
    }
}

// =========================================================================
// Bevy Resources
// =========================================================================

/// Bevy Resource holding the IPC channels to/from the worker thread.
#[derive(Resource)]
pub struct IpcChannels {
    pub tx: Sender<IpcRequest>,
    /// Mutex exists solely to satisfy `Resource: Sync`; there is a single
    /// consumer (`poll_ipc_events`) and this lock is never contended.
    pub rx: Mutex<Receiver<IpcEvent>>,
}

/// Connection state visible to the Bevy UI.
#[derive(Resource, Debug, Clone)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Failed(String),
}

// =========================================================================
// Worker thread
// =========================================================================

/// Spawn the IPC worker thread. Returns the request sender and event receiver.
///
/// The worker calls LibraryBackend::connect ON the worker thread (never the
/// main thread), emits Connected or ConnectionFailed, then loops
/// `recv -> handle_request -> send` until the request channel closes.
pub fn spawn_ipc_worker(config: DjWorkspaceConfig) -> (Sender<IpcRequest>, Receiver<IpcEvent>) {
    let (req_tx, req_rx) = mpsc::channel::<IpcRequest>();
    let (evt_tx, evt_rx) = mpsc::channel::<IpcEvent>();

    std::thread::spawn(move || {
        // Connect on the worker thread — never on the main thread.
        let backend = LibraryBackend::connect(config.gateway.as_deref(), &config.library_socket);

        match backend {
            Err(e) => {
                let _ = evt_tx.send(IpcEvent::ConnectionFailed(e.to_string()));
                // Worker exits — nothing more to do.
            }
            Ok(port) => {
                let _ = evt_tx.send(IpcEvent::Connected);

                // Process requests until the sender side is dropped.
                for req in req_rx {
                    let event = handle_request(&port, req);
                    if evt_tx.send(event).is_err() {
                        break; // Bevy side dropped the receiver.
                    }
                }
            }
        }
    });

    (req_tx, evt_rx)
}

// =========================================================================
// Bevy systems
// =========================================================================

/// Startup system: reads DjWorkspaceConfig, spawns the IPC worker,
/// inserts IpcChannels + ConnectionStatus::Connecting, and fires off
/// an initial ListPlaylists request so the first frame has something in flight.
pub fn setup_ipc(mut commands: Commands, config: Res<DjWorkspaceConfig>) {
    let (tx, rx) = spawn_ipc_worker(config.clone());

    // Kick off the first useful request immediately.
    let _ = tx.send(IpcRequest::ListPlaylists);

    commands.insert_resource(IpcChannels {
        tx,
        rx: Mutex::new(rx),
    });
    commands.insert_resource(ConnectionStatus::Connecting);
}

/// Update system: drain the event channel and handle connection bookkeeping.
pub fn poll_ipc_events(
    channels: Res<IpcChannels>,
    mut status: ResMut<ConnectionStatus>,
    mut playlist_state: ResMut<crate::playlists::PlaylistState>,
    mut search_results: ResMut<crate::results::SearchResults>,
    mut filter_state: ResMut<crate::filters::FilterState>,
) {
    let Ok(rx) = channels.rx.lock() else {
        tracing::warn!("IPC receiver mutex poisoned");
        return;
    };

    loop {
        match rx.try_recv() {
            Ok(IpcEvent::Connected) => {
                *status = ConnectionStatus::Connected;
            }
            Ok(IpcEvent::ConnectionFailed(msg)) => {
                tracing::warn!(error = %msg, "IPC connection failed");
                *status = ConnectionStatus::Failed(msg);
            }
            Ok(IpcEvent::RequestFailed { context, message }) => {
                tracing::warn!(context = %context, error = %message, "IPC request failed");
            }
            Ok(IpcEvent::Playlists(names)) => {
                playlist_state.names = names;
            }
            Ok(IpcEvent::PlaylistTracks { name, tracks }) => {
                // Ignore stale responses from a previously-selected playlist.
                let current_name = playlist_state
                    .selected
                    .and_then(|idx| playlist_state.names.get(idx));
                if current_name.map(|n| n == &name).unwrap_or(false) {
                    playlist_state.tracks = tracks;
                    playlist_state.loading = false;
                } else {
                    tracing::debug!(
                        received = %name.as_str(),
                        "Ignoring stale PlaylistTracks response"
                    );
                }
            }
            Ok(IpcEvent::SearchResults(mut tracks)) => {
                // Concurrent-search overwrite is prevented upstream: the Search
                // button is disabled while `filter_state.searching` is true
                // (ui.rs), so at most one search is in-flight at a time.
                crate::results::sort_tracks(
                    &mut tracks,
                    search_results.sort,
                    search_results.ascending,
                );
                search_results.tracks = tracks;
                filter_state.searching = false;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }
}

// =========================================================================
// Unit tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ------------------------------------------------------------------
    // FakeLibraryPort
    // ------------------------------------------------------------------

    struct FakeLibraryPort {
        playlists: Vec<PlaylistName>,
        /// playlist name -> list of hashes (as strings)
        playlist_tracks: HashMap<String, Vec<String>>,
        /// hash -> Ok(TrackInfo) or Err(message)
        tracks: HashMap<String, Result<TrackInfo, String>>,
        search_result: Result<Vec<TrackInfo>, String>,
    }

    impl FakeLibraryPort {
        fn new() -> Self {
            Self {
                playlists: vec![],
                playlist_tracks: HashMap::new(),
                tracks: HashMap::new(),
                search_result: Ok(vec![]),
            }
        }
    }

    fn make_playlist_name(s: &str) -> PlaylistName {
        PlaylistName::new(s).expect("valid playlist name in test")
    }

    fn make_track(hash: &str, title: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: Some(title.to_string()),
            artist: None,
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
        }
    }

    impl LibraryPort for FakeLibraryPort {
        fn playlist_list(&self) -> Result<Vec<PlaylistName>, String> {
            Ok(self.playlists.clone())
        }

        fn playlist_get(&self, name: &PlaylistName) -> Result<Vec<ContentHash>, String> {
            match self.playlist_tracks.get(name.as_str()) {
                Some(hashes) => Ok(hashes.iter().map(|h| ContentHash::new(h)).collect()),
                None => Err(format!("Playlist not found: {}", name.as_str())),
            }
        }

        fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, String> {
            match self.tracks.get(hash.as_str()) {
                Some(Ok(t)) => Ok(t.clone()),
                Some(Err(e)) => Err(e.clone()),
                None => Err(format!("Track not found: {}", hash.as_str())),
            }
        }

        fn search(&self, _query: &TrackQuery) -> Result<Vec<TrackInfo>, String> {
            self.search_result.clone()
        }
    }

    // ------------------------------------------------------------------
    // Tests for handle_request
    // ------------------------------------------------------------------

    #[test]
    fn list_playlists_returns_playlists_event() {
        let mut port = FakeLibraryPort::new();
        port.playlists = vec![make_playlist_name("alpha"), make_playlist_name("beta")];

        let evt = handle_request(&port, IpcRequest::ListPlaylists);

        match evt {
            IpcEvent::Playlists(names) => {
                assert_eq!(names.len(), 2);
                assert_eq!(names[0].as_str(), "alpha");
                assert_eq!(names[1].as_str(), "beta");
            }
            other => panic!("Expected Playlists, got {:?}", other),
        }
    }

    #[test]
    fn list_playlists_error_maps_to_request_failed() {
        struct FailPort;
        impl LibraryPort for FailPort {
            fn playlist_list(&self) -> Result<Vec<PlaylistName>, String> {
                Err("socket dead".to_string())
            }
            fn playlist_get(&self, _: &PlaylistName) -> Result<Vec<ContentHash>, String> {
                unimplemented!()
            }
            fn get_track(&self, _: &ContentHash) -> Result<TrackInfo, String> {
                unimplemented!()
            }
            fn search(&self, _: &TrackQuery) -> Result<Vec<TrackInfo>, String> {
                unimplemented!()
            }
        }

        let evt = handle_request(&FailPort, IpcRequest::ListPlaylists);
        match evt {
            IpcEvent::RequestFailed { context, message } => {
                assert_eq!(context, "ListPlaylists");
                assert!(message.contains("socket dead"));
            }
            other => panic!("Expected RequestFailed, got {:?}", other),
        }
    }

    #[test]
    fn get_playlist_tracks_returns_all_tracks_when_all_resolve() {
        let mut port = FakeLibraryPort::new();
        let pname = make_playlist_name("deep-house");
        port.playlist_tracks.insert(
            "deep-house".to_string(),
            vec!["sha256:aaa".to_string(), "sha256:bbb".to_string()],
        );
        port.tracks.insert(
            "sha256:aaa".to_string(),
            Ok(make_track("sha256:aaa", "Track A")),
        );
        port.tracks.insert(
            "sha256:bbb".to_string(),
            Ok(make_track("sha256:bbb", "Track B")),
        );

        let evt = handle_request(&port, IpcRequest::GetPlaylistTracks(pname.clone()));

        match evt {
            IpcEvent::PlaylistTracks { name, tracks } => {
                assert_eq!(name.as_str(), "deep-house");
                assert_eq!(tracks.len(), 2);
                assert_eq!(tracks[0].title.as_deref(), Some("Track A"));
                assert_eq!(tracks[1].title.as_deref(), Some("Track B"));
            }
            other => panic!("Expected PlaylistTracks, got {:?}", other),
        }
    }

    #[test]
    fn get_playlist_tracks_skips_failed_hash_and_returns_rest() {
        let mut port = FakeLibraryPort::new();
        let pname = make_playlist_name("mixed");
        port.playlist_tracks.insert(
            "mixed".to_string(),
            vec![
                "sha256:good".to_string(),
                "sha256:bad".to_string(),
                "sha256:also-good".to_string(),
            ],
        );
        port.tracks.insert(
            "sha256:good".to_string(),
            Ok(make_track("sha256:good", "Good Track")),
        );
        port.tracks
            .insert("sha256:bad".to_string(), Err("not found".to_string()));
        port.tracks.insert(
            "sha256:also-good".to_string(),
            Ok(make_track("sha256:also-good", "Also Good")),
        );

        let evt = handle_request(&port, IpcRequest::GetPlaylistTracks(pname));

        match evt {
            IpcEvent::PlaylistTracks { name: _, tracks } => {
                // The failed hash is skipped; two successful tracks returned.
                assert_eq!(tracks.len(), 2);
                assert_eq!(tracks[0].title.as_deref(), Some("Good Track"));
                assert_eq!(tracks[1].title.as_deref(), Some("Also Good"));
            }
            other => panic!("Expected PlaylistTracks, got {:?}", other),
        }
    }

    #[test]
    fn get_playlist_tracks_playlist_not_found_maps_to_request_failed() {
        let port = FakeLibraryPort::new(); // empty — no playlists registered

        let evt = handle_request(
            &port,
            IpcRequest::GetPlaylistTracks(make_playlist_name("nonexistent")),
        );

        match evt {
            IpcEvent::RequestFailed { context, .. } => {
                assert!(context.contains("GetPlaylistTracks"));
            }
            other => panic!("Expected RequestFailed, got {:?}", other),
        }
    }

    #[test]
    fn search_returns_search_results_event() {
        let mut port = FakeLibraryPort::new();
        port.search_result = Ok(vec![
            make_track("sha256:r1", "Result One"),
            make_track("sha256:r2", "Result Two"),
        ]);

        let evt = handle_request(&port, IpcRequest::Search(Box::new(TrackQuery::default())));

        match evt {
            IpcEvent::SearchResults(tracks) => {
                assert_eq!(tracks.len(), 2);
                assert_eq!(tracks[0].title.as_deref(), Some("Result One"));
            }
            other => panic!("Expected SearchResults, got {:?}", other),
        }
    }

    #[test]
    fn search_error_maps_to_request_failed() {
        let mut port = FakeLibraryPort::new();
        port.search_result = Err("search blew up".to_string());

        let evt = handle_request(&port, IpcRequest::Search(Box::new(TrackQuery::default())));

        match evt {
            IpcEvent::RequestFailed { context, message } => {
                assert_eq!(context, "Search");
                assert!(message.contains("search blew up"));
            }
            other => panic!("Expected RequestFailed, got {:?}", other),
        }
    }

    // ------------------------------------------------------------------
    // Channel round-trip test (no real backend)
    // ------------------------------------------------------------------

    #[test]
    fn channel_roundtrip_list_playlists_via_handle_request() {
        // Simulate the channel protocol without spawning a thread.
        let (req_tx, req_rx) = mpsc::channel::<IpcRequest>();
        let (evt_tx, evt_rx) = mpsc::channel::<IpcEvent>();

        let mut port = FakeLibraryPort::new();
        port.playlists = vec![make_playlist_name("techno")];

        // Send a request.
        req_tx.send(IpcRequest::ListPlaylists).unwrap();
        drop(req_tx); // signal end of stream

        // Process all requests (as the worker would).
        for req in req_rx {
            evt_tx.send(handle_request(&port, req)).unwrap();
        }
        drop(evt_tx);

        // Receive and verify event.
        let event = evt_rx.recv().expect("Expected an event");
        match event {
            IpcEvent::Playlists(names) => {
                assert_eq!(names.len(), 1);
                assert_eq!(names[0].as_str(), "techno");
            }
            other => panic!("Expected Playlists, got {:?}", other),
        }

        // No more events.
        assert!(evt_rx.recv().is_err(), "Expected channel to be closed");
    }
}
