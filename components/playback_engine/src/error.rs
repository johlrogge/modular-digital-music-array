use thiserror::Error;

#[derive(Error, Debug)]
pub enum PlaybackError {
    #[error("Audio device error: {0}")]
    AudioDevice(String),

    #[error("Decoder error: {0}")]
    Decoder(String),

    #[error("Track not found: {0}")]
    TrackNotFound(std::path::PathBuf),

    #[error("Channel {0:?} already in use")]
    ChannelInUse(crate::Deck),

    #[error("No track loaded on channel {0:?}")]
    NoTrackLoaded(crate::Deck),

    #[error("Invalid volume: {0}dB")]
    InvalidVolume(f32),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Task cancelled")]
    TaskCancelled,

    #[error("Track is not ready for playback")]
    TrackNotReady,
}
