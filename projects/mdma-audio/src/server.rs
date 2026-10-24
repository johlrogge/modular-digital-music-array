use library_ipc_client::LibraryClient;
use music_primitives::ContentHash;
use playback_engine::{PlaybackEngine, TrackId};
use playback_primitives::AudioSinkInfo;
use std::collections::HashMap;
use std::path::PathBuf;
use stream_source_protocol::{
    AudioOutputConfig, StreamCommand, StreamPlaybackState, StreamResponse, StreamTrackInfo,
};
use tracing::{debug, info, warn};

/// Commands that only read state. They are polled frequently (mdma-playback
/// asks `Loaded` every 200 ms), so they are logged at debug, not info.
fn is_read_only(command: &StreamCommand) -> bool {
    matches!(
        command,
        StreamCommand::Loaded
            | StreamCommand::Ping
            | StreamCommand::ListOutputs
            | StreamCommand::GetOutput
    )
}

fn engine_audio_config_to_protocol(c: playback_engine::AudioOutputConfig) -> AudioOutputConfig {
    AudioOutputConfig {
        device_name: c.device_name,
        sample_rate: Some(c.sample_rate),
        channels: None,
    }
}

fn engine_sink_to_protocol(s: playback_engine::AudioSink) -> AudioSinkInfo {
    AudioSinkInfo {
        name: s.name,
        description: Some(s.description),
        max_sample_rate: Some(s.max_sample_rate),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadedState {
    Playing,
    Paused,
}

/// Library metadata for a track known to the engine.
struct LoadedTrack {
    content_hash: ContentHash,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

/// A track resolved through the library: where it lives plus its metadata.
struct ResolvedTrack {
    path: PathBuf,
    meta: LoadedTrack,
}

/// Drop metadata for ids the engine no longer owns. The engine is the single
/// owner of track lifetime: a track can be live while neither audible nor
/// reported as next (around a commit), so liveness is the only safe criterion.
fn prune_tracks(tracks: &mut HashMap<TrackId, LoadedTrack>, is_live: impl Fn(TrackId) -> bool) {
    tracks.retain(|id, _| is_live(*id));
}

/// Build the Loaded response payload describing the audible track and the
/// track that plays next (`preloaded` in the protocol).
fn build_loaded_info(
    tracks: &HashMap<TrackId, LoadedTrack>,
    audible: Option<TrackId>,
    preloaded: Option<TrackId>,
    play_state: LoadedState,
    finished: bool,
    position_ms: Option<u64>,
    duration_ms: Option<u64>,
) -> Option<StreamTrackInfo> {
    let current = tracks.get(&audible?)?;
    let state = if finished {
        StreamPlaybackState::Finished
    } else if play_state == LoadedState::Playing {
        StreamPlaybackState::Playing
    } else {
        StreamPlaybackState::Paused
    };
    Some(StreamTrackInfo {
        state,
        content_hash: Some(current.content_hash.clone()),
        title: current.title.clone(),
        artist: current.artist.clone(),
        album: current.album.clone(),
        position_ms,
        duration_ms,
        preloaded: preloaded
            .and_then(|id| tracks.get(&id))
            .map(|t| t.content_hash.clone()),
    })
}

pub struct Server {
    engine: PlaybackEngine,
    socket: nng::Socket,
    library_socket: String,
    music_dir: PathBuf,
    tracks: HashMap<TrackId, LoadedTrack>,
    play_state: LoadedState,
}

impl Server {
    pub fn new(
        engine: PlaybackEngine,
        socket: nng::Socket,
        library_socket: String,
        music_dir: PathBuf,
    ) -> Self {
        Self {
            engine,
            socket,
            library_socket,
            music_dir,
            tracks: HashMap::new(),
            play_state: LoadedState::Paused,
        }
    }

    /// Drop metadata for tracks the engine no longer owns.
    fn prune(&mut self) {
        let engine = &self.engine;
        prune_tracks(&mut self.tracks, |id| engine.is_live(id));
    }

    pub async fn run(&mut self) -> color_eyre::Result<()> {
        info!("mdma-audio server starting...");
        loop {
            let msg = self.socket.recv()?;
            let response = self.handle_message(&msg).await;
            let bytes = serde_json::to_vec(&response)?;
            self.socket
                .send(bytes.as_slice())
                .map_err(|(_, e)| color_eyre::eyre::eyre!("nng send error: {}", e))?;
        }
    }

    /// Parse one request and answer it. A message that is not a valid
    /// command (unknown command, bad JSON) gets an `Error` response; it must
    /// never end the serve loop.
    async fn handle_message(&mut self, msg: &[u8]) -> StreamResponse {
        match serde_json::from_slice::<StreamCommand>(msg) {
            Ok(command) => {
                // Read-only queries are polled several times a second; keep
                // them out of the info log so it does not rotate away history.
                let read_only = is_read_only(&command);
                if read_only {
                    debug!("Received command: {:?}", command);
                } else {
                    info!("Received command: {:?}", command);
                }
                let response = self.handle_command(command).await;
                if read_only && !matches!(response, StreamResponse::Error { .. }) {
                    debug!("Sending response: {:?}", response);
                } else {
                    info!("Sending response: {:?}", response);
                }
                response
            }
            Err(e) => {
                warn!("Invalid command: {}", e);
                StreamResponse::Error {
                    message: format!("invalid command: {e}"),
                }
            }
        }
    }

    async fn handle_command(&mut self, command: StreamCommand) -> StreamResponse {
        match command {
            StreamCommand::Load { content_hash } => self.handle_load(content_hash).await,
            StreamCommand::Preload { content_hash } => self.handle_preload(content_hash),
            StreamCommand::ClearPreload => self.handle_clear_preload(),
            StreamCommand::Play => self.handle_play(),
            StreamCommand::Pause => self.handle_pause(),
            StreamCommand::Stop => self.handle_stop(),
            StreamCommand::Loaded => self.handle_loaded(),
            StreamCommand::ListOutputs => self.handle_list_outputs(),
            StreamCommand::SetOutput { device_name } => self.handle_set_output(device_name),
            StreamCommand::GetOutput => self.handle_get_output(),
            StreamCommand::Ping => StreamResponse::Pong,
        }
    }

    /// Look up a track in the library and compute its full path.
    fn resolve_path(&self, content_hash: &ContentHash) -> Result<ResolvedTrack, String> {
        let client = LibraryClient::connect(&self.library_socket).map_err(|e| {
            warn!("Failed to connect to library: {}", e);
            format!("Failed to connect to library: {}", e)
        })?;

        let track_info = client.get_track(content_hash).map_err(|e| {
            warn!("Failed to get track {}: {}", content_hash.as_str(), e);
            format!("Track not found: {}", e)
        })?;

        let blob_path = track_info
            .blob_path
            .clone()
            .ok_or_else(|| "Track has no blob path".to_string())?;

        Ok(ResolvedTrack {
            path: self.music_dir.join(blob_path),
            meta: LoadedTrack {
                content_hash: content_hash.clone(),
                title: track_info.title,
                artist: track_info.artist,
                album: track_info.album,
            },
        })
    }

    async fn handle_load(&mut self, content_hash: ContentHash) -> StreamResponse {
        let resolved = match self.resolve_path(&content_hash) {
            Ok(r) => r,
            Err(message) => return StreamResponse::Error { message },
        };
        info!("Loading track from {:?}", resolved.path);

        // If the hash is the preloaded track the engine reuses it (same id).
        if let Err(e) = self.engine.load_track(&resolved.path).await {
            warn!("Failed to load track: {}", e);
            return StreamResponse::Error {
                message: format!("Failed to load track: {}", e),
            };
        }

        self.play_state = LoadedState::Paused;
        self.prune();
        if let Some(id) = self.engine.audible_track_id() {
            self.tracks.insert(id, resolved.meta);
        }

        StreamResponse::Ok
    }

    fn handle_preload(&mut self, content_hash: ContentHash) -> StreamResponse {
        // `next` is the committed (promoted) track if there is one; asking
        // for that same track again is a no-op.
        let already_preloaded = self
            .engine
            .next_track_id()
            .and_then(|id| self.tracks.get(&id))
            .is_some_and(|t| t.content_hash == content_hash);
        if already_preloaded {
            return StreamResponse::Ok;
        }

        let resolved = match self.resolve_path(&content_hash) {
            Ok(r) => r,
            Err(message) => return StreamResponse::Error { message },
        };
        info!("Preloading track from {:?}", resolved.path);

        match self.engine.preload_track(&resolved.path) {
            Ok(id) => {
                self.prune();
                self.tracks.insert(id, resolved.meta);
                StreamResponse::Ok
            }
            Err(e) => {
                warn!("Failed to preload track: {}", e);
                StreamResponse::Error {
                    message: format!("Failed to preload track: {}", e),
                }
            }
        }
    }

    fn handle_clear_preload(&mut self) -> StreamResponse {
        self.engine.clear_preload();
        self.prune();
        StreamResponse::Ok
    }

    fn handle_play(&mut self) -> StreamResponse {
        if self.engine.audible_track_id().is_none() {
            return StreamResponse::Error {
                message: "nothing loaded".to_string(),
            };
        }

        if let Err(e) = self.engine.play() {
            warn!("Play failed: {}", e);
            return StreamResponse::Error {
                message: format!("Play failed: {}", e),
            };
        }

        self.play_state = LoadedState::Playing;
        StreamResponse::Ok
    }

    fn handle_pause(&mut self) -> StreamResponse {
        if self.engine.audible_track_id().is_none() {
            return StreamResponse::Ok;
        }

        if let Err(e) = self.engine.stop() {
            warn!("Pause (stop) failed: {}", e);
        }
        self.play_state = LoadedState::Paused;
        StreamResponse::Ok
    }

    fn handle_stop(&mut self) -> StreamResponse {
        if self.engine.audible_track_id().is_some() {
            if let Err(e) = self.engine.stop() {
                warn!("Stop failed: {}", e);
            }
            if let Err(e) = self.engine.unload_track() {
                warn!("Unload failed: {}", e);
            }
            // The stream stays active: stop/unload pause the engine, whose
            // output callback keeps running (emitting silence) so the next
            // load_track flush is serviced promptly. Unloading also drops
            // any preload.
            self.play_state = LoadedState::Paused;
        }
        self.prune();
        StreamResponse::Ok
    }

    fn handle_loaded(&mut self) -> StreamResponse {
        // Commit observed promotions before reporting.
        self.engine.reconcile();
        self.prune();

        // One observation for every field.
        let status = self.engine.status();
        StreamResponse::Loaded {
            info: build_loaded_info(
                &self.tracks,
                status.audible,
                status.next,
                self.play_state,
                status.finished,
                status.position_ms,
                status.duration_ms,
            ),
        }
    }

    fn handle_list_outputs(&self) -> StreamResponse {
        match self.engine.list_outputs() {
            Ok(sinks) => StreamResponse::Outputs {
                sinks: sinks.into_iter().map(engine_sink_to_protocol).collect(),
            },
            Err(e) => StreamResponse::Error {
                message: e.to_string(),
            },
        }
    }

    fn handle_set_output(&mut self, device_name: String) -> StreamResponse {
        match self.engine.set_output(device_name) {
            Ok(config) => StreamResponse::Output {
                config: engine_audio_config_to_protocol(config),
            },
            Err(e) => StreamResponse::Error {
                message: e.to_string(),
            },
        }
    }

    fn handle_get_output(&self) -> StreamResponse {
        let config = self.engine.get_output().clone();
        StreamResponse::Output {
            config: engine_audio_config_to_protocol(config),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn polling_commands_are_read_only_and_state_changes_are_not() {
        assert!(is_read_only(&StreamCommand::Loaded));
        assert!(is_read_only(&StreamCommand::Ping));
        assert!(!is_read_only(&StreamCommand::Play));
        assert!(!is_read_only(&StreamCommand::Pause));
        assert!(!is_read_only(&StreamCommand::Stop));
        assert!(!is_read_only(&StreamCommand::ClearPreload));
    }

    fn make_server() -> Server {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("audio_config.json");
        // Leak the tempdir so the config file is available for the engine lifetime.
        std::mem::forget(tmp);

        let engine = PlaybackEngine::new(config_path).unwrap();
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        Server::new(
            engine,
            socket,
            format!("ipc:///tmp/test_lib_{}_{}.sock", std::process::id(), id),
            PathBuf::from("/music"),
        )
    }

    /// Ping → Pong serialization roundtrip.
    #[tokio::test]
    async fn ping_returns_pong() {
        let mut server = make_server();
        let response = server.handle_command(StreamCommand::Ping).await;
        assert!(matches!(response, StreamResponse::Pong));
    }

    /// Serialized Ping deserializes and produces a serializable Pong.
    #[test]
    fn ping_pong_serde_roundtrip() {
        // Deserialize command
        let cmd: StreamCommand = serde_json::from_str(r#"{"cmd":"ping"}"#).unwrap();
        assert!(matches!(cmd, StreamCommand::Ping));

        // Serialize response
        let resp = StreamResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"status":"pong"}"#);
    }

    /// Play with nothing loaded returns error.
    #[tokio::test]
    async fn play_nothing_loaded_returns_error() {
        let mut server = make_server();
        let response = server.handle_command(StreamCommand::Play).await;
        assert!(matches!(response, StreamResponse::Error { .. }));
        if let StreamResponse::Error { message } = response {
            assert_eq!(message, "nothing loaded");
        }
    }

    /// Loaded with nothing loaded returns Loaded { info: None }.
    #[tokio::test]
    async fn loaded_when_nothing_loaded_returns_none() {
        let mut server = make_server();
        let response = server.handle_command(StreamCommand::Loaded).await;
        assert!(matches!(response, StreamResponse::Loaded { info: None }));
    }

    /// Stop with nothing loaded is a no-op returning Ok.
    #[tokio::test]
    async fn stop_nothing_loaded_returns_ok() {
        let mut server = make_server();
        let response = server.handle_command(StreamCommand::Stop).await;
        assert!(matches!(response, StreamResponse::Ok));
    }

    /// Pause with nothing loaded is a no-op returning Ok.
    #[tokio::test]
    async fn pause_nothing_loaded_returns_ok() {
        let mut server = make_server();
        let response = server.handle_command(StreamCommand::Pause).await;
        assert!(matches!(response, StreamResponse::Ok));
    }

    fn track(hash: &str) -> LoadedTrack {
        LoadedTrack {
            content_hash: ContentHash::new(hash),
            title: Some(format!("title-{hash}")),
            artist: None,
            album: None,
        }
    }

    fn tracks(entries: &[(TrackId, &str)]) -> HashMap<TrackId, LoadedTrack> {
        entries.iter().map(|(id, h)| (*id, track(h))).collect()
    }

    fn info(
        tracks: &HashMap<TrackId, LoadedTrack>,
        audible: Option<TrackId>,
        preloaded: Option<TrackId>,
        play_state: LoadedState,
        finished: bool,
    ) -> Option<StreamTrackInfo> {
        build_loaded_info(
            tracks,
            audible,
            preloaded,
            play_state,
            finished,
            Some(10),
            Some(100),
        )
    }

    /// Preloading an unknown hash is an error (not the old placeholder).
    #[tokio::test]
    async fn preload_unknown_hash_returns_error() {
        let mut server = make_server();
        let response = server
            .handle_command(StreamCommand::Preload {
                content_hash: ContentHash::new("nope"),
            })
            .await;
        match response {
            StreamResponse::Error { message } => assert_ne!(message, "preload unsupported"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// ClearPreload with nothing preloaded is Ok.
    #[tokio::test]
    async fn clear_preload_returns_ok() {
        let mut server = make_server();
        let response = server.handle_command(StreamCommand::ClearPreload).await;
        assert!(matches!(response, StreamResponse::Ok));
    }

    #[test]
    fn info_none_without_audible_track() {
        let t = tracks(&[(1, "a")]);
        assert!(info(&t, None, None, LoadedState::Paused, false).is_none());
    }

    #[test]
    fn info_reports_playing_and_paused() {
        let t = tracks(&[(1, "a")]);
        let playing = info(&t, Some(1), None, LoadedState::Playing, false).unwrap();
        assert_eq!(playing.state, StreamPlaybackState::Playing);
        let paused = info(&t, Some(1), None, LoadedState::Paused, false).unwrap();
        assert_eq!(paused.state, StreamPlaybackState::Paused);
    }

    #[test]
    fn info_finished_overrides_play_state() {
        let t = tracks(&[(1, "a")]);
        let i = info(&t, Some(1), None, LoadedState::Playing, true).unwrap();
        assert_eq!(i.state, StreamPlaybackState::Finished);
    }

    #[test]
    fn info_reports_audible_and_preloaded_hashes() {
        let t = tracks(&[(1, "a"), (2, "b")]);
        let i = info(&t, Some(1), Some(2), LoadedState::Playing, false).unwrap();
        assert_eq!(i.content_hash, Some(ContentHash::new("a")));
        assert_eq!(i.title.as_deref(), Some("title-a"));
        assert_eq!(i.preloaded, Some(ContentHash::new("b")));
        assert_eq!(i.position_ms, Some(10));
        assert_eq!(i.duration_ms, Some(100));
    }

    #[test]
    fn info_preloaded_none_when_no_preload() {
        let t = tracks(&[(1, "a")]);
        let i = info(&t, Some(1), None, LoadedState::Paused, false).unwrap();
        assert_eq!(i.preloaded, None);
    }

    /// Metadata lives exactly as long as the engine keeps the track alive.
    #[test]
    fn prune_drops_ids_the_engine_no_longer_has() {
        let mut t = tracks(&[(1, "a"), (2, "b"), (3, "c")]);
        prune_tracks(&mut t, |id| id == 2 || id == 3);
        let mut ids: Vec<_> = t.keys().copied().collect();
        ids.sort();
        assert_eq!(ids, vec![2, 3]);
        let i = info(&t, Some(2), Some(3), LoadedState::Playing, false).unwrap();
        assert_eq!(i.content_hash, Some(ContentHash::new("b")));
        assert_eq!(i.preloaded, Some(ContentHash::new("c")));
    }

    #[test]
    fn prune_everything_when_engine_empty() {
        let mut t = tracks(&[(1, "a"), (2, "b")]);
        prune_tracks(&mut t, |_| false);
        assert!(t.is_empty());
    }

    /// B1: during the commit window the engine has promoted B (live, reported
    /// as next) while A is still audible. B's metadata must survive pruning so
    /// that it is still there when B becomes audible.
    #[test]
    fn promoted_track_metadata_survives_until_it_is_audible() {
        let mut t = tracks(&[(1, "a"), (2, "b")]);
        // Engine: audible = A, next = B (promoted), both live.
        prune_tracks(&mut t, |id| id == 1 || id == 2);
        let before = info(&t, Some(1), Some(2), LoadedState::Playing, false).unwrap();
        assert_eq!(before.content_hash, Some(ContentHash::new("a")));
        assert_eq!(before.preloaded, Some(ContentHash::new("b")));
        // Boundary consumed: audible = B, A no longer live.
        prune_tracks(&mut t, |id| id == 2);
        let after = info(&t, Some(2), None, LoadedState::Playing, false).unwrap();
        assert_eq!(after.content_hash, Some(ContentHash::new("b")));
        assert_eq!(after.preloaded, None);
    }

    /// M3: an unparseable message gets an Error response and the server
    /// keeps serving.
    #[tokio::test]
    async fn unknown_command_gets_error_response_and_server_continues() {
        let mut server = make_server();
        let response = server.handle_message(br#"{"cmd":"bogus"}"#).await;
        assert!(matches!(response, StreamResponse::Error { .. }));
        let response = server.handle_message(b"not json at all").await;
        assert!(matches!(response, StreamResponse::Error { .. }));
        let response = server.handle_message(br#"{"cmd":"ping"}"#).await;
        assert!(matches!(response, StreamResponse::Pong));
    }
}
