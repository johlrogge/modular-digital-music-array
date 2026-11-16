mod protocol;

pub use playback_primitives::{
    AudioOutputConfig, AudioSinkInfo, CanonicalHash, ContentHash, Deck, NotCanonical, SourceName,
};
pub use protocol::{require_canonical, Command, Response, ResponseData};
