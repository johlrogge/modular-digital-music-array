//! MDMA Client library — reusable backend abstraction for CLI, TUI, BDD, and other clients.
//!
//! Provides gateway-or-direct transport for library, playback, and source services.

mod client_config;
mod error;
pub mod library;
pub mod node;
pub mod playback;
mod queue_resolve;
pub mod source;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use client_config::ClientConfig;
pub use error::{source_label, upgrade_node_text};
pub use library::LibraryBackend;
pub use node::{Node, NodeError, NodeOptions};
pub use node_discovery::{NodeSpec, RemoteTarget, Source, Target};
pub use playback::PlaybackBackend;
pub use queue_resolve::resolve_in_queue;
pub use source::{list_available_sources, SourceClient};

// Re-export key types so clients don't need to depend on individual protocol crates.
pub use library_ipc_client::{
    entries_for_playback, entry_tracks, track_hashes, unresolved_entries, CanonicalHash,
    ClientError as LibraryClientError, ContentHash, EntryError, EntryTrackError, InboxPath,
    IngestAllItem, IngestResult, IngestSource, LibraryRequest, LibraryResponse, Mark, MarkFilter,
    MarkNameError, MarkWrite, PlayableEntries, PlaylistEntry, PlaylistName, ProtocolError,
    ResolveError, ServiceStatus, TrackInfo, Unmark, UnplayableEntry,
};
pub use library_search::TrackQuery;
pub use media_client::{
    AudioOutputConfig, AudioSinkInfo, ClientError as PlaybackClientError, Command, Deck, Response,
    ResponseData, SourceName,
};
pub use source_protocol::{SourceRequest, SourceResponse};
