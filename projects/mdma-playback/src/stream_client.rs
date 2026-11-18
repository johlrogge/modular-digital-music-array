use media_protocol::ContentHash;
use nng::options::{Options, RecvTimeout, SendTimeout};
use nng::{Protocol, Socket};
use playback_primitives::PositionMs;
use std::time::Duration;
use stream_source_protocol::{
    AudioOutputConfig, AudioSinkInfo, OutputHealth, StreamCommand, StreamResponse, StreamTrackInfo,
};

use crate::error::ServerError;

const TIMEOUT: Duration = Duration::from_secs(5);

pub struct StreamClient {
    socket: Socket,
}

impl StreamClient {
    pub fn connect(addr: &str) -> Result<Self, ServerError> {
        let socket = Socket::new(Protocol::Req0).map_err(ServerError::from_nng)?;
        socket
            .set_opt::<SendTimeout>(Some(TIMEOUT))
            .map_err(ServerError::from_nng)?;
        socket
            .set_opt::<RecvTimeout>(Some(TIMEOUT))
            .map_err(ServerError::from_nng)?;
        socket.dial(addr).map_err(ServerError::from_nng)?;
        Ok(Self { socket })
    }

    fn send(&self, cmd: &StreamCommand) -> Result<StreamResponse, ServerError> {
        let bytes = serde_json::to_vec(cmd).map_err(ServerError::from_json)?;
        self.socket
            .send(bytes.as_slice())
            .map_err(|(_, e)| ServerError::from_nng(e))?;
        let msg = self.socket.recv().map_err(ServerError::from_nng)?;
        serde_json::from_slice(&msg).map_err(ServerError::from_json)
    }

    /// Send a command whose only valid success reply is `Ok`.
    fn expect_ok(&self, cmd: &StreamCommand) -> Result<(), ServerError> {
        match self.send(cmd)? {
            StreamResponse::Ok => Ok(()),
            StreamResponse::Error { message } => Err(ServerError::stream(message)),
            other => Err(ServerError::stream(format!("unexpected: {other:?}"))),
        }
    }

    pub fn load(&self, content_hash: ContentHash) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::Load { content_hash })
    }

    pub fn preload(&self, content_hash: ContentHash) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::Preload { content_hash })
    }

    pub fn clear_preload(&self) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::ClearPreload)
    }

    pub fn play(&self) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::Play)
    }

    /// Move the audible track; its play state is unchanged. An older source
    /// answers "invalid command", surfaced as a stream error.
    pub fn seek(&self, position: PositionMs) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::Seek {
            position_ms: position,
        })
    }

    pub fn stop(&self) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::Stop)
    }

    pub fn pause(&self) -> Result<(), ServerError> {
        self.expect_ok(&StreamCommand::Pause)
    }

    pub fn loaded(&self) -> Result<Option<StreamTrackInfo>, ServerError> {
        self.loaded_with_output().map(|(info, _)| info)
    }

    /// `Loaded` with the source's output health (`None` from an old source).
    pub fn loaded_with_output(
        &self,
    ) -> Result<(Option<StreamTrackInfo>, Option<OutputHealth>), ServerError> {
        match self.send(&StreamCommand::Loaded)? {
            StreamResponse::Loaded { info, output } => Ok((info, output)),
            StreamResponse::Error { message } => Err(ServerError::stream(message)),
            other => Err(ServerError::stream(format!("unexpected: {other:?}"))),
        }
    }

    pub fn list_outputs(&self) -> Result<Vec<AudioSinkInfo>, ServerError> {
        match self.send(&StreamCommand::ListOutputs)? {
            StreamResponse::Outputs { sinks } => Ok(sinks),
            StreamResponse::Error { message } => Err(ServerError::stream(message)),
            other => Err(ServerError::stream(format!("unexpected: {other:?}"))),
        }
    }

    pub fn set_output(&self, device_name: String) -> Result<AudioOutputConfig, ServerError> {
        match self.send(&StreamCommand::SetOutput { device_name })? {
            StreamResponse::Output { config } => Ok(config),
            StreamResponse::Error { message } => Err(ServerError::stream(message)),
            other => Err(ServerError::stream(format!("unexpected: {other:?}"))),
        }
    }

    pub fn get_output(&self) -> Result<AudioOutputConfig, ServerError> {
        match self.send(&StreamCommand::GetOutput)? {
            StreamResponse::Output { config } => Ok(config),
            StreamResponse::Error { message } => Err(ServerError::stream(message)),
            other => Err(ServerError::stream(format!("unexpected: {other:?}"))),
        }
    }
}
