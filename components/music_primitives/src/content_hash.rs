use std::fmt::Display;

use serde::{Deserialize, Serialize};

/// Content hash of audio file — THE cross-service track identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn new(hash: impl Into<String>) -> Self {
        Self(hash.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_hash_string() {
        let h = ContentHash::new("abc123");
        assert_eq!(h.as_str(), "abc123");
    }

    #[test]
    fn display_shows_inner_string() {
        let h = ContentHash::new("deadbeef");
        assert_eq!(format!("{h}"), "deadbeef");
    }

    #[test]
    fn roundtrip_serialization() {
        let h = ContentHash::new("sha256:abc");
        let json = serde_json::to_string(&h).unwrap();
        let decoded: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, decoded);
    }
}
