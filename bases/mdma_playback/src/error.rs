use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("NNG error: {0}")]
    Nng(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Playback error: {0}")]
    Playback(#[from] playback_engine::PlaybackError),
}

impl From<(nng::Message, nng::Error)> for ServerError {
    fn from(err: (nng::Message, nng::Error)) -> Self {
        ServerError::Nng(err.1.to_string())
    }
}

impl From<nng::Error> for ServerError {
    fn from(err: nng::Error) -> Self {
        ServerError::Nng(err.to_string())
    }
}
