//! Beacon error types with rich context
//!
//! Error handling follows thiserror best practices:
//! - Include operation context
//! - Preserve source errors
//! - Provide actionable messages

use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BeaconError {
    #[error("failed to detect NVMe drives at {path}")]
    NvmeDetection {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("hardware problem: {0}")]
    Hardware(String),

    #[error("failed to read hardware information from {path}")]
    HardwareInfo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("validation failed: {0}")]
    Validation(#[from] crate::types::ValidationError),

    #[error("failed to partition device {device}: {reason}")]
    Partitioning { device: String, reason: String },

    #[error("failed to format partition {partition}: {reason}")]
    Formatting { partition: String, reason: String },

    #[error("failed to install base system: {0}")]
    Installation(String),

    #[error("safety check failed: {0}")]
    Safety(String),

    #[error("io error during {operation}")]
    Io {
        operation: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to provision: {0}")]
    Provisioning(String),

    #[error("command execution failed: {command}")]
    CommandFailed {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("command returned non-zero exit code: {command}\nStderr: {stderr}")]
    CommandExitCode { command: String, stderr: String },
}

/// Helper to create IO error with operation context
impl BeaconError {
    pub fn io(operation: impl Into<String>, source: std::io::Error) -> Self {
        BeaconError::Io {
            operation: operation.into(),
            source,
        }
    }

    pub fn nvme_detection(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        BeaconError::NvmeDetection {
            path: path.into(),
            source,
        }
    }

    pub fn hardware_info(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        BeaconError::HardwareInfo {
            path: path.into(),
            source,
        }
    }

    pub fn command_failed(command: impl Into<String>, source: std::io::Error) -> Self {
        BeaconError::CommandFailed {
            command: command.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, BeaconError>;
