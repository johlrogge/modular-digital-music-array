mod error;
mod protocol;

pub use error::ClientError;
pub use playback_primitives::Channel;
pub use protocol::{Command, Response};
