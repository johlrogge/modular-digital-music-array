use crate::error::ServerError;
use crate::playback_state::{PlaybackEffect, PlaybackState, PlaybackStateMachine};
use acid_client::AcidClient;
use color_eyre::Result;
use event_protocol::{to_topic_message, PlaybackEvent};
use media_protocol::{Command, Response, ResponseData};
use music_facts::{FactOrigin, FactSource, MusicValue};
use nng::Socket;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use stream_source_protocol::StreamPlaybackState;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::stream_client::StreamClient;

struct QueueEntry {
    hash: media_protocol::ContentHash,
    path: PathBuf,
}

fn write_fact(acid_client: &AcidClient, hash: &media_protocol::ContentHash, value: MusicValue) {
    let source = FactSource::new(
        "mdma-playback",
        env!("CARGO_PKG_VERSION"),
        FactOrigin::Unknown,
    );
    if let Err(e) = acid_client.write_music_facts(hash, &[(value, source)]) {
        warn!("Failed to write fact via ACID: {}", e);
    }
}

/// Serializable form of a queue entry for persistence.
#[derive(Serialize, Deserialize)]
struct PersistEntry {
    hash: String,
    path: String,
}

pub struct Server {
    audio: Arc<Mutex<StreamClient>>,
    socket: Socket,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    /// State machine that owns the canonical playback state (replaces the old
    /// `current_hash` and `is_paused` boolean flags).
    state: Arc<Mutex<PlaybackStateMachine>>,
    queue_file: PathBuf,
    event_pub: Socket,
    acid_client: Arc<AcidClient>,
}

impl Server {
    pub fn new(
        audio: Arc<Mutex<StreamClient>>,
        socket: Socket,
        queue_file: PathBuf,
        event_pub: Socket,
        acid_client: Arc<AcidClient>,
    ) -> Self {
        Self {
            audio,
            socket,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            state: Arc::new(Mutex::new(PlaybackStateMachine::new())),
            queue_file,
            event_pub,
            acid_client,
        }
    }

    fn publish_event(&self, event: &PlaybackEvent) {
        publish_event_on_socket(&self.event_pub, event);
    }

    /// Serialize and write the queue to disk atomically. Logs a warning on error.
    fn persist_queue(&self, queue: &VecDeque<QueueEntry>) {
        persist_queue_to_file(&self.queue_file, queue);
    }

    /// Load the queue from disk. Returns an empty queue on first start or corruption.
    async fn load_queue(&self) -> VecDeque<QueueEntry> {
        let data = match std::fs::read(&self.queue_file) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return VecDeque::new();
            }
            Err(e) => {
                warn!("Failed to read queue file {:?}: {}", self.queue_file, e);
                return VecDeque::new();
            }
        };

        let entries: Vec<PersistEntry> = match serde_json::from_slice(&data) {
            Ok(e) => e,
            Err(e) => {
                warn!("Queue file corrupt ({}), discarding", e);
                let _ = std::fs::remove_file(&self.queue_file);
                return VecDeque::new();
            }
        };

        let mut queue = VecDeque::new();
        for e in entries {
            let path = PathBuf::from(&e.path);
            if path.exists() {
                queue.push_back(QueueEntry {
                    hash: media_protocol::ContentHash::new(e.hash),
                    path,
                });
            } else {
                warn!("Skipping missing queue entry: {}", e.path);
            }
        }
        info!("Restored {} track(s) from queue file", queue.len());
        queue
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        info!("Playback server starting...");

        // Restore queue from disk.
        let restored = self.load_queue().await;
        *self.queue.lock().await = restored;

        // Background task: auto-advance to next queued track when current track finishes.
        tokio::spawn(auto_advance_task(
            self.audio.clone(),
            self.queue.clone(),
            self.state.clone(),
            self.event_pub.clone(),
            self.acid_client.clone(),
            self.queue_file.clone(),
        ));

        loop {
            // Receive command
            let msg = self.socket.recv().map_err(ServerError::from)?;
            let command: Command = serde_json::from_slice(&msg)?;

            info!("Received command: {:?}", command);

            // Process command
            let response = self.handle_command(command).await;

            info!("Handled command, response {:?}", response);
            // Send response
            let response_data = serde_json::to_vec(&response)?;
            self.socket
                .send(&response_data)
                .map_err(ServerError::from)?;
            info!("sent response");
        }
    }

    async fn handle_command(&self, command: Command) -> Response {
        match command {
            // ---------------------------------------------------------------
            // Deprecated low-level deck commands — not supported in split-architecture mode.
            // ---------------------------------------------------------------
            Command::LoadTrack { .. }
            | Command::SetVolume { .. }
            | Command::Unload { .. }
            | Command::Seek { .. } => Response::Err {
                message: "This command is not supported in split-architecture mode".into(),
            },
            Command::Play { deck: _ } => {
                info!("Play command");
                let effects = {
                    let mut sm = self.state.lock().await;
                    match sm.state() {
                        PlaybackState::Paused { .. } => sm.resume(),
                        PlaybackState::Playing { .. } => vec![],
                        PlaybackState::Idle => {
                            // No track loaded via state machine — treat as no-op
                            warn!("Play command received while Idle and no queue entry");
                            vec![]
                        }
                    }
                };
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            // ---------------------------------------------------------------
            // State-machine-backed commands
            // ---------------------------------------------------------------
            Command::Stop { deck: _ } => {
                info!("Stopping playback");
                let effects = self.state.lock().await.stop();
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::Pause { deck: _ } => {
                info!("Pausing playback");
                let effects = self.state.lock().await.pause();
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::Resume { deck: _ } => {
                info!("Resuming playback");
                let effects = self.state.lock().await.resume();
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::GetLength { deck: _ } => {
                info!("Getting length");
                match self.audio.lock().await.loaded() {
                    Ok(Some(info)) => Response::Ok {
                        data: info.duration_ms.map(|d| ResponseData::Length(d as usize)),
                    },
                    Ok(None) => Response::Ok { data: None },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Command::QueueNext { hash, path } => {
                info!("Queue next: {:?}", path);
                let mut queue = self.queue.lock().await;
                queue.push_front(QueueEntry { hash, path });
                self.persist_queue(&queue);
                self.publish_event(&PlaybackEvent::QueueChanged {
                    length: queue.len(),
                });
                self.ok_response()
            }
            Command::QueueAppend { hash, path } => {
                info!("Queue append: {:?}", path);
                let mut queue = self.queue.lock().await;
                queue.push_back(QueueEntry { hash, path });
                self.persist_queue(&queue);
                self.publish_event(&PlaybackEvent::QueueChanged {
                    length: queue.len(),
                });
                self.ok_response()
            }
            Command::QueueList => {
                let hashes: Vec<media_protocol::ContentHash> = self
                    .queue
                    .lock()
                    .await
                    .iter()
                    .map(|e| e.hash.clone())
                    .collect();
                info!("Queue list: {} items", hashes.len());
                Response::Ok {
                    data: Some(ResponseData::Queue(hashes)),
                }
            }
            Command::QueueClear => {
                let mut queue = self.queue.lock().await;
                queue.clear();
                self.persist_queue(&queue);
                info!("Queue cleared");
                self.publish_event(&PlaybackEvent::QueueChanged { length: 0 });
                self.ok_response()
            }
            Command::QueueRemove { hashes } => {
                let mut queue = self.queue.lock().await;
                let before = queue.len();
                queue.retain(|e| !hashes.contains(&e.hash));
                let removed = before - queue.len();
                self.persist_queue(&queue);
                info!("Removed {}/{} hash(es) from queue", removed, hashes.len());
                self.publish_event(&PlaybackEvent::QueueChanged {
                    length: queue.len(),
                });
                Response::Ok {
                    data: Some(ResponseData::Count(removed)),
                }
            }
            Command::QueueReplace { entries } => {
                let mut queue = self.queue.lock().await;
                queue.clear();
                for (hash, path) in entries {
                    queue.push_back(QueueEntry { hash, path });
                }
                let n = queue.len();
                self.persist_queue(&queue);
                info!("Queue replaced with {} entries", n);
                self.publish_event(&PlaybackEvent::QueueChanged { length: n });
                self.ok_response()
            }
            Command::PlayQueue => {
                info!("Play from queue");
                let entry = {
                    let mut queue = self.queue.lock().await;
                    let e = queue.pop_front();
                    if e.is_some() {
                        self.persist_queue(&queue);
                        self.publish_event(&PlaybackEvent::QueueChanged {
                            length: queue.len(),
                        });
                    }
                    e
                };
                match entry {
                    None => Response::Err {
                        message: "Queue is empty".to_string(),
                    },
                    Some(e) => {
                        let effects = self
                            .state
                            .lock()
                            .await
                            .play_queue(e.hash.clone(), e.path.clone());
                        let result = execute_effects(
                            effects,
                            &self.audio,
                            &self.event_pub,
                            &self.acid_client,
                        )
                        .await;
                        self.create_response(result, None)
                    }
                }
            }
            Command::NowPlaying => {
                let hash = self.state.lock().await.current_hash().cloned();
                info!("Now playing: {:?}", hash);
                Response::Ok {
                    data: Some(ResponseData::NowPlaying(hash)),
                }
            }
            Command::Skip => {
                info!("Skip: stopping current track and advancing queue");
                let next = {
                    let mut queue = self.queue.lock().await;
                    let e = queue.pop_front();
                    if e.is_some() {
                        self.persist_queue(&queue);
                        self.publish_event(&PlaybackEvent::QueueChanged {
                            length: queue.len(),
                        });
                    }
                    e.map(|entry| (entry.hash, entry.path))
                };
                let effects = self.state.lock().await.skip(next);
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::GetSession => {
                let session_id = self
                    .state
                    .lock()
                    .await
                    .session_id()
                    .map(|id| id.as_str().to_owned());
                info!("GetSession: {:?}", session_id);
                Response::Ok {
                    data: Some(ResponseData::Session(session_id)),
                }
            }
            Command::ListAudioOutputs => {
                info!("Listing audio outputs");
                match self.audio.lock().await.list_outputs() {
                    Ok(sinks) => Response::Ok {
                        data: Some(ResponseData::AudioOutputs(sinks)),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Command::SetAudioOutput { device_name } => {
                info!("Setting audio output to {:?}", device_name);
                match self.audio.lock().await.set_output(device_name) {
                    Ok(config) => Response::Ok {
                        data: Some(ResponseData::AudioOutput(config)),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Command::GetAudioOutput => {
                info!("Getting current audio output");
                match self.audio.lock().await.get_output() {
                    Ok(config) => Response::Ok {
                        data: Some(ResponseData::AudioOutput(config)),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
        }
    }

    fn ok_response(&self) -> Response {
        Response::Ok { data: None }
    }

    fn create_response(
        &self,
        result: Result<(), ServerError>,
        data: Option<ResponseData>,
    ) -> Response {
        match result {
            Ok(()) => {
                info!("Command completed successfully");
                Response::Ok { data }
            }
            Err(e) => {
                warn!("Command failed: {}", e);
                Response::Err {
                    message: e.to_string(),
                }
            }
        }
    }
}

/// Execute a list of effects produced by the state machine.
async fn execute_effects(
    effects: Vec<PlaybackEffect>,
    audio: &Arc<Mutex<StreamClient>>,
    event_pub: &nng::Socket,
    acid_client: &AcidClient,
) -> Result<(), ServerError> {
    for effect in effects {
        match effect {
            PlaybackEffect::StopEngine => {
                let guard = audio.lock().await;
                if let Err(e) = guard.stop() {
                    warn!("Failed to stop audio: {e}");
                }
            }
            PlaybackEffect::PauseEngine => {
                if let Err(e) = audio.lock().await.pause() {
                    warn!("Failed to pause audio: {e}");
                }
            }
            PlaybackEffect::PlayEngine => {
                let guard = audio.lock().await;
                if let Err(e) = guard.play() {
                    warn!("Failed to play audio: {e}");
                }
            }
            PlaybackEffect::LoadAndPlay { hash, path: _ } => {
                let client = audio.lock().await;
                if let Err(e) = client.load(hash.clone()) {
                    warn!("Failed to load {hash}: {e}");
                } else if let Err(e) = client.play() {
                    warn!("Failed to play after load: {e}");
                }
            }
            PlaybackEffect::EmitEvent(event) => {
                publish_event_on_socket(event_pub, &event);
            }
            PlaybackEffect::WriteFact { hash, value } => {
                write_fact(acid_client, &hash, value);
            }
        }
    }
    Ok(())
}

fn publish_event_on_socket(socket: &nng::Socket, event: &PlaybackEvent) {
    let msg = to_topic_message(event);
    if let Err(e) = socket.send(&msg) {
        warn!("Failed to publish event: {:?}", e);
    }
}

/// Persist queue to disk (free function for use in auto_advance_task).
fn persist_queue_to_file(queue_file: &Path, queue: &VecDeque<QueueEntry>) {
    let entries: Vec<PersistEntry> = queue
        .iter()
        .map(|e| PersistEntry {
            hash: e.hash.as_str().to_owned(),
            path: e.path.to_string_lossy().into_owned(),
        })
        .collect();

    let json = match serde_json::to_string_pretty(&entries) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize queue: {}", e);
            return;
        }
    };

    let tmp = queue_file.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &json) {
        warn!("Failed to write queue temp file {:?}: {}", tmp, e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, queue_file) {
        warn!("Failed to rename queue file: {}", e);
    }
}

/// Polls the audio source every 200 ms. When the track reaches `Finished`, pops the next
/// entry from the queue and starts playing it automatically.
/// Auto-advance is suppressed while playback is paused (state machine handles this).
/// Every 5th poll iteration (~1 s) broadcasts a `PositionUpdate` event while playing.
async fn auto_advance_task(
    audio: Arc<Mutex<StreamClient>>,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    state: Arc<Mutex<PlaybackStateMachine>>,
    event_pub: nng::Socket,
    acid_client: Arc<AcidClient>,
    queue_file: PathBuf,
) {
    let mut tick: u8 = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        tick = tick.wrapping_add(1);

        // Every 5th tick (~1 s) broadcast a position update while playing.
        if tick.is_multiple_of(5) {
            let current_state = state.lock().await;
            if let PlaybackState::Playing { hash } = current_state.state() {
                let h = hash.clone();
                drop(current_state);
                if let Ok(Some(ref info)) = audio.lock().await.loaded() {
                    if let (Some(position_ms), Some(duration_ms)) =
                        (info.position_ms, info.duration_ms)
                    {
                        publish_event_on_socket(
                            &event_pub,
                            &PlaybackEvent::PositionUpdate {
                                hash: h.clone(),
                                position_ms,
                                duration_ms,
                            },
                        );
                    }
                }
            }
        }

        // Check whether the current state is Playing before checking audio source.
        // If Paused or Idle, do not auto-advance.
        {
            let current_state = state.lock().await;
            match current_state.state() {
                PlaybackState::Playing { .. } => {}
                PlaybackState::Paused { .. } => {
                    continue;
                }
                PlaybackState::Idle => {
                    continue;
                }
            }
        }

        let finished = match audio.lock().await.loaded() {
            Ok(Some(ref info)) => matches!(info.state, StreamPlaybackState::Finished),
            Ok(None) => false,
            Err(e) => {
                warn!("Failed to poll audio source: {e}");
                false
            }
        };
        if !finished {
            continue;
        }

        // Track ended — pop next from queue.
        let next = {
            let mut q = queue.lock().await;
            let entry = q.pop_front();
            if entry.is_some() {
                persist_queue_to_file(&queue_file, &q);
                publish_event_on_socket(
                    &event_pub,
                    &PlaybackEvent::QueueChanged { length: q.len() },
                );
            }
            entry.map(|e| (e.hash, e.path))
        };

        // Drive the state machine.
        let effects = state.lock().await.track_ended(next);

        if let Err(e) = execute_effects(effects, &audio, &event_pub, &acid_client).await {
            warn!("Auto-advance failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_client::AcidClient;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_server() -> Server {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let event_pub = nng::Socket::new(nng::Protocol::Pub0).unwrap();
        // Create a dummy ACID listener so the client can connect.
        let acid_listen = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let acid_addr = format!("ipc:///tmp/test_acid_{}_{}.sock", std::process::id(), id);
        acid_listen.listen(&acid_addr).unwrap();
        let acid_client = Arc::new(AcidClient::connect(&acid_addr).unwrap());
        std::mem::forget(acid_listen);

        // Create a dummy audio source (Req0 side of a Rep0/Req0 pair).
        // The StreamClient connects to a stub that we never actually read from in these
        // state-machine-only tests, so we just create a socket that listens but is never used.
        let audio_stub = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let audio_addr = format!("ipc:///tmp/test_audio_{}_{}.sock", std::process::id(), id);
        audio_stub.listen(&audio_addr).unwrap();
        std::mem::forget(audio_stub);

        let audio_client = StreamClient::connect(&audio_addr).unwrap();
        let audio = Arc::new(tokio::sync::Mutex::new(audio_client));

        Server::new(
            audio,
            socket,
            PathBuf::from("/tmp/test_queue.json"),
            event_pub,
            acid_client,
        )
    }

    /// After pause -> skip, the state machine must be in Idle (not Paused).
    /// This is the regression test for the original bug where Skip didn't clear
    /// `is_paused`.
    #[tokio::test]
    async fn skip_after_pause_state_is_not_paused() {
        let server = make_server();

        // Drive the state machine directly into Paused.
        {
            let mut sm = server.state.lock().await;
            let hash = media_protocol::ContentHash::new("sha256:test");
            let path = PathBuf::from("/nonexistent/track.flac");
            sm.play_queue(hash, path);
            sm.pause();
        }

        assert!(
            matches!(
                server.state.lock().await.state(),
                PlaybackState::Paused { .. }
            ),
            "Expected Paused state before Skip"
        );

        // Issue Skip directly on the state machine.
        // We skip effects execution entirely — the audio stub doesn't respond,
        // and we only care about the state transition here.
        let _effects = server.state.lock().await.skip(None);

        assert!(
            matches!(server.state.lock().await.state(), PlaybackState::Idle),
            "Expected Idle after Skip from Paused"
        );
    }

    /// Verify state machine transitions work for stop/resume without involving audio.
    #[tokio::test]
    async fn stop_from_idle_is_noop_at_server_level() {
        let server = make_server();
        // State starts Idle; stop from Idle should produce no effects and remain Idle.
        let effects = server.state.lock().await.stop();
        assert!(
            effects.is_empty(),
            "Stop from Idle should produce no effects"
        );
        assert!(
            matches!(server.state.lock().await.state(), PlaybackState::Idle),
            "Expected Idle after Stop from Idle"
        );
    }
}
