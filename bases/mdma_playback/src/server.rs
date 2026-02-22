use crate::error::ServerError;
use color_eyre::Result;
use media_protocol::{Command, ContentHash, Response, ResponseData};
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
    current_hash: Arc<Mutex<Option<ContentHash>>>,
    queue_file: PathBuf,
}

impl Server {
    pub fn new(engine: Arc<Mutex<PlaybackEngine>>, socket: Socket, queue_file: PathBuf) -> Self {
        Self {
            engine,
            socket,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            current_hash: Arc::new(Mutex::new(None)),
            queue_file,
        }
    }

    /// Serialize and write the queue to disk atomically. Logs a warning on error.
    fn persist_queue(&self, queue: &VecDeque<QueueEntry>) {
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

        // Write atomically: temp file then rename.
        let tmp = self.queue_file.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &json) {
            warn!("Failed to write queue temp file {:?}: {}", tmp, e);
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.queue_file) {
            warn!("Failed to rename queue file: {}", e);
        }
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
            self.current_hash.clone(),
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
                self.create_response(result, None)
            }
            Command::Stop { deck } => {
                info!("Stopping deck {:?}", deck);
                let result = self.engine.lock().await.stop(deck);
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
                self.ok_response()
            }
            Command::QueueAppend { hash, path } => {
                info!("Queue append: {:?}", path);
                let mut queue = self.queue.lock().await;
                queue.push_back(QueueEntry { hash, path });
                self.persist_queue(&queue);
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
                self.ok_response()
            }
            Command::QueueRemove { hashes } => {
                let mut queue = self.queue.lock().await;
                let before = queue.len();
                queue.retain(|e| !hashes.contains(&e.hash));
                let removed = before - queue.len();
                self.persist_queue(&queue);
                info!("Removed {}/{} hash(es) from queue", removed, hashes.len());
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
                self.ok_response()
            }
            Command::PlayQueue => {
                info!("Play from queue");
                let entry = {
                    let mut queue = self.queue.lock().await;
                    let e = queue.pop_front();
                    if e.is_some() {
                        self.persist_queue(&queue);
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
                        *self.current_hash.lock().await = Some(e.hash.clone());
                        let result = load_and_play(&self.engine, &e.path).await;
                        self.create_response(result, None)
                    }
                }
            }
            Command::NowPlaying => {
                let hash = self.current_hash.lock().await.clone();
                info!("Now playing: {:?}", hash);
                Response {
                    success: true,
                    error_message: String::new(),
                    data: Some(ResponseData::NowPlaying(hash)),
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

async fn load_and_play(
    engine: &Arc<Mutex<PlaybackEngine>>,
    path: &Path,
) -> Result<(), PlaybackError> {
    let mut eng = engine.lock().await;
    eng.load_track(Deck::A, path).await?;
    eng.play(Deck::A)
}

/// Polls deck A every 200 ms. When the track reaches `Finished`, pops the next
/// entry from the queue and starts playing it automatically.
async fn auto_advance_task(
    engine: Arc<Mutex<PlaybackEngine>>,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    current_hash: Arc<Mutex<Option<ContentHash>>>,
) {
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;

        let finished = engine.lock().await.is_track_finished(Deck::A);
        if !finished {
            continue;
        }

        let next = queue.lock().await.pop_front();
        let Some(entry) = next else {
            continue;
        };

        info!("Auto-advance: loading {:?}", entry.path);
        *current_hash.lock().await = Some(entry.hash.clone());
        if let Err(e) = load_and_play(&engine, &entry.path).await {
            warn!("Auto-advance failed to load {:?}: {}", entry.path, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    #[ignore]
    async fn test_handle_nonexistent_track() {
        let engine = PlaybackEngine::new().unwrap();
        let engine = Arc::new(Mutex::new(engine));

        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let server = Server::new(engine, socket, PathBuf::from("/tmp/test_queue.json"));

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
