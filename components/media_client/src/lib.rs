//! IPC Client for mdma-playback
//!
//! NNG client wrapper for connecting to the playback service.
//! Used by mdma-cli.

pub use media_protocol::{
    AudioOutputConfig, AudioSinkInfo, Command, ContentHash, Deck, Held, PreviewStatus, Response,
    ResponseData, SourceName, Transport, TransportStatus,
};
pub use playback_primitives::{PositionMs, Volume};

use std::path::PathBuf;

/// The transport in a response's data, for the commands that answer one
/// (`GetTransport` and `Seek`).
pub fn into_transport(data: ResponseData) -> Option<TransportStatus> {
    match data {
        ResponseData::Transport(status) => Some(status),
        _ => None,
    }
}

/// The audible track in a response's data, for the commands that answer one
/// (`NowPlaying` and `EndPreview`).
pub fn into_now_playing(data: ResponseData) -> Option<Option<ContentHash>> {
    match data {
        ResponseData::NowPlaying(hash) => Some(hash),
        _ => None,
    }
}

/// Errors that can occur when communicating with the playback service.
///
/// This is an alias for [`nng_transport::NngClientError`]. The `Service` variant
/// carries command-level error messages.
pub type ClientError = nng_transport::NngClientError;

/// A clone shares the socket.
#[derive(Clone)]
pub struct MediaClient {
    socket: nng_transport::SharedSocket,
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
        let socket = nng_transport::SharedSocket::connect(url)?;
        Ok(Self { socket })
    }

    /// Whether `other` is a clone of this client, on the same connection.
    pub fn is_same_connection(&self, other: &Self) -> bool {
        self.socket.is_same_socket(&other.socket)
    }

    /// Why a request found the node unreachable since this was last asked, if one
    /// did; see [`nng_transport::SharedSocket::take_unreachable`].
    pub fn take_unreachable(&self) -> Option<String> {
        self.socket.take_unreachable()
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

    /// Move the audible track to `position` (kept within the track); playing
    /// stays playing, paused stays paused. Answers where the track is now.
    pub fn seek(&self, deck: Deck, position: PositionMs) -> Result<TransportStatus, ClientError> {
        self.send_command_with_response(
            Command::Seek {
                deck,
                position_ms: position,
            },
            into_transport,
        )
    }

    /// Play `hash` (a full one) at once, holding what is audible to go back to
    /// (ADR-031). Previewing again replaces the preview.
    pub fn preview(&self, deck: Deck, hash: ContentHash) -> Result<(), ClientError> {
        self.send_command(Command::Preview { deck, hash })
    }

    /// End the preview and go back to what it held; nothing happens when no
    /// preview runs. Answers the track that is audible afterwards (`None`:
    /// nothing was held, so playback is idle).
    pub fn end_preview(&self, deck: Deck) -> Result<Option<ContentHash>, ClientError> {
        self.send_command_with_response(Command::EndPreview { deck }, into_now_playing)
    }

    /// Transport state, audible track, position and duration of `deck`.
    pub fn get_transport(&self, deck: Deck) -> Result<TransportStatus, ClientError> {
        self.send_command_with_response(Command::GetTransport { deck }, into_transport)
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

    pub fn queue_next(&self, hash: ContentHash, source: SourceName) -> Result<(), ClientError> {
        self.send_command(Command::QueueNext { hash, source })
    }

    pub fn queue_append(&self, hash: ContentHash, source: SourceName) -> Result<(), ClientError> {
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

    pub fn queue_replace(
        &self,
        entries: Vec<(ContentHash, SourceName)>,
    ) -> Result<(), ClientError> {
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
        self.send_command_with_response(Command::NowPlaying, into_now_playing)
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
        let response: Response = self.socket.request_response(&cmd)?;
        tracing::debug!("Received response");

        match response {
            Response::Ok { .. } => Ok(()),
            Response::Err { message } => Err(ClientError::Refused(message)),
        }
    }

    fn send_command_with_response<T>(
        &self,
        cmd: Command,
        extract: fn(ResponseData) -> Option<T>,
    ) -> Result<T, ClientError> {
        let response: Response = self.socket.request_response(&cmd)?;

        match response {
            Response::Err { message } => Err(ClientError::Refused(message)),
            Response::Ok { data: Some(data) } => extract(data)
                .ok_or_else(|| ClientError::Service("Unexpected response data type".to_string())),
            Response::Ok { data: None } => {
                Err(ClientError::Service("Missing response data".to_string()))
            }
        }
    }
}
