//! Playback backend abstraction — works in both gateway and direct mode.

use media_client::{
    AudioOutputConfig, AudioSinkInfo, ClientError, Command, ContentHash, Deck, MediaClient,
    Response, ResponseData, SourceName,
};

/// Abstraction for playback commands, works in both gateway and direct mode.
pub enum PlaybackBackend {
    Direct(MediaClient),
    Gateway(gateway_client::GatewayClient),
}

impl PlaybackBackend {
    /// Connect directly to the playback IPC socket.
    pub fn connect_direct(socket: &str) -> Result<Self, ClientError> {
        let client = MediaClient::connect(socket)?;
        Ok(PlaybackBackend::Direct(client))
    }

    /// Connect to playback via gateway.
    pub fn connect_gateway(gateway: &str) -> Result<Self, ClientError> {
        let gw = gateway_client::GatewayClient::connect(gateway)?;
        Ok(PlaybackBackend::Gateway(gw))
    }

    /// Connect using gateway if provided, otherwise direct.
    pub fn connect(gateway: Option<&str>, socket: &str) -> Result<Self, ClientError> {
        match gateway {
            Some(gw) => Self::connect_gateway(gw),
            None => Self::connect_direct(socket),
        }
    }

    pub fn play_queue(&self) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.play_queue(),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::PlayQueue),
        }
    }

    pub fn skip(&self) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.skip(),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::Skip),
        }
    }

    pub fn stop(&self, deck: Deck) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.stop(deck),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::Stop { deck }),
        }
    }

    pub fn pause(&self, deck: Deck) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.pause(deck),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::Pause { deck }),
        }
    }

    pub fn resume(&self, deck: Deck) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.resume(deck),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::Resume { deck }),
        }
    }

    pub fn now_playing(&self) -> Result<Option<ContentHash>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.now_playing(),
            PlaybackBackend::Gateway(gw) => gw_extract(gw, Command::NowPlaying, |data| {
                if let ResponseData::NowPlaying(hash) = data {
                    Some(hash)
                } else {
                    None
                }
            }),
        }
    }

    pub fn queue_next(&self, hash: ContentHash, source: SourceName) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_next(hash, source),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::QueueNext { hash, source }),
        }
    }

    pub fn queue_append(&self, hash: ContentHash, source: SourceName) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_append(hash, source),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::QueueAppend { hash, source }),
        }
    }

    pub fn queue_list(&self) -> Result<Vec<ContentHash>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_list(),
            PlaybackBackend::Gateway(gw) => gw_extract(gw, Command::QueueList, |data| {
                if let ResponseData::Queue(hashes) = data {
                    Some(hashes)
                } else {
                    None
                }
            }),
        }
    }

    pub fn queue_clear(&self) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_clear(),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::QueueClear),
        }
    }

    pub fn queue_replace(
        &self,
        entries: Vec<(ContentHash, SourceName)>,
    ) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_replace(entries),
            PlaybackBackend::Gateway(gw) => gw_command(gw, Command::QueueReplace { entries }),
        }
    }

    pub fn queue_remove(&self, hashes: Vec<ContentHash>) -> Result<usize, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_remove(hashes),
            PlaybackBackend::Gateway(gw) => {
                gw_extract(gw, Command::QueueRemove { hashes }, |data| {
                    if let ResponseData::Count(n) = data {
                        Some(n)
                    } else {
                        None
                    }
                })
            }
        }
    }

    pub fn session(&self) -> Result<Option<String>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.get_session(),
            PlaybackBackend::Gateway(gw) => gw_extract(gw, Command::GetSession, |data| {
                if let ResponseData::Session(id) = data {
                    Some(id)
                } else {
                    None
                }
            }),
        }
    }

    pub fn list_audio_outputs(&self) -> Result<Vec<AudioSinkInfo>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.list_audio_outputs(),
            PlaybackBackend::Gateway(gw) => gw_extract(gw, Command::ListAudioOutputs, |data| {
                if let ResponseData::AudioOutputs(sinks) = data {
                    Some(sinks)
                } else {
                    None
                }
            }),
        }
    }

    pub fn set_audio_output(&self, device_name: String) -> Result<AudioOutputConfig, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.set_audio_output(device_name),
            PlaybackBackend::Gateway(gw) => {
                gw_extract(gw, Command::SetAudioOutput { device_name }, |data| {
                    if let ResponseData::AudioOutput(cfg) = data {
                        Some(cfg)
                    } else {
                        None
                    }
                })
            }
        }
    }

    pub fn get_audio_output(&self) -> Result<AudioOutputConfig, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.get_audio_output(),
            PlaybackBackend::Gateway(gw) => gw_extract(gw, Command::GetAudioOutput, |data| {
                if let ResponseData::AudioOutput(cfg) = data {
                    Some(cfg)
                } else {
                    None
                }
            }),
        }
    }
}

fn gw_command(gw: &gateway_client::GatewayClient, cmd: Command) -> Result<(), ClientError> {
    match gw.playback_command(&cmd)? {
        Response::Ok { .. } => Ok(()),
        Response::Err { message } => Err(ClientError::Service(message)),
    }
}

fn gw_extract<T>(
    gw: &gateway_client::GatewayClient,
    cmd: Command,
    extract: fn(ResponseData) -> Option<T>,
) -> Result<T, ClientError> {
    match gw.playback_command(&cmd)? {
        Response::Err { message } => Err(ClientError::Service(message)),
        Response::Ok { data: Some(data) } => extract(data)
            .ok_or_else(|| ClientError::Service("Unexpected response data type".to_string())),
        Response::Ok { data: None } => {
            Err(ClientError::Service("Missing response data".to_string()))
        }
    }
}
