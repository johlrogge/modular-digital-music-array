use thiserror::Error;

#[derive(Error, Debug)]
pub enum FingerprintError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Audio decoding error: {0}")]
    AudioDecode(String),

    #[error("Fingerprint generation failed: {0}")]
    Generation(String),

    #[error("Invalid audio format: {0}")]
    InvalidFormat(String),

    #[error("AcoustID API error: {0}")]
    AcoustIdApi(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Invalid fingerprint data")]
    InvalidFingerprint,
}
