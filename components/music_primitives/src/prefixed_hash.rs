/// Why a string is not a valid prefixed sha256 hash newtype
/// (e.g. `AudioHash`, `PcmHash`).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PrefixedHashParseError {
    #[error("missing expected hash prefix")]
    MissingPrefix,
    #[error("digest must be 64 lowercase hex chars")]
    BadDigest,
}

/// Validate `s` as `<prefix><64 lowercase hex>`.
pub(crate) fn validate(prefix: &str, s: &str) -> Result<(), PrefixedHashParseError> {
    let digest = s
        .strip_prefix(prefix)
        .ok_or(PrefixedHashParseError::MissingPrefix)?;
    let ok = digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(PrefixedHashParseError::BadDigest)
    }
}

/// Render `<prefix><64 lowercase hex>` from a sha256 digest.
pub(crate) fn render(prefix: &str, digest: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(prefix.len() + 64);
    s.push_str(prefix);
    for b in digest {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Define a `String`-backed newtype holding `<PREFIX_V1><64 lowercase hex>`.
///
/// Provides `PREFIX_V1`, `from_sha256_v1`, `as_str`, `FromStr`, `TryFrom<String>`,
/// `From<X> for String`, `Display`, and serde as a plain string.
macro_rules! prefixed_sha256_newtype {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub const PREFIX_V1: &'static str = $prefix;

            pub fn from_sha256_v1(digest: [u8; 32]) -> Self {
                Self($crate::prefixed_hash::render(Self::PREFIX_V1, digest))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::prefixed_hash::PrefixedHashParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $crate::prefixed_hash::validate(Self::PREFIX_V1, s)?;
                Ok(Self(s.to_string()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = $crate::prefixed_hash::PrefixedHashParseError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                s.parse()
            }
        }

        impl From<$name> for String {
            fn from(h: $name) -> Self {
                h.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pub(crate) use prefixed_sha256_newtype;
