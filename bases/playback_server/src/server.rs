use crate::error::ServerError;
use color_eyre::Result;
use media_protocol::{Command, Deck as ProtocolChannel, Response, ResponseData};
use nng::Socket;
use playback_engine::{self, PlaybackEngine, PlaybackError};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub struct Server {
    engine: Arc<Mutex<PlaybackEngine>>,
    socket: Socket,
}

impl Server {
    // Convert protocol channel to playback channel
    fn convert_deck(protocol_channel: ProtocolChannel) -> playback_engine::Deck {
        match protocol_channel {
            ProtocolChannel::A => playback_engine::Deck::A,
            ProtocolChannel::B => playback_engine::Deck::B,
        }
    }

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

            // Send response
            let response_data = serde_json::to_vec(&response)?;
            self.socket
                .send(&response_data)
                .map_err(ServerError::from)?;
        }
    }

    async fn handle_command(&self, command: Command) -> Response {
        match command {
            Command::LoadTrack { path, deck } => {
                info!("Loading track {:?} on deck {:?}", path, deck);
                // Use .await to acquire the lock asynchronously
                let result = self
                    .engine
                    .lock()
                    .await
                    .load_track(Self::convert_deck(deck), &path)
                    .await;
                self.create_response(result, None)
            } // For non-async operations, keep the original pattern
            Command::Play { deck } => {
                info!("Playing deck {:?}", deck);
                let result = self.engine.lock().await.play(Self::convert_deck(deck));
                self.create_response(result, None)
            }
            Command::Stop { deck } => {
                info!("Stopping deck {:?}", deck);
                let result = self.engine.lock().await.stop(Self::convert_deck(deck));
                self.create_response(result, None)
            }
            Command::SetVolume { deck, db } => {
                info!("Setting volume on deck {:?} to {}dB", deck, db);
                let result = self
                    .engine
                    .lock()
                    .await
                    .set_volume(Self::convert_deck(deck), db);
                self.create_response(result, None)
            }
            Command::Unload { deck } => {
                info!("Unloading deck {:?}", deck);
                let result = self
                    .engine
                    .lock()
                    .await
                    .unload_track(Self::convert_deck(deck));
                self.create_response(result, None)
            }

            // New commands
            Command::Seek { deck, position } => {
                info!("Seeking deck {:?} to position {}", deck, position);
                let result = self
                    .engine
                    .lock()
                    .await
                    .seek(Self::convert_deck(deck), position);
                self.create_response(result, None)
            }
            Command::GetPosition { deck } => {
                info!("Getting position for deck {:?}", deck);
                match self
                    .engine
                    .lock()
                    .await
                    .get_position(Self::convert_deck(deck))
                {
                    Ok(position) => {
                        info!("Current position: {}", position);
                        self.create_response(Ok(()), Some(ResponseData::Position(position)))
                    }
                    Err(e) => {
                        warn!("Failed to get position: {}", e);
                        self.create_response(Err(e), None)
                    }
                }
            }
            Command::GetLength { deck } => {
                info!("Getting length for deck {:?}", deck);
                match self
                    .engine
                    .lock()
                    .await
                    .get_length(Self::convert_deck(deck))
                {
                    Ok(length) => {
                        info!("Track length: {}", length);
                        self.create_response(Ok(()), Some(ResponseData::Length(length)))
                    }
                    Err(e) => {
                        warn!("Failed to get length: {}", e);
                        self.create_response(Err(e), None)
                    }
                }
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

    #[test]
    fn test_channel_conversion() {
        use playback_engine::Deck as PlaybackChannel;

        assert!(matches!(
            Server::convert_deck(ProtocolChannel::A),
            PlaybackChannel::A
        ));

        assert!(matches!(
            Server::convert_deck(ProtocolChannel::B),
            PlaybackChannel::B
        ));
    }

    #[tokio::test]
    async fn test_handle_nonexistent_track() {
        let engine = PlaybackEngine::new().unwrap();
        let engine = Arc::new(Mutex::new(engine));

        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let server = Server::new(engine, socket);

        let nonexistent_path = PathBuf::from("/this/file/does/not/exist.flac");
        let command = Command::LoadTrack {
            path: nonexistent_path.clone(),
            deck: ProtocolChannel::A,
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
