use crate::error::ServerError;
use crate::playback_state::{PlaybackEffect, PlaybackState, PlaybackStateMachine};
use acid_client::AcidClient;
use color_eyre::Result;
use event_protocol::{to_topic_message, PlaybackEvent};
use media_protocol::{Command, ContentHash, Response, ResponseData};
use music_facts::{FactOrigin, FactSource, MusicValue, StartReason};
use nng::Socket;
use playback_engine::{Deck, PlaybackEngine, PlaybackError};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};

struct QueueEntry {
    hash: ContentHash,
    path: PathBuf,
}

fn write_fact(acid_client: &AcidClient, hash: &ContentHash, value: MusicValue) {
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
    engine: Arc<Mutex<PlaybackEngine>>,
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
        engine: Arc<Mutex<PlaybackEngine>>,
        socket: Socket,
        queue_file: PathBuf,
        event_pub: Socket,
        acid_client: Arc<AcidClient>,
    ) -> Self {
        Self {
            engine,
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
                    hash: ContentHash(e.hash),
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
            self.engine.clone(),
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
            // Low-level deck commands — bypass the state machine.
            // TODO: deprecate these in favour of queue-based commands once all
            // callers have been migrated.
            // ---------------------------------------------------------------
            Command::LoadTrack { path, deck } => {
                info!("Loading track {:?} on deck {:?}", path, deck);
                let result = self.engine.lock().await.load_track(deck, &path).await;
                info!("Track loaded");
                self.create_response(result, None)
            }
            Command::Play { deck } => {
                info!("About to play deck {:?}", deck);
                let result = self.engine.lock().await.play(deck);
                info!("Play command completed for deck {:?}: {:?}", deck, result);
                if result.is_ok() {
                    if let Some(hash) = self.state.lock().await.current_hash().cloned() {
                        write_fact(
                            &self.acid_client,
                            &hash,
                            MusicValue::TrackStarted(StartReason::OnRequest),
                        );
                    }
                }
                self.create_response(result, None)
            }
            // ---------------------------------------------------------------
            // State-machine-backed commands
            // ---------------------------------------------------------------
            Command::Stop { deck: _ } => {
                info!("Stopping playback");
                let effects = self.state.lock().await.stop();
                let result =
                    execute_effects(effects, &self.engine, &self.event_pub, &self.acid_client)
                        .await;
                self.create_response(result, None)
            }
            Command::Pause { deck: _ } => {
                info!("Pausing playback");
                let effects = self.state.lock().await.pause();
                let result =
                    execute_effects(effects, &self.engine, &self.event_pub, &self.acid_client)
                        .await;
                self.create_response(result, None)
            }
            Command::Resume { deck: _ } => {
                info!("Resuming playback");
                let effects = self.state.lock().await.resume();
                let result =
                    execute_effects(effects, &self.engine, &self.event_pub, &self.acid_client)
                        .await;
                self.create_response(result, None)
            }
            Command::SetVolume { deck, db } => {
                info!("Setting volume on deck {:?} to {}dB", deck, db);
                let result = self.engine.lock().await.set_volume(deck, db);
                self.create_response(result, None)
            }
            Command::Unload { deck } => {
                info!("Unloading deck {:?}", deck);
                let result = self.engine.lock().await.unload_track(deck);
                self.create_response(result, None)
            }
            Command::Seek { deck, position } => {
                info!("Seeking deck {:?} to position {}", deck, position);
                let result = self.engine.lock().await.seek(deck, position).await;
                self.create_response(result, None)
            }
            Command::GetLength { deck } => {
                info!("Getting length for deck {:?}", deck);
                todo!("get length of track, or remove opportunity")
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
                let hashes: Vec<ContentHash> = self
                    .queue
                    .lock()
                    .await
                    .iter()
                    .map(|e| e.hash.clone())
                    .collect();
                info!("Queue list: {} items", hashes.len());
                Response {
                    success: true,
                    error_message: String::new(),
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
                Response {
                    success: true,
                    error_message: String::new(),
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
                    None => Response {
                        success: false,
                        error_message: "Queue is empty".to_string(),
                        data: None,
                    },
                    Some(e) => {
                        let effects = self
                            .state
                            .lock()
                            .await
                            .play_queue(e.hash.clone(), e.path.clone());
                        let result = execute_effects(
                            effects,
                            &self.engine,
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
                Response {
                    success: true,
                    error_message: String::new(),
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
                    execute_effects(effects, &self.engine, &self.event_pub, &self.acid_client)
                        .await;
                self.create_response(result, None)
            }
            Command::GetSession => {
                let session_id = self.state.lock().await.session_id().map(|id| id.0.clone());
                info!("GetSession: {:?}", session_id);
                Response {
                    success: true,
                    error_message: String::new(),
                    data: Some(ResponseData::Session(session_id)),
                }
            }
        }
    }

    fn ok_response(&self) -> Response {
        Response {
            success: true,
            error_message: String::new(),
            data: None,
        }
    }

    fn create_response(
        &self,
        result: Result<(), PlaybackError>,
        data: Option<ResponseData>,
    ) -> Response {
        match result {
            Ok(()) => {
                info!("Command completed successfully");
                Response {
                    success: true,
                    error_message: String::new(),
                    data,
                }
            }
            Err(e) => {
                warn!("Command failed: {}", e);
                Response {
                    success: false,
                    error_message: e.to_string(),
                    data: None,
                }
            }
        }
    }
}

/// Execute a list of effects produced by the state machine.
async fn execute_effects(
    effects: Vec<PlaybackEffect>,
    engine: &Arc<Mutex<PlaybackEngine>>,
    event_pub: &nng::Socket,
    acid_client: &AcidClient,
) -> Result<(), PlaybackError> {
    for effect in effects {
        match effect {
            PlaybackEffect::StopEngine => {
                engine.lock().await.stop(Deck::A)?;
            }
            PlaybackEffect::PlayEngine => {
                let mut eng = engine.lock().await;
                eng.set_stream_active(true);
                eng.play(Deck::A)?;
            }
            PlaybackEffect::LoadAndPlay { hash: _, path } => {
                let mut eng = engine.lock().await;
                eng.set_stream_active(true);
                eng.load_track(Deck::A, &path).await?;
                eng.play(Deck::A)?;
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
            hash: e.hash.0.clone(),
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

/// Polls deck A every 200 ms. When the track reaches `Finished`, pops the next
/// entry from the queue and starts playing it automatically.
/// Auto-advance is suppressed while playback is paused (state machine handles this).
/// Every 5th poll iteration (~1 s) broadcasts a `PositionUpdate` event while playing.
async fn auto_advance_task(
    engine: Arc<Mutex<PlaybackEngine>>,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    state: Arc<Mutex<PlaybackStateMachine>>,
    event_pub: nng::Socket,
    acid_client: Arc<AcidClient>,
    queue_file: PathBuf,
) {
    let mut tick: u8 = 0;
    let mut idle_since: Option<tokio::time::Instant> = None;
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        tick = tick.wrapping_add(1);

        // Every 5th tick (~1 s) broadcast a position update while playing.
        if tick.is_multiple_of(5) {
            let current_state = state.lock().await;
            if let PlaybackState::Playing { hash } = current_state.state() {
                let h = hash.clone();
                drop(current_state);
                let eng = engine.lock().await;
                if let (Some(position_ms), Some(duration_ms)) =
                    (eng.position_ms(Deck::A), eng.duration_ms(Deck::A))
                {
                    drop(eng);
                    publish_event_on_socket(
                        &event_pub,
                        &PlaybackEvent::PositionUpdate {
                            hash: h.0.clone(),
                            position_ms,
                            duration_ms,
                        },
                    );
                }
            }
        }

        // Check whether the current state is Playing before checking engine.
        // If Paused or Idle, do not auto-advance.
        {
            let current_state = state.lock().await;
            match current_state.state() {
                PlaybackState::Playing { .. } => {
                    // If the stream was deactivated during idle, reactivate it now.
                    // The engine is the source of truth for stream active state.
                    if !engine.lock().await.is_stream_active() {
                        engine.lock().await.set_stream_active(true);
                        idle_since = None;
                        info!("Stream reactivated: playback resumed");
                    } else {
                        idle_since = None;
                    }
                }
                PlaybackState::Paused { .. } => {
                    idle_since = None;
                    continue;
                }
                PlaybackState::Idle => {
                    // Track idle start time.
                    if idle_since.is_none() {
                        idle_since = Some(tokio::time::Instant::now());
                    }
                    // Deactivate stream after 5 seconds of idle.
                    if let Some(since) = idle_since {
                        if since.elapsed() >= Duration::from_secs(5)
                            && engine.lock().await.is_stream_active()
                        {
                            engine.lock().await.set_stream_active(false);
                            info!("Stream deactivated: idle for 5 seconds");
                        }
                    }
                    continue;
                }
            }
        }

        let finished = engine.lock().await.is_track_finished(Deck::A);
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

        if let Err(e) = execute_effects(effects, &engine, &event_pub, &acid_client).await {
            warn!("Auto-advance failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_client::AcidClient;
    use std::path::PathBuf;

    fn make_server() -> Server {
        let engine = PlaybackEngine::new().unwrap();
        let engine = Arc::new(Mutex::new(engine));
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let event_pub = nng::Socket::new(nng::Protocol::Pub0).unwrap();
        // Create a dummy ACID listener so the client can connect.
        let acid_listen = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let acid_addr = format!("ipc:///tmp/test_acid_{}.sock", std::process::id());
        acid_listen.listen(&acid_addr).unwrap();
        let acid_client = Arc::new(AcidClient::connect(&acid_addr).unwrap());
        std::mem::forget(acid_listen);
        Server::new(
            engine,
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
            let hash = ContentHash("sha256:test".to_string());
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

        // Issue Skip via state machine (engine stop may error on no-loaded track,
        // but state must still transition to Idle).
        let effects = server.state.lock().await.skip(None);
        // Ignore engine errors — we only care about the state transition.
        let _ = execute_effects(
            effects,
            &server.engine,
            &server.event_pub,
            &server.acid_client,
        )
        .await;

        assert!(
            matches!(server.state.lock().await.state(), PlaybackState::Idle),
            "Expected Idle after Skip from Paused"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_handle_nonexistent_track() {
        let engine = PlaybackEngine::new().unwrap();
        let engine = Arc::new(Mutex::new(engine));

        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let event_pub = nng::Socket::new(nng::Protocol::Pub0).unwrap();
        let acid_listen = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let acid_addr = format!("ipc:///tmp/test_acid_ignored_{}.sock", std::process::id());
        acid_listen.listen(&acid_addr).unwrap();
        let acid_client = Arc::new(AcidClient::connect(&acid_addr).unwrap());
        std::mem::forget(acid_listen);
        let server = Server::new(
            engine,
            socket,
            PathBuf::from("/tmp/test_queue.json"),
            event_pub,
            acid_client,
        );

        let nonexistent_path = PathBuf::from("/this/file/does/not/exist.flac");
        let command = Command::LoadTrack {
            path: nonexistent_path.clone(),
            deck: playback_engine::Deck::A,
        };

        let response = server.handle_command(command).await;
        assert!(!response.success);
        assert!(
            response.error_message.contains("No such file or directory"),
            "Error message '{}' should contain path '{}'",
            response.error_message,
            nonexistent_path.display()
        );
        assert!(response.data.is_none());
    }
}
