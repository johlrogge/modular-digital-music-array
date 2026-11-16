mod protocol;

pub use playback_primitives::{
    AudioOutputConfig, AudioSinkInfo, CanonicalHash, ContentHash, Deck, NotCanonical, OutputHealth,
    SourceName,
};
pub use protocol::{require_canonical, Command, Response, ResponseData};
