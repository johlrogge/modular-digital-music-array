use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("NNG error: {0}")]
    Nng(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Stream source error: {0}")]
    Stream(String),
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

impl ServerError {
    pub fn from_nng(e: nng::Error) -> Self {
        ServerError::Nng(e.to_string())
    }

    pub fn from_json(e: serde_json::Error) -> Self {
        ServerError::Json(e)
    }

    pub fn stream(msg: impl Into<String>) -> Self {
        ServerError::Stream(msg.into())
    }
}
