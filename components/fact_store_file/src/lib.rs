//! File-backed fact storage — server-side production implementation.
mod storage;
pub use acid_protocol::{FactEntry, StreamChunk};
pub use storage::{FactStorage, StorageError};
