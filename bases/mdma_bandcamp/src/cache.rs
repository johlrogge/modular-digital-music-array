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

    #[error("Invalid cache entry: {0}")]
    InvalidEntry(String),
}

/// Track-oriented download cache
pub struct DownloadCache {
    path: PathBuf,
    /// Set of track keys that have been downloaded
    downloaded: HashSet<String>,
}

impl DownloadCache {
    /// Open or create a cache file
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let downloaded = if path.exists() {
            Self::load_entries(path)?
        } else {
            HashSet::new()
        };

        tracing::info!(path = %path.display(), entries = downloaded.len(), "Loaded download cache");

        Ok(Self {
            path: path.to_path_buf(),
            downloaded,
        })
    }

    /// Load entries from cache file
    fn load_entries(path: &Path) -> Result<HashSet<String>, CacheError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = HashSet::new();

        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Format: track_key|source_item_id
            // We only need the track_key part for lookup
            // Reconstruct the key from the first 4 parts (artist|album|track|duration)
            let parts: Vec<&str> = line.splitn(5, '|').collect();
            if parts.len() >= 4 {
                let track_key = format!("{}|{}|{}|{}", parts[0], parts[1], parts[2], parts[3]);
                entries.insert(track_key);
            } else {
                tracing::warn!(line = %line, "Invalid cache entry, skipping");
            }
        }

        Ok(entries)
    }

    /// Build a track key for cache lookup
    pub fn make_track_key(
        artist: &str,
        album: &str,
        track_name: &str,
        duration_secs: u32,
    ) -> String {
        format!("{}|{}|{}|{}", artist, album, track_name, duration_secs)
    }

    /// Check if a track has been downloaded
    pub fn is_downloaded(&self, key: &str) -> bool {
        self.downloaded.contains(key)
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

        // Add to in-memory set
        self.downloaded.insert(key.to_string());

        Ok(())
    }

    /// Get the number of cached entries
    pub fn len(&self) -> usize {
        self.downloaded.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.downloaded.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_make_track_key() {
        let key = DownloadCache::make_track_key("Artist", "Album", "Track One", 234);
        assert_eq!(key, "Artist|Album|Track One|234");
    }

    #[test]
    fn test_cache_operations() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.txt");

        let mut cache = DownloadCache::open(&cache_path).unwrap();
        assert!(cache.is_empty());

        let key = DownloadCache::make_track_key("Artist", "Album", "Track", 180);
        assert!(!cache.is_downloaded(&key));

        cache.mark_downloaded(&key, "p123456").unwrap();
        assert!(cache.is_downloaded(&key));
        assert_eq!(cache.len(), 1);

        // Marking again should be idempotent
        cache.mark_downloaded(&key, "p123456").unwrap();
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_cache_persistence() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("cache.txt");

        // Write some entries
        {
            let mut cache = DownloadCache::open(&cache_path).unwrap();
            cache
                .mark_downloaded(
                    &DownloadCache::make_track_key("Artist", "Album", "Track 1", 180),
                    "p123",
                )
                .unwrap();
            cache
                .mark_downloaded(
                    &DownloadCache::make_track_key("Artist", "Album", "Track 2", 210),
                    "p123",
                )
                .unwrap();
        }

        // Reload and verify
        {
            let cache = DownloadCache::open(&cache_path).unwrap();
            assert_eq!(cache.len(), 2);
            assert!(cache.is_downloaded(&DownloadCache::make_track_key(
                "Artist", "Album", "Track 1", 180
            )));
            assert!(cache.is_downloaded(&DownloadCache::make_track_key(
                "Artist", "Album", "Track 2", 210
            )));
            assert!(!cache.is_downloaded(&DownloadCache::make_track_key(
                "Artist", "Album", "Track 3", 240
            )));
        }
    }
}
