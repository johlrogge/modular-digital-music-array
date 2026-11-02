//! The remembered node, persisted as JSON in the user's config directory.

use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("node cache I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("node cache at {path} is corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

type Result<T> = std::result::Result<T, CacheError>;

/// The node the CLI last talked to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCache {
    pub id: String,
    pub name: String,
    pub host: String,
    pub ip: Ipv4Addr,
    pub port: u16,
}

/// `<config dir>/mdma/node.json`, if a config directory is known.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("mdma").join("node.json"))
}

/// Load the cache. A missing file is `Ok(None)`; a corrupt one is an error.
pub fn load(path: &Path) -> Result<Option<NodeCache>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CacheError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| CacheError::Corrupt {
            path: path.to_path_buf(),
            source,
        })
}

/// Atomically write the cache (temp file + rename), creating the parent directory.
pub fn save(path: &Path, cache: &NodeCache) -> Result<()> {
    let io_err = |source| CacheError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let json = serde_json::to_vec_pretty(cache).expect("NodeCache serializes");
    private_file::write_file_atomic(path, &json, 0o644).map_err(io_err)
}

/// Remove the cache. A missing file is fine.
pub fn forget(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CacheError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NodeCache {
        NodeCache {
            id: "mdma-stage".into(),
            name: "Stage".into(),
            host: "mdma-stage.local".into(),
            ip: Ipv4Addr::new(192, 168, 1, 20),
            port: 5555,
        }
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("node.json")).unwrap(), None);
    }

    #[test]
    fn save_then_load_roundtrips_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mdma").join("node.json");
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(sample()));
    }

    #[test]
    fn save_overwrites_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.json");
        save(&path, &sample()).unwrap();
        let mut other = sample();
        other.ip = Ipv4Addr::new(10, 0, 0, 1);
        save(&path, &other).unwrap();
        assert_eq!(load(&path).unwrap(), Some(other));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_file_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.json");
        fs::write(&path, "{not json").unwrap();
        assert!(matches!(load(&path), Err(CacheError::Corrupt { .. })));
    }

    #[test]
    fn forget_removes_and_tolerates_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.json");
        save(&path, &sample()).unwrap();
        forget(&path).unwrap();
        assert_eq!(load(&path).unwrap(), None);
        forget(&path).unwrap();
    }

    #[test]
    fn default_path_ends_with_mdma_node_json() {
        if let Some(p) = default_path() {
            assert!(p.ends_with("mdma/node.json"));
        }
    }
}
