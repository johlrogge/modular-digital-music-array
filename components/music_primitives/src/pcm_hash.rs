use crate::prefixed_hash::prefixed_sha256_newtype;

prefixed_sha256_newtype!(
    /// Exact hash of the decoded lossless PCM samples (#55 tier 1b).
    ///
    /// Format: `pcm-sha256-v1:<64 lowercase hex>`. The version is part of the
    /// value, so hashes of different algorithm versions never compare equal.
    PcmHash,
    "pcm-sha256-v1:"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefixed_hash::PrefixedHashParseError;
    use pretty_assertions::assert_eq;

    fn digest() -> [u8; 32] {
        let mut d = [0u8; 32];
        for (i, b) in d.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(9).wrapping_add(0xAB);
        }
        d
    }

    #[test]
    fn prefix_is_pcm_v1() {
        assert_eq!(PcmHash::PREFIX_V1, "pcm-sha256-v1:");
    }

    #[test]
    fn roundtrip_through_parse() {
        let h = PcmHash::from_sha256_v1(digest());
        assert!(h.as_str().starts_with(PcmHash::PREFIX_V1));
        assert_eq!(h.as_str().len(), PcmHash::PREFIX_V1.len() + 64);
        let parsed: PcmHash = h.as_str().parse().unwrap();
        assert_eq!(parsed, h);
        assert_eq!(h.to_string(), h.as_str());
    }

    #[test]
    fn serde_json_roundtrip() {
        let h = PcmHash::from_sha256_v1(digest());
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, format!("\"{}\"", h.as_str()));
        let back: PcmHash = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn serde_rejects_invalid() {
        let bad = format!("\"sha256:{}\"", "a".repeat(64));
        assert!(serde_json::from_str::<PcmHash>(&bad).is_err());
    }

    #[test]
    fn rejects_other_prefixes() {
        for prefix in ["sha256:", "audio-sha256-v1:"] {
            let s = format!("{prefix}{}", "a".repeat(64));
            assert_eq!(
                s.parse::<PcmHash>(),
                Err(PrefixedHashParseError::MissingPrefix),
                "prefix {prefix:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_digests() {
        let p = PcmHash::PREFIX_V1;
        for digest in [
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            String::new(),
        ] {
            assert_eq!(
                format!("{p}{digest}").parse::<PcmHash>(),
                Err(PrefixedHashParseError::BadDigest),
                "digest {digest:?}"
            );
        }
    }
}
