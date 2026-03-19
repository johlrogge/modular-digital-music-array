//! In-memory fact store for tests and development.
//!
//! Stores facts as JSONL lines in a `Vec<String>` behind a `Mutex`.
//! The API is identical to the production `fact-store` component.

use acid_protocol::{cursor_from_offset, offset_from_cursor, FactEntry, StreamChunk};
use music_facts::{ContentHash, FactSource, MusicValue};
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FactStoreError {
    #[error("connection error: {0}")]
    Connection(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("service error: {0}")]
    Service(String),
}

impl From<serde_json::Error> for FactStoreError {
    fn from(err: serde_json::Error) -> Self {
        FactStoreError::Serialization(format!("{}", err))
    }
}

/// In-memory fact store for tests and development.
///
/// Facts are stored as JSONL strings (one per line), matching the format
/// used by the ACID service's append-only stream.
pub struct FactStore {
    lines: Mutex<Vec<String>>,
}

impl FactStore {
    /// Create a new in-memory fact store.
    ///
    /// The `_address` parameter is accepted for API compatibility with the
    /// production implementation but is ignored.
    pub fn connect(_address: &str) -> Result<Self, FactStoreError> {
        Ok(Self {
            lines: Mutex::new(Vec::new()),
        })
    }

    /// Write raw fact entries for an entity.
    ///
    /// Each entry is serialized as a JSONL line and appended to the in-memory store.
    pub fn write_facts(
        &self,
        entity: &str,
        facts: &[FactEntry],
    ) -> Result<usize, FactStoreError> {
        let mut lines = self.lines.lock().unwrap();
        let count = facts.len();

        for fact in facts {
            // Produce a JSONL line in the same format as ACID:
            // a stainless_facts::Fact serialized to JSON.
            let line = serde_json::json!({
                "entity": entity,
                "value": fact.value_json,
                "source": fact.source_json,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "operation": "Assert"
            });
            lines.push(serde_json::to_string(&line)?);
        }

        Ok(count)
    }

    /// Write music-domain facts, serializing `MusicValue` and `FactSource` to JSON.
    pub fn write_music_facts(
        &self,
        hash: &ContentHash,
        facts: &[(MusicValue, FactSource)],
    ) -> Result<usize, FactStoreError> {
        let entries: Vec<FactEntry> = facts
            .iter()
            .map(|(value, source)| -> Result<FactEntry, FactStoreError> {
                Ok(FactEntry {
                    value_json: serde_json::to_string(value)?,
                    source_json: serde_json::to_string(source)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.write_facts(hash.as_str(), &entries)
    }

    /// Read a paginated chunk of the in-memory fact stream.
    ///
    /// Pass `cursor: None` to start from the beginning.
    /// Pass the cursor from a previous `StreamChunk` to continue paging.
    pub fn read_stream(
        &self,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<StreamChunk, FactStoreError> {
        let lines = self.lines.lock().unwrap();
        let start = match cursor {
            Some(ref c) => offset_from_cursor(c).unwrap_or(0),
            None => 0,
        };

        let chunk_lines: Vec<String> = lines.iter().skip(start).take(limit).cloned().collect();
        let new_offset = start + chunk_lines.len();

        Ok(StreamChunk {
            lines: chunk_lines,
            cursor: cursor_from_offset(new_offset),
        })
    }

    /// Ping -- always succeeds for the in-memory store.
    pub fn ping(&self) -> Result<(), FactStoreError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_facts::{Artist, FactOrigin, Title};

    #[test]
    fn connect_always_succeeds() {
        let store = FactStore::connect("ipc:///nonexistent").unwrap();
        store.ping().unwrap();
    }

    #[test]
    fn write_and_read_roundtrip() {
        let store = FactStore::connect("").unwrap();

        let facts = vec![FactEntry {
            value_json: r#"{"Title":"Test"}"#.to_string(),
            source_json: r#"{"name":"test"}"#.to_string(),
        }];

        let written = store.write_facts("entity:1", &facts).unwrap();
        assert_eq!(written, 1);

        let chunk = store.read_stream(None, 100).unwrap();
        assert_eq!(chunk.lines.len(), 1);
        assert!(chunk.lines[0].contains("entity:1"));
    }

    #[test]
    fn read_stream_pagination() {
        let store = FactStore::connect("").unwrap();

        // Write 5 facts
        for i in 0..5 {
            let facts = vec![FactEntry {
                value_json: format!("\"fact_{}\"", i),
                source_json: r#""test""#.to_string(),
            }];
            store.write_facts(&format!("entity:{}", i), &facts).unwrap();
        }

        // Read first 2
        let chunk1 = store.read_stream(None, 2).unwrap();
        assert_eq!(chunk1.lines.len(), 2);

        // Read next 2
        let chunk2 = store.read_stream(Some(chunk1.cursor), 2).unwrap();
        assert_eq!(chunk2.lines.len(), 2);

        // Read remaining
        let chunk3 = store.read_stream(Some(chunk2.cursor), 2).unwrap();
        assert_eq!(chunk3.lines.len(), 1);

        // No more
        let chunk4 = store.read_stream(Some(chunk3.cursor), 2).unwrap();
        assert_eq!(chunk4.lines.len(), 0);
    }

    #[test]
    fn write_music_facts_roundtrip() {
        let store = FactStore::connect("").unwrap();

        let hash = ContentHash::new("sha256:abc123");
        let source = FactSource::new("test", "0.1.0", FactOrigin::Unknown);
        let facts = vec![
            (MusicValue::Title(Title::new("Test Track")), source.clone()),
            (
                MusicValue::Artist(Artist::new("Test Artist")),
                source.clone(),
            ),
        ];

        let written = store.write_music_facts(&hash, &facts).unwrap();
        assert_eq!(written, 2);

        let chunk = store.read_stream(None, 100).unwrap();
        assert_eq!(chunk.lines.len(), 2);
    }
}
