//! Playback backend abstraction — works in both gateway and direct mode.

use media_client::{
    AudioOutputConfig, AudioSinkInfo, ClientError, Command, ContentHash, Deck, MediaClient,
    Response, ResponseData,
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

    fn gw_send(&self, cmd: Command) -> Result<Response, ClientError> {
        match self {
            PlaybackBackend::Gateway(gw) => gw.playback_command(&cmd),
            PlaybackBackend::Direct(_) => unreachable!(),
        }
    }

    fn gw_command(&self, cmd: Command) -> Result<(), ClientError> {
        match self.gw_send(cmd)? {
            Response::Ok { .. } => Ok(()),
            Response::Err { message } => Err(ClientError::Service(message)),
        }
    }

    fn gw_command_with_data(&self, cmd: Command) -> Result<ResponseData, ClientError> {
        match self.gw_send(cmd)? {
            Response::Err { message } => Err(ClientError::Service(message)),
            Response::Ok { data: Some(data) } => Ok(data),
            Response::Ok { data: None } => {
                Err(ClientError::Service("Missing response data".to_string()))
            }
        }
    }

    pub fn play_queue(&self) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.play_queue(),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::PlayQueue),
        }
    }

    pub fn skip(&self) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.skip(),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::Skip),
        }
    }

    pub fn stop(&self, deck: Deck) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.stop(deck),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::Stop { deck }),
        }
    }

    pub fn pause(&self, deck: Deck) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.pause(deck),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::Pause { deck }),
        }
    }

    pub fn resume(&self, deck: Deck) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.resume(deck),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::Resume { deck }),
        }
    }

    pub fn now_playing(&self) -> Result<Option<ContentHash>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.now_playing(),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::NowPlaying)?;
                if let ResponseData::NowPlaying(hash) = data {
                    Ok(hash)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }

    pub fn queue_next(&self, hash: ContentHash, source: String) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_next(hash, source),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::QueueNext { hash, source }),
        }
    }

    pub fn queue_append(&self, hash: ContentHash, source: String) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_append(hash, source),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::QueueAppend { hash, source }),
        }
    }

    pub fn queue_list(&self) -> Result<Vec<ContentHash>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_list(),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::QueueList)?;
                if let ResponseData::Queue(hashes) = data {
                    Ok(hashes)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }

    pub fn queue_clear(&self) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_clear(),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::QueueClear),
        }
    }

    pub fn queue_replace(&self, entries: Vec<(ContentHash, String)>) -> Result<(), ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_replace(entries),
            PlaybackBackend::Gateway(_) => self.gw_command(Command::QueueReplace { entries }),
        }
    }

    pub fn queue_remove(&self, hashes: Vec<ContentHash>) -> Result<usize, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.queue_remove(hashes),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::QueueRemove { hashes })?;
                if let ResponseData::Count(n) = data {
                    Ok(n)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }

    pub fn session(&self) -> Result<Option<String>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.get_session(),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::GetSession)?;
                if let ResponseData::Session(id) = data {
                    Ok(id)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }

    pub fn list_audio_outputs(&self) -> Result<Vec<AudioSinkInfo>, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.list_audio_outputs(),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::ListAudioOutputs)?;
                if let ResponseData::AudioOutputs(sinks) = data {
                    Ok(sinks)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }

    pub fn set_audio_output(&self, device_name: String) -> Result<AudioOutputConfig, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.set_audio_output(device_name),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::SetAudioOutput { device_name })?;
                if let ResponseData::AudioOutput(cfg) = data {
                    Ok(cfg)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }

    pub fn get_audio_output(&self) -> Result<AudioOutputConfig, ClientError> {
        match self {
            PlaybackBackend::Direct(c) => c.get_audio_output(),
            PlaybackBackend::Gateway(_) => {
                let data = self.gw_command_with_data(Command::GetAudioOutput)?;
                if let ResponseData::AudioOutput(cfg) = data {
                    Ok(cfg)
                } else {
                    Err(ClientError::Service(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
        }
    }
}
