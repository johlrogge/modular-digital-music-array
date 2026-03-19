//! In-memory FactStorage for ACID server.
use acid_protocol::{cursor_from_offset, FactEntry, StreamChunk};
use chrono::Utc;
use stainless_facts::{Fact, Operation};
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// In-memory storage for the ACID service. Thread-safe.
pub struct FactStorage {
    lines: Mutex<Vec<String>>,
}

impl FactStorage {
    /// Create a new in-memory storage. `metadata_dir` is accepted for API
    /// compatibility with the file-backed implementation but is ignored.
    pub fn new(_metadata_dir: &Path) -> Result<Self, StorageError> {
        Ok(Self {
            lines: Mutex::new(Vec::new()),
        })
    }

    /// Append facts for `entity`. Returns the count of facts written.
    pub fn write_facts(&self, entity: &str, facts: &[FactEntry]) -> Result<usize, StorageError> {
        let mut lines = self.lines.lock().unwrap();
        let now = Utc::now();
        let count = facts.len();
        for fact in facts {
            let value: serde_json::Value = serde_json::from_str(&fact.value_json)?;
            let source: serde_json::Value = serde_json::from_str(&fact.source_json)?;
            let fact_struct: Fact<String, serde_json::Value, serde_json::Value> =
                Fact::new(entity.to_string(), value, now, source, Operation::Assert);
            lines.push(serde_json::to_string(&fact_struct)?);
        }
        Ok(count)
    }

    /// Read a paginated chunk. `after_line` is a 0-based offset; `limit` is max lines.
    pub fn read_stream(
        &self,
        after_line: usize,
        limit: usize,
    ) -> Result<StreamChunk, StorageError> {
        let lines = self.lines.lock().unwrap();
        let chunk_lines: Vec<String> = lines.iter().skip(after_line).take(limit).cloned().collect();
        let new_offset = after_line + chunk_lines.len();
        Ok(StreamChunk {
            lines: chunk_lines,
            cursor: cursor_from_offset(new_offset),
        })
    }

    /// Return the total number of stored lines (for cursor initialisation).
    pub fn line_count(&self) -> usize {
        self.lines.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_protocol::offset_from_cursor;

    #[test]
    fn new_creates_empty_storage() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();
        assert_eq!(storage.line_count(), 0);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();

        let facts = vec![FactEntry {
            value_json: r#"{"Title":"Test"}"#.to_string(),
            source_json: r#"{"name":"test"}"#.to_string(),
        }];

        let written = storage.write_facts("entity:1", &facts).unwrap();
        assert_eq!(written, 1);

        let chunk = storage.read_stream(0, 100).unwrap();
        assert_eq!(chunk.lines.len(), 1);
        assert!(chunk.lines[0].contains("entity:1"));
    }

    #[test]
    fn read_stream_pagination() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();

        for i in 0..5 {
            let facts = vec![FactEntry {
                value_json: format!("\"fact_{}\"", i),
                source_json: r#""test""#.to_string(),
            }];
            storage
                .write_facts(&format!("entity:{}", i), &facts)
                .unwrap();
        }

        let chunk1 = storage.read_stream(0, 2).unwrap();
        assert_eq!(chunk1.lines.len(), 2);

        let offset1 = offset_from_cursor(&chunk1.cursor).unwrap();
        let chunk2 = storage.read_stream(offset1, 2).unwrap();
        assert_eq!(chunk2.lines.len(), 2);

        let offset2 = offset_from_cursor(&chunk2.cursor).unwrap();
        let chunk3 = storage.read_stream(offset2, 2).unwrap();
        assert_eq!(chunk3.lines.len(), 1);

        let offset3 = offset_from_cursor(&chunk3.cursor).unwrap();
        let chunk4 = storage.read_stream(offset3, 2).unwrap();
        assert_eq!(chunk4.lines.len(), 0);
    }

    #[test]
    fn line_count_reflects_writes() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();

        let facts = vec![
            FactEntry {
                value_json: r#""fact1""#.to_string(),
                source_json: r#""src""#.to_string(),
            },
            FactEntry {
                value_json: r#""fact2""#.to_string(),
                source_json: r#""src""#.to_string(),
            },
        ];

        storage.write_facts("entity:1", &facts).unwrap();
        assert_eq!(storage.line_count(), 2);
    }

    #[test]
    fn write_facts_returns_error_on_bad_value_json() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();
        let facts = vec![FactEntry {
            value_json: "not valid json{{".to_string(),
            source_json: r#""src""#.to_string(),
        }];
        let result = storage.write_facts("entity:1", &facts);
        assert!(result.is_err(), "expected error on bad value_json, got Ok");
    }

    #[test]
    fn write_facts_returns_error_on_bad_source_json() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();
        let facts = vec![FactEntry {
            value_json: r#""val""#.to_string(),
            source_json: "not valid json{{".to_string(),
        }];
        let result = storage.write_facts("entity:1", &facts);
        assert!(result.is_err(), "expected error on bad source_json, got Ok");
    }

    /// Verify that the memory backend serializes facts in array (tuple) format,
    /// matching the output of `fact_store_file` via `FactStreamWriter`.
    ///
    /// Expected format: `["entity","<timestamp>",<value_obj>,<source_obj>,"Assert"]`
    /// Wrong format (old):
    ///   `{"entity":"...","value":{...},"timestamp":"...","source":{...},"operation":"Assert"}`
    #[test]
    fn write_facts_produces_array_format() {
        let storage = FactStorage::new(Path::new("/tmp")).unwrap();
        let facts = vec![FactEntry {
            value_json: r#"{"t":"Test Track"}"#.to_string(),
            source_json: r#"{"name":"analyser"}"#.to_string(),
        }];

        storage.write_facts("sha256:deadbeef", &facts).unwrap();

        let chunk = storage.read_stream(0, 10).unwrap();
        assert_eq!(chunk.lines.len(), 1);

        let line = &chunk.lines[0];

        // The line must be a JSON array — if it is an object this is the old broken format
        let first_char = line.trim_start().chars().next().unwrap_or(' ');
        assert_eq!(
            first_char, '[',
            "memory backend must produce array-format fact lines (matching fact_store_file), \
             but got: {line}"
        );

        // It must contain the entity string
        assert!(
            line.contains("sha256:deadbeef"),
            "serialized line must contain the entity, got: {line}"
        );
    }
}
