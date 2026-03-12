use thiserror::Error;

/// Application-level error type. Used by future tasks for typed error propagation.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum TuiError {
    #[error("Library error: {0}")]
    Library(#[from] mdma_client::LibraryClientError),
    #[error("Playback error: {0}")]
    Playback(String),
    #[error("Event error: {0}")]
    EventParse(String),
}
