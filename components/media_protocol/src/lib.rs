mod error;
mod protocol;

pub use error::ClientError;
pub use playback_primitives::Deck;
pub use protocol::{Command, Response};
