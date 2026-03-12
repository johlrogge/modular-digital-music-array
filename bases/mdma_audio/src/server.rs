use library_ipc_client::LibraryClient;
use music_primitives::ContentHash;
use playback_engine::PlaybackEngine;
use playback_primitives::AudioSinkInfo;
use std::path::PathBuf;
use stream_source_protocol::{
    AudioOutputConfig, StreamCommand, StreamPlaybackState, StreamResponse, StreamTrackInfo,
};
use tracing::{info, warn};

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

struct LoadedTrack {
    content_hash: ContentHash,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    is_playing: bool,
}

pub struct Server {
    engine: PlaybackEngine,
    socket: nng::Socket,
    library_socket: String,
    music_dir: PathBuf,
    loaded: Option<LoadedTrack>,
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
            loaded: None,
        }
    }

    pub async fn run(&mut self) -> color_eyre::Result<()> {
        info!("mdma-audio server starting...");
        loop {
            let msg = self.socket.recv()?;
            let command: StreamCommand = serde_json::from_slice(&msg)?;
            info!("Received command: {:?}", command);
            let response = self.handle_command(command).await;
            info!("Sending response: {:?}", response);
            let bytes = serde_json::to_vec(&response)?;
            self.socket
                .send(bytes.as_slice())
                .map_err(|(_, e)| color_eyre::eyre::eyre!("nng send error: {}", e))?;
        }
    }

    async fn handle_command(&mut self, command: StreamCommand) -> StreamResponse {
        match command {
            StreamCommand::Load { content_hash } => self.handle_load(content_hash).await,
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

    async fn handle_load(&mut self, content_hash: ContentHash) -> StreamResponse {
        let client = match LibraryClient::connect(&self.library_socket) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to connect to library: {}", e);
                return StreamResponse::Error {
                    message: format!("Failed to connect to library: {}", e),
                };
            }
        };

        let track_info = match client.get_track(&content_hash) {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to get track {}: {}", content_hash.as_str(), e);
                return StreamResponse::Error {
                    message: format!("Track not found: {}", e),
                };
            }
        };

        let blob_path = match &track_info.blob_path {
            Some(p) => p.clone(),
            None => {
                return StreamResponse::Error {
                    message: "Track has no blob path".to_string(),
                };
            }
        };

        let full_path = self.music_dir.join(&blob_path);
        info!("Loading track from {:?}", full_path);

        if let Err(e) = self.engine.load_track(&full_path).await {
            warn!("Failed to load track: {}", e);
            return StreamResponse::Error {
                message: format!("Failed to load track: {}", e),
            };
        }

        self.loaded = Some(LoadedTrack {
            content_hash,
            title: track_info.title,
            artist: track_info.artist,
            album: track_info.album,
            is_playing: false,
        });

        StreamResponse::Ok
    }

    fn handle_play(&mut self) -> StreamResponse {
        let loaded = match &mut self.loaded {
            Some(l) => l,
            None => {
                return StreamResponse::Error {
                    message: "nothing loaded".to_string(),
                };
            }
        };

        self.engine.set_stream_active(true);
        if let Err(e) = self.engine.play() {
            warn!("Play failed: {}", e);
            return StreamResponse::Error {
                message: format!("Play failed: {}", e),
            };
        }

        loaded.is_playing = true;
        StreamResponse::Ok
    }

    fn handle_pause(&mut self) -> StreamResponse {
        let loaded = match &mut self.loaded {
            Some(l) => l,
            None => return StreamResponse::Ok,
        };

        if let Err(e) = self.engine.stop() {
            warn!("Pause (stop) failed: {}", e);
        }
        loaded.is_playing = false;
        StreamResponse::Ok
    }

    fn handle_stop(&mut self) -> StreamResponse {
        if self.loaded.is_some() {
            if let Err(e) = self.engine.stop() {
                warn!("Stop failed: {}", e);
            }
            if let Err(e) = self.engine.unload_track() {
                warn!("Unload failed: {}", e);
            }
            self.engine.set_stream_active(false);
            self.loaded = None;
        }
        StreamResponse::Ok
    }

    fn handle_loaded(&mut self) -> StreamResponse {
        let loaded = match &self.loaded {
            Some(l) => l,
            None => return StreamResponse::Loaded { info: None },
        };

        let state = if self.engine.is_track_finished() {
            StreamPlaybackState::Finished
        } else if loaded.is_playing {
            StreamPlaybackState::Playing
        } else {
            StreamPlaybackState::Paused
        };

        StreamResponse::Loaded {
            info: Some(StreamTrackInfo {
                state,
                content_hash: Some(loaded.content_hash.clone()),
                title: loaded.title.clone(),
                artist: loaded.artist.clone(),
                album: loaded.album.clone(),
                position_ms: self.engine.position_ms(),
                duration_ms: self.engine.duration_ms(),
            }),
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
}
