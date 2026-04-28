//! File-backed FactStorage for the ACID service (production).
use acid_protocol::{cursor_from_offset, FactEntry, StreamChunk};
use chrono::Utc;
use stainless_facts::{Fact, FactStreamWriter, Operation};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("fact write error: {0}")]
    FactWrite(#[from] stainless_facts::WriteError),
}

/// Return true if the JSONL line belongs to `entity`.
///
/// Facts are stored in array format `[entity, ...]`. Parse just the first element.
fn is_entity_match(line: &str, entity: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    v.get(0)
        .and_then(|e| e.as_str())
        .map(|e| e == entity)
        .unwrap_or(false)
}

/// File-backed storage for the ACID service. Writes JSONL to `metadata_dir/facts.jsonl`.
pub struct FactStorage {
    metadata_dir: PathBuf,
}

impl FactStorage {
    pub fn new(metadata_dir: &Path) -> Result<Self, StorageError> {
        std::fs::create_dir_all(metadata_dir)?;
        Ok(Self {
            metadata_dir: metadata_dir.to_path_buf(),
        })
    }

    pub fn write_facts(&self, entity: &str, facts: &[FactEntry]) -> Result<usize, StorageError> {
        let facts_path = self.metadata_dir.join("facts.jsonl");
        let now = Utc::now();
        let fact_structs: Vec<Fact<String, serde_json::Value, serde_json::Value>> = facts
            .iter()
            .map(|entry| {
                let value: serde_json::Value = serde_json::from_str(&entry.value_json)?;
                let source: serde_json::Value = serde_json::from_str(&entry.source_json)?;
                Ok(Fact::new(
                    entity.to_string(),
                    value,
                    now,
                    source,
                    Operation::Assert,
                ))
            })
            .collect::<Result<_, serde_json::Error>>()?;
        let count = fact_structs.len();
        let mut writer = FactStreamWriter::open(&facts_path)?;
        writer.write_batch(&fact_structs)?;
        Ok(count)
    }

    pub fn read_stream(
        &self,
        after_line: usize,
        limit: usize,
    ) -> Result<StreamChunk, StorageError> {
        let facts_path = self.metadata_dir.join("facts.jsonl");
        if !facts_path.exists() {
            return Ok(StreamChunk {
                lines: vec![],
                cursor: cursor_from_offset(after_line),
            });
        }
        let file = std::fs::File::open(&facts_path)?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader
            .lines()
            .skip(after_line)
            .take(limit)
            .collect::<Result<_, _>>()?;
        let next_offset = after_line + lines.len();
        Ok(StreamChunk {
            lines,
            cursor: cursor_from_offset(next_offset),
        })
    }

    /// Append retraction facts for `entity`. Returns the count of retractions written.
    pub fn retract_facts(
        &self,
        entity: &str,
        facts: &[FactEntry],
    ) -> Result<usize, StorageError> {
        let facts_path = self.metadata_dir.join("facts.jsonl");
        let now = Utc::now();
        let fact_structs: Vec<Fact<String, serde_json::Value, serde_json::Value>> = facts
            .iter()
            .map(|entry| {
                let value: serde_json::Value = serde_json::from_str(&entry.value_json)?;
                let source: serde_json::Value = serde_json::from_str(&entry.source_json)?;
                Ok(Fact::new(
                    entity.to_string(),
                    value,
                    now,
                    source,
                    Operation::Retract,
                ))
            })
            .collect::<Result<_, serde_json::Error>>()?;
        let count = fact_structs.len();
        let mut writer = FactStreamWriter::open(&facts_path)?;
        writer.write_batch(&fact_structs)?;
        Ok(count)
    }

    /// Read all stored facts for a single entity. Returns matching JSONL lines.
    pub fn read_entity(&self, entity: &str) -> Result<Vec<String>, StorageError> {
        let facts_path = self.metadata_dir.join("facts.jsonl");
        if !facts_path.exists() {
            return Ok(vec![]);
        }
        let file = std::fs::File::open(&facts_path)?;
        let reader = BufReader::new(file);
        let mut matches = Vec::new();
        for line_result in reader.lines() {
            let line = line_result?;
            if is_entity_match(&line, entity) {
                matches.push(line);
            }
        }
        Ok(matches)
    }

    /// No-op for the file-backed implementation — this backend reads directly
    /// from disk on every `read_stream` call so there is nothing to replay.
    /// Exists for API symmetry with `fact_store_memory`.
    pub fn replay_from_file(&self, _path: &Path) -> io::Result<usize> {
        Ok(0)
    }

    pub fn line_count(&self) -> usize {
        let facts_path = self.metadata_dir.join("facts.jsonl");
        if !facts_path.exists() {
            return 0;
        }
        match std::fs::File::open(&facts_path) {
            Ok(file) => BufReader::new(file).lines().count(),
            Err(_) => 0,
        }
    }

    /// Returns the name of this storage backend for logging purposes.
    pub fn backend_name(&self) -> &'static str {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn backend_name_returns_file() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();
        assert_eq!(storage.backend_name(), "file");
    }

    #[test]
    fn new_creates_metadata_dir() {
        let base = temp_dir();
        let meta = base.path().join("metadata");
        let storage = FactStorage::new(&meta).unwrap();
        assert!(meta.exists());
        assert_eq!(storage.line_count(), 0);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        let facts = vec![FactEntry {
            value_json: r#"{"bpm": 128}"#.to_string(),
            source_json: r#"{"source": "test"}"#.to_string(),
        }];

        let written = storage.write_facts("track:test", &facts).unwrap();
        assert_eq!(written, 1);

        let chunk = storage.read_stream(0, 100).unwrap();
        assert_eq!(chunk.lines.len(), 1);
    }

    #[test]
    fn read_stream_missing_file_returns_empty() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        let chunk = storage.read_stream(0, 10).unwrap();
        assert!(chunk.lines.is_empty());
        assert_eq!(acid_protocol::offset_from_cursor(&chunk.cursor), Some(0));
    }

    #[test]
    fn read_stream_with_offset() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        for i in 0..3 {
            storage
                .write_facts(
                    &format!("entity:{i}"),
                    &[FactEntry {
                        value_json: format!("\"fact_{i}\""),
                        source_json: r#""src""#.to_string(),
                    }],
                )
                .unwrap();
        }

        let chunk = storage.read_stream(1, 10).unwrap();
        assert_eq!(chunk.lines.len(), 2);
    }

    #[test]
    fn replay_from_file_is_noop_returns_zero() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();
        let result = storage.replay_from_file(&dir.path().join("facts.jsonl"));
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn retract_facts_appends_retract_operation() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        let facts = vec![FactEntry {
            value_json: r#"{"bpm": 128}"#.to_string(),
            source_json: r#"{"source": "test"}"#.to_string(),
        }];

        let count = storage.retract_facts("track:test", &facts).unwrap();
        assert_eq!(count, 1);
        assert_eq!(storage.line_count(), 1);

        // The stored line must carry "Retract" operation
        let chunk = storage.read_stream(0, 100).unwrap();
        assert_eq!(chunk.lines.len(), 1);
        assert!(
            chunk.lines[0].contains("Retract"),
            "expected Retract in line, got: {}",
            chunk.lines[0]
        );
    }

    #[test]
    fn read_entity_returns_matching_lines() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        storage
            .write_facts(
                "entity:alpha",
                &[FactEntry {
                    value_json: r#""v1""#.to_string(),
                    source_json: r#""s""#.to_string(),
                }],
            )
            .unwrap();
        storage
            .write_facts(
                "entity:beta",
                &[FactEntry {
                    value_json: r#""v2""#.to_string(),
                    source_json: r#""s""#.to_string(),
                }],
            )
            .unwrap();
        storage
            .write_facts(
                "entity:alpha",
                &[FactEntry {
                    value_json: r#""v3""#.to_string(),
                    source_json: r#""s""#.to_string(),
                }],
            )
            .unwrap();

        let lines = storage.read_entity("entity:alpha").unwrap();
        assert_eq!(lines.len(), 2, "expected 2 lines for entity:alpha");
        for line in &lines {
            assert!(
                line.contains("entity:alpha"),
                "line should contain entity:alpha, got: {line}"
            );
        }
    }

    #[test]
    fn read_entity_mixed_entities_filtered_correctly() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        for i in 0..5 {
            storage
                .write_facts(
                    &format!("entity:{i}"),
                    &[FactEntry {
                        value_json: format!("\"fact_{i}\""),
                        source_json: r#""src""#.to_string(),
                    }],
                )
                .unwrap();
        }

        let lines = storage.read_entity("entity:3").unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("entity:3"));
    }

    #[test]
    fn read_entity_missing_file_returns_empty() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();
        let lines = storage.read_entity("entity:none").unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn line_count_reflects_writes() {
        let dir = temp_dir();
        let storage = FactStorage::new(dir.path()).unwrap();

        assert_eq!(storage.line_count(), 0);

        storage
            .write_facts(
                "entity:1",
                &[
                    FactEntry {
                        value_json: r#""v1""#.to_string(),
                        source_json: r#""s""#.to_string(),
                    },
                    FactEntry {
                        value_json: r#""v2""#.to_string(),
                        source_json: r#""s""#.to_string(),
                    },
                ],
            )
            .unwrap();

        assert_eq!(storage.line_count(), 2);
    }
}
