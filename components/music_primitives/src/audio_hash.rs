use crate::prefixed_hash::{prefixed_sha256_newtype, PrefixedHashParseError};

/// Why a string is not a valid [`AudioHash`].
pub type AudioHashParseError = PrefixedHashParseError;

prefixed_sha256_newtype!(
    /// Container-aware hash of the encoded audio payload only (ADR-010).
    ///
    /// Format: `audio-sha256-v1:<64 lowercase hex>`. The version is part of the
    /// value, so hashes of different algorithm versions never compare equal.
    AudioHash,
    "audio-sha256-v1:"
);

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
