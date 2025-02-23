use media_protocol::{ClientError, Command, Deck, Response};
use nng::{Protocol, Socket};
use std::path::PathBuf;

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

    pub fn load_track(&self, path: PathBuf, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::LoadTrack { path, deck };
        self.send_command(cmd)
    }

    pub fn play(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Play { deck };
        self.send_command(cmd)
    }

    pub fn stop(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Stop { deck };
        self.send_command(cmd)
    }

    pub fn set_volume(&self, deck: Deck, db: f32) -> Result<(), ClientError> {
        let cmd = Command::SetVolume { deck, db };
        self.send_command(cmd)
    }

    pub fn unload_track(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Unload { deck };
        self.send_command(cmd)
    }

    fn send_command(&self, cmd: Command) -> Result<(), ClientError> {
        let data = serde_json::to_vec(&cmd).map_err(|e| ClientError::Protocol(e.to_string()))?;

        self.socket
            .send(&data)
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        let msg = self
            .socket
            .recv()
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        let response: Response =
            serde_json::from_slice(&msg).map_err(|e| ClientError::Protocol(e.to_string()))?;

        if !response.success {
            return Err(ClientError::Command(response.error_message));
        }

        Ok(())
    }
}
