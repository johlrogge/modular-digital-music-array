//! In-memory fact storage — server-side implementation for tests and development.
mod storage;
pub use acid_protocol::{FactEntry, StreamChunk};
pub use storage::{FactStorage, StorageError};
