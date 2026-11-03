use std::fmt::{self, Display, Write as _};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Container-aware hash of the encoded audio payload only (ADR-010).
///
/// Format: `audio-sha256-v1:<64 lowercase hex>`. The version is part of the
/// value, so hashes of different algorithm versions never compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AudioHash(String);

/// Why a string is not a valid [`AudioHash`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AudioHashParseError {
    #[error("missing audio-sha256-v1: prefix")]
    MissingPrefix,
    #[error("digest must be 64 lowercase hex chars")]
    BadDigest,
}

impl AudioHash {
    pub const PREFIX_V1: &'static str = "audio-sha256-v1:";

    pub fn from_sha256_v1(digest: [u8; 32]) -> Self {
        let mut s = String::with_capacity(Self::PREFIX_V1.len() + 64);
        s.push_str(Self::PREFIX_V1);
        for b in digest {
            // Writing to a String cannot fail.
            let _ = write!(s, "{b:02x}");
        }
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AudioHash {
    type Err = AudioHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digest = s
            .strip_prefix(Self::PREFIX_V1)
            .ok_or(AudioHashParseError::MissingPrefix)?;
        let ok = digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(AudioHashParseError::BadDigest)
        }
    }
}

impl TryFrom<String> for AudioHash {
    type Error = AudioHashParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<AudioHash> for String {
    fn from(h: AudioHash) -> Self {
        h.0
    }
}

impl Display for AudioHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn digest() -> [u8; 32] {
        let mut d = [0u8; 32];
        for (i, b) in d.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(9).wrapping_add(0xAB);
        }
        d
    }

    #[test]
    fn roundtrip_through_parse() {
        let h = AudioHash::from_sha256_v1(digest());
        assert!(h.as_str().starts_with(AudioHash::PREFIX_V1));
        assert_eq!(h.as_str().len(), AudioHash::PREFIX_V1.len() + 64);
        let parsed: AudioHash = h.as_str().parse().unwrap();
        assert_eq!(parsed, h);
        assert_eq!(h.to_string(), h.as_str());
    }

    #[test]
    fn serde_json_roundtrip() {
        let h = AudioHash::from_sha256_v1(digest());
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, format!("\"{}\"", h.as_str()));
        let back: AudioHash = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn serde_rejects_invalid() {
        let bad = format!("\"sha256:{}\"", "a".repeat(64));
        assert!(serde_json::from_str::<AudioHash>(&bad).is_err());
    }

    #[test]
    fn rejects_wrong_prefix() {
        let s = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            s.parse::<AudioHash>(),
            Err(AudioHashParseError::MissingPrefix)
        );
    }

    #[test]
    fn rejects_bad_digests() {
        let p = AudioHash::PREFIX_V1;
        for digest in [
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            String::new(),
        ] {
            assert_eq!(
                format!("{p}{digest}").parse::<AudioHash>(),
                Err(AudioHashParseError::BadDigest),
                "digest {digest:?}"
            );
        }
    }
}
