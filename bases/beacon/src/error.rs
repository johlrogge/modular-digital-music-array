// bases/beacon/src/error.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BeaconError {
    #[error("failed to detect NVMe drives: {0}")]
    NvmeDetection(String),

    #[error("Problem with hardware: {0}")]
    Hardware(String),

    #[error("failed to read hardware information: {0}")]
    HardwareInfo(String),

    #[error("invalid hostname: {0}")]
    InvalidHostname(String),

    #[error("invalid SSH public key: {0}")]
    InvalidSshKey(String),

    #[error("failed to partition device {device}: {reason}")]
    Partitioning { device: String, reason: String },

    #[error("failed to format partition {partition}: {reason}")]
    Formatting { partition: String, reason: String },

    #[error("failed to install base system: {0}")]
    Installation(String),

    #[error("safety check failed: {0}")]
    Safety(String),

    #[error("io error")]
    Io(#[from] std::io::Error),

    #[error("failed to provision: {0}")]
    Provisioning(String),

    #[error("failed to validate: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, BeaconError>;
