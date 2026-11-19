use std::sync::{
    mpsc::{self, Receiver, Sender},
    Mutex,
};

use bevy::prelude::*;
use mdma_client::{
    entry_tracks, ContentHash, EntryError, EntryTrackError, LibraryBackend, LibraryClientError,
    PlaylistEntry, PlaylistName, ProtocolError, TrackInfo, TrackQuery,
};

use crate::config::DjWorkspaceConfig;
use crate::playlists::PlaylistRow;

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
        rows: Vec<PlaylistRow>,
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
    fn playlist_entries(&self, name: &PlaylistName) -> Result<Vec<PlaylistEntry>, String>;
    /// The typed error, so "track not found" can be told from other failures.
    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, LibraryClientError>;
    fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, String>;
}

impl LibraryPort for LibraryBackend {
    fn playlist_list(&self) -> Result<Vec<PlaylistName>, String> {
        self.playlist_list().map_err(|e| e.to_string())
    }

    fn playlist_entries(&self, name: &PlaylistName) -> Result<Vec<PlaylistEntry>, String> {
        self.playlist_entries(name).map_err(|e| e.to_string())
    }

    fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, LibraryClientError> {
        self.get_track(hash)
    }

    fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, String> {
        self.search(query).map_err(|e| e.to_string())
    }
}

// =========================================================================
// Pure request→event mapping
// =========================================================================

/// Turn entries into rows, keeping every entry at its position.
///
/// An entry the library could not resolve becomes an unavailable row, and so
/// does one whose track lookup answers "track not found". Any other failed
/// lookup fails the whole load: it is not an answer about the entry, so it
/// must not be shown as one.
fn playlist_rows(
    entries: &[PlaylistEntry],
    port: &impl LibraryPort,
) -> Result<Vec<PlaylistRow>, String> {
    entry_tracks(entries, |hash| port.get_track(hash))
        .map(|outcome| match outcome {
            Ok(track) => Ok(PlaylistRow::Track(track)),
            Err(EntryTrackError::Unresolved(error)) => {
                Ok(PlaylistRow::Unavailable(error.to_string()))
            }
            Err(EntryTrackError::Lookup {
                hash,
                source: LibraryClientError::Protocol(ProtocolError::TrackNotFound { .. }),
            }) => Ok(PlaylistRow::Unavailable(
                EntryError::Unknown {
                    hash: hash.as_content_hash().clone(),
                }
                .to_string(),
            )),
            Err(error @ EntryTrackError::Lookup { .. }) => Err(error.to_string()),
        })
        .collect()
}

/// Map a single IpcRequest to an IpcEvent using the given port.
///
/// GetPlaylistTracks lists the playlist's entries and looks up each resolved
/// one. Unresolved entries stay in place as unavailable rows; a failed lookup
/// is a RequestFailed.
pub(crate) fn handle_request(port: &impl LibraryPort, req: IpcRequest) -> IpcEvent {
    match req {
        IpcRequest::ListPlaylists => match port.playlist_list() {
            Ok(names) => IpcEvent::Playlists(names),
            Err(e) => IpcEvent::RequestFailed {
                context: "ListPlaylists".to_string(),
                message: e,
            },
        },

        IpcRequest::GetPlaylistTracks(name) => match port.playlist_entries(&name) {
            Err(e) => IpcEvent::RequestFailed {
                context: format!("GetPlaylistTracks({})", name.as_str()),
                message: e,
            },
            Ok(entries) => match playlist_rows(&entries, port) {
                Ok(rows) => IpcEvent::PlaylistTracks { name, rows },
                Err(message) => IpcEvent::RequestFailed {
                    context: format!("GetPlaylistTracks({})", name.as_str()),
                    message,
                },
            },
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
            Ok(IpcEvent::PlaylistTracks { name, rows }) => {
                // Ignore stale responses from a previously-selected playlist.
                let current_name = playlist_state
                    .selected
                    .and_then(|idx| playlist_state.names.get(idx));
                if current_name.map(|n| n == &name).unwrap_or(false) {
                    playlist_state.rows = rows;
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
        /// playlist name -> its entries, in order
        playlist_entries: HashMap<String, Vec<PlaylistEntry>>,
        /// hash -> Ok(TrackInfo) or Err(message)
        tracks: HashMap<String, Result<TrackInfo, String>>,
        search_result: Result<Vec<TrackInfo>, String>,
    }

    impl FakeLibraryPort {
        fn new() -> Self {
            Self {
                playlists: vec![],
                playlist_entries: HashMap::new(),
                tracks: HashMap::new(),
                search_result: Ok(vec![]),
            }
        }
    }

    fn make_playlist_name(s: &str) -> PlaylistName {
        PlaylistName::new(s).expect("valid playlist name in test")
    }

    /// A full canonical hash made of one repeated hex digit.
    fn full_hash(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn track_entry(c: char) -> PlaylistEntry {
        PlaylistEntry::Track {
            hash: mdma_client::CanonicalHash::try_from(ContentHash::new(full_hash(c)))
                .expect("canonical test hash"),
        }
    }

    fn unknown_entry(token: &str) -> PlaylistEntry {
        PlaylistEntry::Unresolved {
            error: mdma_client::EntryError::Unknown {
                hash: ContentHash::new(token),
            },
        }
    }

    fn make_track(hash: &str, title: &str) -> TrackInfo {
        TrackInfo {
            title: Some(title.to_string()),
            ..TrackInfo::new(ContentHash::new(hash))
        }
    }

    /// What each row shows: a track's title, or the unavailable text.
    fn row_labels(rows: &[PlaylistRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                PlaylistRow::Track(track) => track.title.clone().unwrap_or_default(),
                PlaylistRow::Unavailable(reason) => format!("unavailable: {reason}"),
            })
            .collect()
    }

    impl LibraryPort for FakeLibraryPort {
        fn playlist_list(&self) -> Result<Vec<PlaylistName>, String> {
            Ok(self.playlists.clone())
        }

        fn playlist_entries(&self, name: &PlaylistName) -> Result<Vec<PlaylistEntry>, String> {
            match self.playlist_entries.get(name.as_str()) {
                Some(entries) => Ok(entries.clone()),
                None => Err(format!("Playlist not found: {}", name.as_str())),
            }
        }

        /// A registered `Err(message)` is a library failure; a track that was
        /// never registered is "track not found".
        fn get_track(&self, hash: &ContentHash) -> Result<TrackInfo, LibraryClientError> {
            match self.tracks.get(hash.as_str()) {
                Some(Ok(t)) => Ok(t.clone()),
                Some(Err(message)) => Err(LibraryClientError::Protocol(ProtocolError::Internal {
                    message: message.clone(),
                })),
                None => Err(LibraryClientError::Protocol(ProtocolError::TrackNotFound {
                    hash: hash.as_str().to_string(),
                })),
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
            fn playlist_entries(&self, _: &PlaylistName) -> Result<Vec<PlaylistEntry>, String> {
                unimplemented!()
            }
            fn get_track(&self, _: &ContentHash) -> Result<TrackInfo, LibraryClientError> {
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
        port.playlist_entries.insert(
            "deep-house".to_string(),
            vec![track_entry('a'), track_entry('b')],
        );
        port.tracks
            .insert(full_hash('a'), Ok(make_track(&full_hash('a'), "Track A")));
        port.tracks
            .insert(full_hash('b'), Ok(make_track(&full_hash('b'), "Track B")));

        let evt = handle_request(&port, IpcRequest::GetPlaylistTracks(pname.clone()));

        match evt {
            IpcEvent::PlaylistTracks { name, rows } => {
                assert_eq!(name.as_str(), "deep-house");
                assert_eq!(row_labels(&rows), vec!["Track A", "Track B"]);
            }
            other => panic!("Expected PlaylistTracks, got {:?}", other),
        }
    }

    #[test]
    fn get_playlist_tracks_keeps_unresolved_entries_in_place_as_unavailable_rows() {
        let mut port = FakeLibraryPort::new();
        let pname = make_playlist_name("mixed");
        port.playlist_entries.insert(
            "mixed".to_string(),
            vec![track_entry('a'), unknown_entry("dead"), track_entry('c')],
        );
        port.tracks.insert(
            full_hash('a'),
            Ok(make_track(&full_hash('a'), "Good Track")),
        );
        port.tracks
            .insert(full_hash('c'), Ok(make_track(&full_hash('c'), "Also Good")));

        let evt = handle_request(&port, IpcRequest::GetPlaylistTracks(pname));

        match evt {
            IpcEvent::PlaylistTracks { name: _, rows } => {
                assert_eq!(
                    row_labels(&rows),
                    vec!["Good Track", "unavailable: unknown track dead", "Also Good"]
                );
            }
            other => panic!("Expected PlaylistTracks, got {:?}", other),
        }
    }

    #[test]
    fn get_playlist_tracks_lookup_failure_maps_to_request_failed() {
        let mut port = FakeLibraryPort::new();
        let pname = make_playlist_name("broken");
        port.playlist_entries.insert(
            "broken".to_string(),
            vec![track_entry('a'), track_entry('b')],
        );
        port.tracks.insert(
            full_hash('a'),
            Ok(make_track(&full_hash('a'), "Good Track")),
        );
        port.tracks
            .insert(full_hash('b'), Err("library unreachable".to_string()));

        let evt = handle_request(&port, IpcRequest::GetPlaylistTracks(pname));

        match evt {
            IpcEvent::RequestFailed { context, message } => {
                assert_eq!(context, "GetPlaylistTracks(broken)");
                assert!(message.contains("library unreachable"));
            }
            other => panic!("Expected RequestFailed, got {:?}", other),
        }
    }

    #[test]
    fn get_playlist_tracks_keeps_an_entry_the_library_has_no_track_for_as_unavailable() {
        let mut port = FakeLibraryPort::new();
        port.playlist_entries.insert(
            "stale".to_string(),
            vec![track_entry('a'), track_entry('b')],
        );
        // 'a' is never registered, so the library answers "track not found".
        port.tracks.insert(
            full_hash('b'),
            Ok(make_track(&full_hash('b'), "Still Here")),
        );

        let evt = handle_request(
            &port,
            IpcRequest::GetPlaylistTracks(make_playlist_name("stale")),
        );

        match evt {
            IpcEvent::PlaylistTracks { rows, .. } => {
                assert_eq!(
                    row_labels(&rows),
                    vec![
                        format!("unavailable: unknown track {}", full_hash('a')),
                        "Still Here".to_string()
                    ]
                );
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

        let evt = handle_request(&port, IpcRequest::Search(Box::default()));

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

        let evt = handle_request(&port, IpcRequest::Search(Box::default()));

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
