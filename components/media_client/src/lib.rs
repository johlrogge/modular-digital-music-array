//! IPC Client for mdma-playback
//!
//! NNG client wrapper for connecting to the playback service.
//! Used by mdma-cli.

pub use media_protocol::{
    AudioOutputConfig, AudioSinkInfo, Command, ContentHash, Deck, Response, ResponseData,
};
pub use playback_primitives::Volume;

use std::path::PathBuf;

/// Errors that can occur when communicating with the playback service.
///
/// This is an alias for [`nng_transport::NngClientError`]. The `Service` variant
/// carries command-level error messages.
pub type ClientError = nng_transport::NngClientError;

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

    pub fn pause(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Pause { deck };
        self.send_command(cmd)
    }

    pub fn resume(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Resume { deck };
        self.send_command(cmd)
    }

    pub fn set_volume(&self, deck: Deck, volume: Volume) -> Result<(), ClientError> {
        let cmd = Command::SetVolume { deck, volume };
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

    pub fn queue_next(&self, hash: ContentHash, source: String) -> Result<(), ClientError> {
        self.send_command(Command::QueueNext { hash, source })
    }

    pub fn queue_append(&self, hash: ContentHash, source: String) -> Result<(), ClientError> {
        self.send_command(Command::QueueAppend { hash, source })
    }

    pub fn queue_list(&self) -> Result<Vec<ContentHash>, ClientError> {
        self.send_command_with_response(Command::QueueList, |data| {
            if let ResponseData::Queue(hashes) = data {
                Some(hashes)
            } else {
                None
            }
        })
    }

    pub fn queue_clear(&self) -> Result<(), ClientError> {
        self.send_command(Command::QueueClear)
    }

    pub fn queue_replace(&self, entries: Vec<(ContentHash, String)>) -> Result<(), ClientError> {
        self.send_command(Command::QueueReplace { entries })
    }

    pub fn queue_remove(&self, hashes: Vec<ContentHash>) -> Result<usize, ClientError> {
        self.send_command_with_response(Command::QueueRemove { hashes }, |data| {
            if let ResponseData::Count(n) = data {
                Some(n)
            } else {
                None
            }
        })
    }

    pub fn now_playing(&self) -> Result<Option<ContentHash>, ClientError> {
        self.send_command_with_response(Command::NowPlaying, |data| {
            if let ResponseData::NowPlaying(hash) = data {
                Some(hash)
            } else {
                None
            }
        })
    }

    pub fn play_queue(&self) -> Result<(), ClientError> {
        self.send_command(Command::PlayQueue)
    }

    pub fn skip(&self) -> Result<(), ClientError> {
        self.send_command(Command::Skip)
    }

    pub fn get_session(&self) -> Result<Option<String>, ClientError> {
        self.send_command_with_response(Command::GetSession, |data| {
            if let ResponseData::Session(id) = data {
                Some(id)
            } else {
                None
            }
        })
    }

    pub fn list_audio_outputs(&self) -> Result<Vec<AudioSinkInfo>, ClientError> {
        self.send_command_with_response(Command::ListAudioOutputs, |data| {
            if let ResponseData::AudioOutputs(sinks) = data {
                Some(sinks)
            } else {
                None
            }
        })
    }

    pub fn set_audio_output(&self, device_name: String) -> Result<AudioOutputConfig, ClientError> {
        self.send_command_with_response(Command::SetAudioOutput { device_name }, |data| {
            if let ResponseData::AudioOutput(cfg) = data {
                Some(cfg)
            } else {
                None
            }
        })
    }

    pub fn get_audio_output(&self) -> Result<AudioOutputConfig, ClientError> {
        self.send_command_with_response(Command::GetAudioOutput, |data| {
            if let ResponseData::AudioOutput(cfg) = data {
                Some(cfg)
            } else {
                None
            }
        })
    }

    fn send_command(&self, cmd: Command) -> Result<(), ClientError> {
        tracing::debug!("Serializing command: {:?}", cmd);
        tracing::debug!("Sending command to server");
        let response: Response = nng_transport::request_response(&self.socket, &cmd)?;
        tracing::debug!("Received response");

        match response {
            Response::Ok { .. } => Ok(()),
            Response::Err { message } => Err(ClientError::Service(message)),
        }
    }

    fn send_command_with_response<T>(
        &self,
        cmd: Command,
        extract: fn(ResponseData) -> Option<T>,
    ) -> Result<T, ClientError> {
        let response: Response = nng_transport::request_response(&self.socket, &cmd)?;

        match response {
            Response::Err { message } => Err(ClientError::Service(message)),
            Response::Ok { data: Some(data) } => extract(data)
                .ok_or_else(|| ClientError::Service("Unexpected response data type".to_string())),
            Response::Ok { data: None } => {
                Err(ClientError::Service("Missing response data".to_string()))
            }
        }
    }
}
