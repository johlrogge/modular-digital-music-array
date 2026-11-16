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

const CANONICAL_PREFIX: &str = "sha256:";
const CANONICAL_HEX_LEN: usize = 64;

/// A content hash in full canonical form: `sha256:` followed by exactly 64
/// lowercase hex digits. Holds no prefix, so what carries one cannot be a
/// short form. Services that record or compare hashes (playback state, play
/// facts) hold this; resolving a prefix to it is the library's job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalHash(ContentHash);

/// A [`ContentHash`] that is not in canonical form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a full content hash: {0}")]
pub struct NotCanonical(pub ContentHash);

impl CanonicalHash {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_content_hash(&self) -> &ContentHash {
        &self.0
    }
}

impl TryFrom<ContentHash> for CanonicalHash {
    type Error = NotCanonical;

    fn try_from(hash: ContentHash) -> Result<Self, Self::Error> {
        let canonical = hash
            .as_str()
            .strip_prefix(CANONICAL_PREFIX)
            .is_some_and(|hex| {
                hex.len() == CANONICAL_HEX_LEN
                    && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            });
        if canonical {
            Ok(Self(hash))
        } else {
            Err(NotCanonical(hash))
        }
    }
}

impl From<CanonicalHash> for ContentHash {
    fn from(hash: CanonicalHash) -> Self {
        hash.0
    }
}

impl From<&CanonicalHash> for ContentHash {
    fn from(hash: &CanonicalHash) -> Self {
        hash.0.clone()
    }
}

impl Display for CanonicalHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<CanonicalHash> for ContentHash {
    fn eq(&self, other: &CanonicalHash) -> bool {
        *self == other.0
    }
}

impl PartialEq<ContentHash> for CanonicalHash {
    fn eq(&self, other: &ContentHash) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    fn hex64() -> String {
        "0123456789abcdef".repeat(4)
    }

    #[rstest]
    #[case::full(format!("sha256:{}", hex64()))]
    #[case::all_zero(format!("sha256:{}", "0".repeat(64)))]
    fn canonical_accepts_full_lowercase_hash(#[case] hash: String) {
        let canonical = CanonicalHash::try_from(ContentHash::new(hash.clone())).unwrap();
        assert_eq!(canonical.as_str(), hash);
        assert_eq!(ContentHash::from(canonical), ContentHash::new(hash));
    }

    #[rstest]
    #[case::eight_hex("9fb4105e".to_string())]
    #[case::twelve_hex("9fb4105e0a1b".to_string())]
    #[case::prefix_and_eight("sha256:9fb4105e".to_string())]
    #[case::sixty_three(format!("sha256:{}", &hex64()[..63]))]
    #[case::sixty_five(format!("sha256:{}0", hex64()))]
    #[case::uppercase(format!("sha256:{}", hex64().to_uppercase()))]
    #[case::one_uppercase(format!("sha256:{}F", &hex64()[..63]))]
    #[case::no_prefix(hex64())]
    #[case::non_hex(format!("sha256:{}", "g".repeat(64)))]
    #[case::empty(String::new())]
    fn canonical_rejects_anything_else(#[case] hash: String) {
        let err = CanonicalHash::try_from(ContentHash::new(hash.clone())).unwrap_err();
        assert_eq!(err, NotCanonical(ContentHash::new(hash.clone())));
        assert_eq!(err.to_string(), format!("not a full content hash: {hash}"));
    }

    #[test]
    fn a_prefix_is_not_equal_to_the_canonical_hash_it_abbreviates() {
        let canonical = CanonicalHash::try_from(ContentHash::new(format!(
            "sha256:9fb4105e{}",
            "0".repeat(56)
        )))
        .unwrap();
        let prefix = ContentHash::new("9fb4105e");
        assert!(prefix != canonical);
        assert!(canonical != prefix);
        // Equal only when the strings are.
        let same = ContentHash::new(canonical.as_str());
        assert!(same == canonical);
    }

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
