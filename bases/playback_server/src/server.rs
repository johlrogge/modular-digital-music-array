use crate::error::ServerError;
use color_eyre::Result;
use media_protocol::{Command, Deck as ProtocolChannel, Response};
use nng::Socket;
use parking_lot::Mutex;
use playback_engine::{self, PlaybackEngine};
use std::sync::Arc;
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
            let response = self.handle_command(command);

            // Send response
            let response_data = serde_json::to_vec(&response)?;
            self.socket
                .send(&response_data)
                .map_err(ServerError::from)?;
        }
    }
    fn handle_command(&self, command: Command) -> Response {
        let mut engine = self.engine.lock();

        let result = match command {
            Command::LoadTrack { path, deck } => {
                info!("Loading track {:?} on deck {:?}", path, deck);
                engine.load_track(path, Self::convert_deck(deck))
            }
            Command::Play { deck } => {
                info!("Playing deck {:?}", deck);
                engine.play(Self::convert_deck(deck))
            }
            Command::Stop { deck } => {
                info!("Stopping deck {:?}", deck);
                engine.stop(Self::convert_deck(deck))
            }
            Command::SetVolume { deck, db } => {
                info!("Setting volume on deck {:?} to {}dB", deck, db);
                engine.set_volume(Self::convert_deck(deck), db)
            }
            Command::Unload { deck } => {
                info!("Unloading deck {:?}", deck);
                engine.unload_track(Self::convert_deck(deck))
            }
        };

        match result {
            Ok(()) => {
                info!("Command completed successfully");
                Response {
                    success: true,
                    error_message: String::new(),
                }
            }
            Err(e) => {
                warn!("Command failed: {}", e);
                Response {
                    success: false,
                    error_message: e.to_string(),
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

    #[test]
    fn test_handle_nonexistent_track() {
        let engine = PlaybackEngine::new().unwrap();
        let engine = Arc::new(Mutex::new(engine));

        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let server = Server::new(engine, socket);

        let nonexistent_path = PathBuf::from("/this/file/does/not/exist.flac");
        let command = Command::LoadTrack {
            path: nonexistent_path.clone(),
            deck: ProtocolChannel::A,
        };

        let response = server.handle_command(command);
        assert!(!response.success);
        assert!(
            response.error_message.contains("No such file or directory"),
            "Error message '{}' should contain path '{}'",
            response.error_message,
            nonexistent_path.display()
        );
    }
}
