//! IPC Client for mdma-playback
//!
//! NNG client wrapper for connecting to the playback service.
//! Used by mdma-cli.

pub use media_protocol::{Command, Deck, Response, ResponseData};

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur when communicating with the playback service.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Connection error: {0}")]
    Connection(#[from] nng_transport::ConnectionError),

    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Command failed: {0}")]
    Command(String),
}

pub struct MediaClient {
    socket: nng::Socket,
}

impl MediaClient {
    /// Connect to the playback service at the given address.
    ///
    /// Supports both IPC (`ipc:///path/to/socket`) and TCP (`tcp://host:port`).
    /// For TCP addresses, hostnames are resolved to IPv4 addresses.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use media_client::MediaClient;
    ///
    /// let client = MediaClient::connect("ipc:///run/mdma/playback.sock")?;
    /// let client = MediaClient::connect("tcp://mdma-909.local:5557")?;
    /// # Ok::<(), media_client::ClientError>(())
    /// ```
    pub fn connect(url: &str) -> Result<Self, ClientError> {
        let socket = nng_transport::connect(url)?;
        Ok(Self { socket })
    }

    pub fn load_track(&self, path: PathBuf, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::LoadTrack { path, deck };
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

    pub fn play(&self, deck: Deck) -> Result<(), ClientError> {
        tracing::debug!("Sending Play command for deck {:?}", deck);
        let cmd = Command::Play { deck };
        let result = self.send_command(cmd);
        tracing::debug!("Play command result: {:?}", result);
        result
    }

    pub fn seek(&self, deck: Deck, position: usize) -> Result<(), ClientError> {
        let cmd = Command::Seek { deck, position };
        self.send_command(cmd)
    }

    pub fn get_length(&self, deck: Deck) -> Result<usize, ClientError> {
        let cmd = Command::GetLength { deck };
        self.send_command_with_response(cmd, |data| {
            if let ResponseData::Length(len) = data {
                Some(len)
            } else {
                None
            }
        })
    }

    fn send_command(&self, cmd: Command) -> Result<(), ClientError> {
        tracing::debug!("Serializing command: {:?}", cmd);
        let data = serde_json::to_vec(&cmd)?;

        let msg = nng::Message::from(&data[..]);
        tracing::debug!("Sending {} bytes to server", data.len());
        self.socket
            .send(msg)
            .map_err(|(_, e)| ClientError::Nng(e))?;

        tracing::debug!("Waiting for response...");
        let response_msg = self.socket.recv()?;
        tracing::debug!("Received response of {} bytes", response_msg.len());

        let response: Response = serde_json::from_slice(&response_msg)?;

        if !response.success {
            return Err(ClientError::Command(response.error_message));
        }

        Ok(())
    }

    fn send_command_with_response<T>(
        &self,
        cmd: Command,
        extract: fn(ResponseData) -> Option<T>,
    ) -> Result<T, ClientError> {
        let data = serde_json::to_vec(&cmd)?;

        let msg = nng::Message::from(&data[..]);
        self.socket
            .send(msg)
            .map_err(|(_, e)| ClientError::Nng(e))?;

        let response_msg = self.socket.recv()?;
        let response: Response = serde_json::from_slice(&response_msg)?;

        if !response.success {
            return Err(ClientError::Command(response.error_message));
        }

        match response.data {
            Some(data) => extract(data)
                .ok_or_else(|| ClientError::Command("Unexpected response data type".to_string())),
            None => Err(ClientError::Command("Missing response data".to_string())),
        }
    }
}
