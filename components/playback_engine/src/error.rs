use thiserror::Error;
use std::path::PathBuf;
use crate::Channel;

#[derive(Error, Debug)]
pub enum PlaybackError {
    #[error("Audio device error: {0}")]
    AudioDevice(String),
    
    #[error("Decoder error: {0}")]
    Decoder(String),
    
    #[error("Track not found: {0}")]
    TrackNotFound(PathBuf),
    
    #[error("Channel {0:?} already in use")]
    ChannelInUse(Channel),
    
    #[error("No track loaded on channel {0:?}")]
    NoTrackLoaded(Channel),
    
    #[error("Invalid volume: {0}dB")]
    InvalidVolume(f32),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}