use crate::Channel;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    LoadTrack { path: PathBuf, channel: Channel },
    Play { channel: Channel },
    Stop { channel: Channel },
    SetVolume { channel: Channel, db: f32 },
    Unload { channel: Channel },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub success: bool,
    pub error_message: String,
}
