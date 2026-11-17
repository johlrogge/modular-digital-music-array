use thiserror::Error;

#[derive(Error, Debug)]
pub enum PlaybackError {
    #[error(transparent)]
    AudioOutput(#[from] audio_output::AudioOutputError),

    #[error(transparent)]
    Decoder(#[from] audio_decoder::DecoderError),

    /// PipeWire could not be started or did not come up in time.
    #[error("No audio output: {0}")]
    NoAudioOutput(#[source] audio_output::AudioOutputError),

    /// A source did not say how its samples are laid out, so a time in it
    /// cannot be turned into a sample index.
    #[error("The source reports no {0}")]
    NoFormat(&'static str),

    #[error("No track loaded")]
    NoTrackLoaded,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Task cancelled")]
    TaskCancelled,

    #[error(transparent)]
    Resampler(#[from] audio_resampler::ResamplerError),
}
