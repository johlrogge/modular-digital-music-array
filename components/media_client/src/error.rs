use thiserror::Error;

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Command failed: {0}")]
    Command(String),

    #[error("Protocol error: {0}")]
    Protocol(String),
}
