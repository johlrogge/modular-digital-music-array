use crate::error::ServerError;
use color_eyre::Result;
use media_protocol::{Command, ContentHash, Response, ResponseData};
use nng::Socket;
use playback_engine::{Deck, PlaybackEngine, PlaybackError};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};

struct QueueEntry {
    hash: ContentHash,
    path: PathBuf,
}

pub struct Server {
    engine: Arc<Mutex<PlaybackEngine>>,
    socket: Socket,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
}

impl Server {
    pub fn new(engine: Arc<Mutex<PlaybackEngine>>, socket: Socket) -> Self {
        Self {
            engine,
            socket,
            queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        info!("Playback server starting...");

        // Background task: auto-advance to next queued track when current track finishes.
        tokio::spawn(auto_advance_task(self.engine.clone(), self.queue.clone()));

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
                self.queue
                    .lock()
                    .await
                    .push_front(QueueEntry { hash, path });
                self.ok_response()
            }
            Command::QueueAppend { hash, path } => {
                info!("Queue append: {:?}", path);
                self.queue.lock().await.push_back(QueueEntry { hash, path });
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
                self.queue.lock().await.clear();
                info!("Queue cleared");
                self.ok_response()
            }
            Command::QueueRemove { hashes } => {
                self.queue
                    .lock()
                    .await
                    .retain(|e| !hashes.contains(&e.hash));
                info!("Removed {} hash(es) from queue", hashes.len());
                self.ok_response()
            }
            Command::PlayQueue => {
                info!("Play from queue");
                let entry = self.queue.lock().await.pop_front();
                match entry {
                    None => Response {
                        success: false,
                        error_message: "Queue is empty".to_string(),
                        data: None,
                    },
                    Some(e) => {
                        let result = load_and_play(&self.engine, &e.path).await;
                        self.create_response(result, None)
                    }
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
    path: &PathBuf,
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
        let server = Server::new(engine, socket);

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
