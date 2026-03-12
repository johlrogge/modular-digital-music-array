use thiserror::Error;

#[derive(Error, Debug)]
pub enum PlaybackError {
    #[error("Audio device error: {0}")]
    AudioDevice(String),

    #[error("Decoder error: {0}")]
    Decoder(String),

    #[error("No track loaded")]
    NoTrackLoaded,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Task cancelled")]
    TaskCancelled,

    #[error("Resampler error: {0}")]
    Resampler(String),
}
