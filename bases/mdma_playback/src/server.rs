use crate::error::ServerError;
use color_eyre::Result;
use media_protocol::{Command, Response, ResponseData};
use nng::Socket;
use playback_engine::{PlaybackEngine, PlaybackError};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub struct Server {
    engine: Arc<Mutex<PlaybackEngine>>,
    socket: Socket,
}

impl Server {
    pub fn new(engine: Arc<Mutex<PlaybackEngine>>, socket: Socket) -> Self {
        Self { engine, socket }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        info!("Playback server starting...");

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
                // Use .await to acquire the lock asynchronously
                let result = self.engine.lock().await.load_track(deck, &path).await;
                info!("Track loaded");
                self.create_response(result, None)
            } // For non-async operations, keep the original pattern
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
                let result = self.engine.lock().await.seek(deck, position).await; // Now awaiting the seek operation
                self.create_response(result, None)
            }
            Command::GetLength { deck } => {
                info!("Getting length for deck {:?}", deck);
                todo!("get length of track, or remove opportunity")
            }
        }
    }

    // Add a helper method to create responses
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
