mod error;
mod protocol;
mod types;

use error::ClientError;
use nng::{Protocol, Socket};
use protocol::{Command, Response};
use std::path::PathBuf;
pub use types::Channel;

pub struct MediaClient {
    socket: Socket,
}

impl MediaClient {
    pub fn connect(url: &str) -> Result<Self, ClientError> {
        let socket =
            Socket::new(Protocol::Req0).map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        socket
            .dial(url)
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        Ok(Self { socket })
    }

    pub fn load_track(&self, path: PathBuf, channel: Channel) -> Result<(), ClientError> {
        let cmd = Command::LoadTrack { path, channel };
        self.send_command(cmd)
    }

    pub fn play(&self, channel: Channel) -> Result<(), ClientError> {
        let cmd = Command::Play { channel };
        self.send_command(cmd)
    }

    pub fn stop(&self, channel: Channel) -> Result<(), ClientError> {
        let cmd = Command::Stop { channel };
        self.send_command(cmd)
    }

    pub fn set_volume(&self, channel: Channel, db: f32) -> Result<(), ClientError> {
        let cmd = Command::SetVolume { channel, db };
        self.send_command(cmd)
    }

    fn send_command(&self, cmd: Command) -> Result<(), ClientError> {
        // Serialize command
        let data = serde_json::to_vec(&cmd).map_err(|e| ClientError::Protocol(e.to_string()))?;

        // Send command
        self.socket
            .send(&data)
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        // Receive response
        let msg = self
            .socket
            .recv()
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        // Convert to slice for deserialization
        let msg_slice = msg.as_slice();

        // Parse response
        let response: Response =
            serde_json::from_slice(msg_slice).map_err(|e| ClientError::Protocol(e.to_string()))?;

        if !response.success {
            return Err(ClientError::Command(response.error_message));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_conversion() {
        assert_eq!(i32::from(Channel::A), 0);
        assert_eq!(i32::from(Channel::B), 1);

        assert!(matches!(Channel::try_from(0), Ok(Channel::A)));
        assert!(matches!(Channel::try_from(1), Ok(Channel::B)));
        assert!(Channel::try_from(2).is_err());
    }
}
