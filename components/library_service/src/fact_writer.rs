//! Fact stream writer for persisting track metadata
//!
//! Uses stainless-facts for append-only fact storage

use chrono::Utc;
use music_facts::{ContentHash, FactSource, MusicValue};
use std::path::Path;
use thiserror::Error;

// Re-export for convenience
pub use stainless_facts::{Fact, Operation};

use stainless_facts::FactStreamWriter;

#[derive(Debug, Error)]
pub enum FactWriteError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Write error: {0}")]
    Write(#[from] stainless_facts::WriteError),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// Writer for the music fact stream
pub struct FactWriter {
    writer: FactStreamWriter,
    facts_written: usize,
}

impl FactWriter {
    /// Open or create a fact stream file
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FactWriteError> {
        let writer = FactStreamWriter::open(path)?;

        Ok(Self {
            writer,
            facts_written: 0,
        })
    }

    /// Write facts for a newly indexed track
    ///
    /// Converts (MusicValue, FactSource) pairs into Fact structs and writes them
    pub fn write_track_facts(
        &mut self,
        content_hash: &ContentHash,
        facts: &[(MusicValue, FactSource)],
    ) -> Result<(), FactWriteError> {
        let now = Utc::now();

        let fact_structs: Vec<Fact<ContentHash, MusicValue, FactSource>> = facts
            .iter()
            .map(|(value, source)| {
                Fact::new(
                    content_hash.clone(),
                    value.clone(),
                    now,
                    source.clone(),
                    Operation::Assert,
                )
            })
            .collect();

        self.writer.write_batch(&fact_structs)?;

        self.facts_written += fact_structs.len();
        Ok(())
    }

    /// Get count of facts written
    pub fn facts_written(&self) -> usize {
        self.facts_written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_facts::{Artist, FactOrigin, Title};
    use tempfile::NamedTempFile;

    #[test]
    fn write_track_facts() {
        let temp = NamedTempFile::new().unwrap();
        let content_hash = ContentHash::new("sha256:abc123");
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);

        let facts = vec![
            (MusicValue::Title(Title::new("Test Track")), source.clone()),
            (
                MusicValue::Artist(Artist::new("Test Artist")),
                source.clone(),
            ),
        ];

        let mut writer = FactWriter::open(temp.path()).unwrap();
        writer.write_track_facts(&content_hash, &facts).unwrap();

        assert_eq!(writer.facts_written(), 2);

        // Verify file has content
        let metadata = std::fs::metadata(temp.path()).unwrap();
        assert!(metadata.len() > 0);
    }
}
