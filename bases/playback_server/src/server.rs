use color_eyre::Result;
use media_protocol::{Channel as ProtocolChannel, Command, Response};
use nng::Socket;
use playback_engine::{self, PlaybackEngine};
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

use crate::error::ServerError;

pub struct Server {
    engine: Arc<Mutex<PlaybackEngine>>,
    socket: Socket,
}

impl Server {
    // Convert protocol channel to playback channel
    fn convert_channel(protocol_channel: ProtocolChannel) -> playback_engine::Channel {
        match protocol_channel {
            ProtocolChannel::ChannelA => playback_engine::Channel::ChannelA,
            ProtocolChannel::ChannelB => playback_engine::Channel::ChannelB,
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
        let mut engine = match self.engine.lock() {
            Ok(engine) => engine,
            Err(e) => {
                error!("Failed to lock engine: {}", e);
                return Response {
                    success: false,
                    error_message: "Internal server error".into(),
                };
            }
        };

        let result = match command {
            Command::LoadTrack { path, channel } => {
                info!("Loading track {:?} on channel {:?}", path, channel);
                engine.load_track(path, Self::convert_channel(channel))
            }
            Command::Play { channel } => {
                info!("Playing channel {:?}", channel);
                engine.play(Self::convert_channel(channel))
            }
            Command::Stop { channel } => {
                info!("Stopping channel {:?}", channel);
                engine.stop(Self::convert_channel(channel))
            }
            Command::SetVolume { channel, db } => {
                info!("Setting volume on channel {:?} to {}dB", channel, db);
                engine.set_volume(Self::convert_channel(channel), db)
            }
            Command::Unload { channel } => {
                info!("Unloading channel {:?}", channel);
                engine.unload_track(Self::convert_channel(channel))
            }
        };

        match result {
            Ok(()) => Response {
                success: true,
                error_message: String::new(),
            },
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
        use playback_engine::Channel as PlaybackChannel;

        assert!(matches!(
            Server::convert_channel(ProtocolChannel::ChannelA),
            PlaybackChannel::ChannelA
        ));

        assert!(matches!(
            Server::convert_channel(ProtocolChannel::ChannelB),
            PlaybackChannel::ChannelB
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
            channel: ProtocolChannel::ChannelA,
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
