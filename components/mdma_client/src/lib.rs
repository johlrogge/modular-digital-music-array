//! MDMA Client library — reusable backend abstraction for CLI, TUI, BDD, and other clients.
//!
//! Provides gateway-or-direct transport for library, playback, and source services.

mod error;
pub mod library;
pub mod playback;
pub mod source;

pub use library::LibraryBackend;
pub use playback::PlaybackBackend;
pub use source::{list_available_sources, SourceClient};

// Re-export key types so clients don't need to depend on individual protocol crates.
pub use library_ipc_client::{
    ClientError as LibraryClientError, ContentHash, InboxPath, IngestAllItem, IngestResult,
    IngestSource, LibraryRequest, LibraryResponse, PlaylistName, ProtocolError, ServiceStatus,
    TrackInfo,
};
pub use library_search::TrackQuery;
pub use media_client::{
    AudioOutputConfig, AudioSinkInfo, ClientError as PlaybackClientError, Command, Deck, Response,
    ResponseData,
};
pub use source_protocol::{SourceRequest, SourceResponse};
