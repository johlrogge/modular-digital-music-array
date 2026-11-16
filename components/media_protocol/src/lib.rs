mod protocol;

pub use playback_primitives::{
    AudioOutputConfig, AudioSinkInfo, ContentHash, Deck, OutputHealth, SourceName,
};
pub use protocol::{Command, Response, ResponseData};
