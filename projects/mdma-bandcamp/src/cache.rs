//! Download cache for tracking what has been downloaded
//!
//! Uses a track-oriented cache to properly handle partial album purchases.
//! Cache key: {artist}|{album}|{track_name}|{duration_seconds}
//!
//! This handles the case where a user buys track 1 from an album, downloads it,
//! then later buys track 2 from the same album - the cache will recognize that
//! track 2 is new and needs downloading.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Track-oriented download cache
pub struct DownloadCache {
    path: PathBuf,
    /// Set of track keys that have been downloaded
    downloaded: HashSet<String>,
    /// Set of source item IDs that have been downloaded
    downloaded_item_ids: HashSet<String>,
}

impl DownloadCache {
    /// Open or create a cache file
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let (downloaded, downloaded_item_ids) = if path.exists() {
            Self::load_entries(path)?
        } else {
            (HashSet::new(), HashSet::new())
        };

        tracing::info!(path = %path.display(), entries = downloaded.len(), "Loaded download cache");

        Ok(Self {
            path: path.to_path_buf(),
            downloaded,
            downloaded_item_ids,
        })
    }

    /// Load entries from cache file
    fn load_entries(path: &Path) -> Result<(HashSet<String>, HashSet<String>), CacheError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = HashSet::new();
        let mut item_ids = HashSet::new();

        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Format: artist|album|track|duration|source_item_id
            let parts: Vec<&str> = line.splitn(5, '|').collect();
            if parts.len() >= 4 {
                let track_key = format!("{}|{}|{}|{}", parts[0], parts[1], parts[2], parts[3]);
                entries.insert(track_key);
                // Also index by source item ID (5th field)
                if parts.len() >= 5 && !parts[4].is_empty() {
                    item_ids.insert(parts[4].to_string());
                }
            } else {
                tracing::warn!(line = %line, "Invalid cache entry, skipping");
            }
        }

        Ok((entries, item_ids))
    }

    /// Check if a track has been downloaded (by track key)
    pub fn is_downloaded(&self, key: &str) -> bool {
        self.downloaded.contains(key)
    }

    /// Check if an item ID has been downloaded
    pub fn is_item_downloaded(&self, item_id: &str) -> bool {
        self.downloaded_item_ids.contains(item_id)
    }

    /// Mark a track as downloaded
    pub fn mark_downloaded(&mut self, key: &str, source_item_id: &str) -> Result<(), CacheError> {
        if self.downloaded.contains(key) {
            return Ok(());
        }

        // Append to file
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        writeln!(file, "{}|{}", key, source_item_id)?;

        // Add to in-memory sets
        self.downloaded.insert(key.to_string());
        if !source_item_id.is_empty() {
            self.downloaded_item_ids.insert(source_item_id.to_string());
        }

        Ok(())
    }

    /// Remove all cache entries whose source item ID matches `item_id`.
    ///
    /// Rewrites the cache file atomically (write to temp + rename) excluding any
    /// line whose 5th `|`-separated field equals `item_id`. Also evicts matching
    /// entries from the in-memory `downloaded` and `downloaded_item_ids` sets.
    ///
    /// Calling this with an `item_id` that is not present in the cache is a no-op
    /// and does not return an error.
    pub fn forget_item(&mut self, item_id: &str) -> Result<(), CacheError> {
        if !self.downloaded_item_ids.contains(item_id) {
            return Ok(());
        }

        // Read current file and rebuild without the target item_id
        let mut kept_lines: Vec<String> = Vec::new();
        let mut removed_track_keys: Vec<String> = Vec::new();

        if self.path.exists() {
            let file = File::open(&self.path)?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line?;
                let trimmed = line.trim();

                if trimmed.is_empty() || trimmed.starts_with('#') {
                    kept_lines.push(line);
                    continue;
                }

                let parts: Vec<&str> = trimmed.splitn(5, '|').collect();
                if parts.len() >= 5 && parts[4] == item_id {
                    // Record the track key so we can evict it from memory
                    let track_key = format!("{}|{}|{}|{}", parts[0], parts[1], parts[2], parts[3]);
                    removed_track_keys.push(track_key);
                    // Skip this line — it belongs to the forgotten item
                } else {
                    kept_lines.push(line);
                }
            }
        }

        // Write atomically: write to a sibling temp file then rename over the original
        let tmp_path = self.path.with_extension("tmp");
        {
            let mut tmp_file = File::create(&tmp_path)?;
            for line in &kept_lines {
                writeln!(tmp_file, "{}", line)?;
            }
            tmp_file.flush()?;
        }
        std::fs::rename(&tmp_path, &self.path)?;

        // Evict from in-memory state
        for key in &removed_track_keys {
            self.downloaded.remove(key);
        }
        self.downloaded_item_ids.remove(item_id);

        Ok(())
    }

    /// Get the number of cached entries
    #[allow(dead_code)] // Used in tests; production code checks keys directly
    pub fn len(&self) -> usize {
        self.downloaded.len()
    }

    /// Check if cache is empty
    #[allow(dead_code)] // Used in tests; production code checks keys directly
    pub fn is_empty(&self) -> bool {
        self.downloaded.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn cache_operations() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.txt");

        let mut cache = DownloadCache::open(&cache_path).unwrap();
        assert!(cache.is_empty());

        let key = "Artist|Album|Track|180";
        assert!(!cache.is_downloaded(key));

        cache.mark_downloaded(key, "p123456").unwrap();
        assert!(cache.is_downloaded(key));
        assert_eq!(cache.len(), 1);

        // Marking again should be idempotent
        cache.mark_downloaded(key, "p123456").unwrap();
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn forget_item_removes_entries_and_updates_memory() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.txt");

        let mut cache = DownloadCache::open(&cache_path).unwrap();

        // Two tracks for item-A, one track for item-B
        cache
            .mark_downloaded("Artist A|Album A|Track 1|180", "item-A")
            .unwrap();
        cache
            .mark_downloaded("Artist A|Album A|Track 2|210", "item-A")
            .unwrap();
        cache
            .mark_downloaded("Artist B|Album B|Track 1|200", "item-B")
            .unwrap();
        assert_eq!(cache.len(), 3);
        assert!(cache.is_item_downloaded("item-A"));
        assert!(cache.is_item_downloaded("item-B"));

        cache.forget_item("item-A").unwrap();

        // In-memory state should only contain item-B's track
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_downloaded("Artist A|Album A|Track 1|180"));
        assert!(!cache.is_downloaded("Artist A|Album A|Track 2|210"));
        assert!(cache.is_downloaded("Artist B|Album B|Track 1|200"));
        assert!(!cache.is_item_downloaded("item-A"));
        assert!(cache.is_item_downloaded("item-B"));

        // Reload from disk and verify persistence
        let reloaded = DownloadCache::open(&cache_path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(!reloaded.is_downloaded("Artist A|Album A|Track 1|180"));
        assert!(reloaded.is_downloaded("Artist B|Album B|Track 1|200"));
        assert!(!reloaded.is_item_downloaded("item-A"));
        assert!(reloaded.is_item_downloaded("item-B"));
    }

    #[test]
    fn forget_item_noop_for_unknown_id() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.txt");

        let mut cache = DownloadCache::open(&cache_path).unwrap();
        cache
            .mark_downloaded("Artist A|Album A|Track 1|180", "item-A")
            .unwrap();

        // Forgetting a non-existent ID should not error and should leave state intact
        cache.forget_item("item-unknown").unwrap();

        assert_eq!(cache.len(), 1);
        assert!(cache.is_downloaded("Artist A|Album A|Track 1|180"));
        assert!(cache.is_item_downloaded("item-A"));
    }

    #[test]
    fn cache_persistence() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.txt");

        // Write some entries
        {
            let mut cache = DownloadCache::open(&cache_path).unwrap();
            cache
                .mark_downloaded("Artist|Album|Track 1|180", "p123")
                .unwrap();
            cache
                .mark_downloaded("Artist|Album|Track 2|210", "p123")
                .unwrap();
        }

        // Reload and verify
        {
            let cache = DownloadCache::open(&cache_path).unwrap();
            assert_eq!(cache.len(), 2);
            assert!(cache.is_downloaded("Artist|Album|Track 1|180"));
            assert!(cache.is_downloaded("Artist|Album|Track 2|210"));
            assert!(!cache.is_downloaded("Artist|Album|Track 3|240"));
        }
    }
}
