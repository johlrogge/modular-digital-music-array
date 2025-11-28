// bases/library_crawler/src/fact_writer.rs
use color_eyre::Result;
use std::path::Path;

// Re-export stainless-facts types
pub use stainless_facts::{Fact, Operation};

// Import what we need
use music_facts::{ContentHash, FactSource, MusicValue};
use stainless_facts::FactStreamWriter;

/// Writer for fact streams
///
/// This is a simple wrapper around FactStreamWriter from stainless-facts
pub struct FactWriter {
    writer: FactStreamWriter,
    facts_written: usize,
}

impl FactWriter {
    /// Create a new fact writer
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let writer = FactStreamWriter::open(path)?;

        Ok(Self {
            writer,
            facts_written: 0,
        })
    }

    /// Write a single fact
    pub fn write_fact(&mut self, fact: &Fact<ContentHash, MusicValue, FactSource>) -> Result<()> {
        self.writer.write_batch(&[fact.clone()])?;
        self.facts_written += 1;
        Ok(())
    }

    /// Write multiple facts
    pub fn write_facts(
        &mut self,
        facts: &[Fact<ContentHash, MusicValue, FactSource>],
    ) -> Result<()> {
        self.writer.write_batch(facts)?;
        self.facts_written += facts.len();
        Ok(())
    }

    /// Flush writer (no-op, FactStreamWriter handles this automatically)
    pub fn flush(&mut self) -> Result<()> {
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
    use chrono::Utc;
    use music_facts::FactOrigin;
    use tempfile::NamedTempFile;

    #[test]
    fn write_and_verify_facts() {
        let temp = NamedTempFile::new().unwrap();
        let content_hash = ContentHash("test_hash".to_string());
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let now = Utc::now();

        let fact = Fact::new(
            content_hash,
            MusicValue::Title("Test Track".to_string()),
            now,
            source,
            Operation::Assert,
        );

        let mut writer = FactWriter::create(temp.path()).unwrap();
        writer.write_fact(&fact).unwrap();
        writer.flush().unwrap();

        assert_eq!(writer.facts_written(), 1);

        // Verify file was created and has content
        let metadata = std::fs::metadata(temp.path()).unwrap();
        assert!(metadata.len() > 0);
    }
}
