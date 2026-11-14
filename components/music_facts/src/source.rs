use serde::{Deserialize, Serialize};
use std::fmt;

/// A source name (tool or origin kind) was empty.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0} is empty")]
pub struct EmptyNameError(&'static str);

/// Generates a non-empty, opaque string newtype. The library never parses these
/// or matches them against literals: the writing client owns the vocabulary.
macro_rules! source_name {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(name: impl Into<String>) -> Result<Self, EmptyNameError> {
                let name = name.into();
                if name.is_empty() {
                    return Err(EmptyNameError($what));
                }
                Ok(Self(name))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = EmptyNameError;

            fn try_from(name: String) -> Result<Self, Self::Error> {
                Self::new(name)
            }
        }

        impl From<$name> for String {
            fn from(name: $name) -> Self {
                name.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

source_name!(
    /// Name of the tool that wrote a fact (e.g. `mdma-bandcamp`)
    ToolName,
    "tool name"
);

source_name!(
    /// Kind of origin a fact came from (e.g. `Bandcamp`, `User`)
    OriginKind,
    "origin kind"
);

/// Origin/provenance of a fact. The kind is an opaque string owned by the writing
/// client; the wire shape is `{"type": "<kind>"}`.
///
/// Older logs carry a `data` payload next to `type`. It is ignored on read
/// (serde skips unknown fields) and never written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    #[serde(rename = "type")]
    pub kind: OriginKind,
}

/// What makes two fact sources the same source: the tool and the origin kind.
/// The version is recorded provenance, never identity (ADR-018).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceIdentity<'a> {
    pub tool: &'a ToolName,
    pub origin: &'a OriginKind,
}

/// Source of a fact - who/what created it and where it came from
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactSource {
    /// Tool that created the fact
    pub tool: ToolName,

    /// Version of the tool
    pub version: String,

    /// Origin/provenance of the data
    pub origin: Origin,
}

impl FactSource {
    /// Create a new fact source.
    ///
    /// For names fixed in code. Use [`FactSource::try_new`] for names that come
    /// from outside.
    ///
    /// # Panics
    ///
    /// Panics if `tool` or `origin` is empty.
    pub fn new(
        tool: impl Into<String>,
        version: impl Into<String>,
        origin: impl Into<String>,
    ) -> Self {
        Self::try_new(tool, version, origin).expect("fact source names must not be empty")
    }

    /// Create a new fact source, rejecting an empty tool or origin kind.
    pub fn try_new(
        tool: impl Into<String>,
        version: impl Into<String>,
        origin: impl Into<String>,
    ) -> Result<Self, EmptyNameError> {
        Ok(Self {
            tool: ToolName::new(tool)?,
            version: version.into(),
            origin: Origin {
                kind: OriginKind::new(origin)?,
            },
        })
    }

    /// The part of the source that identifies it: tool and origin kind.
    pub fn identity(&self) -> SourceIdentity<'_> {
        SourceIdentity {
            tool: &self.tool,
            origin: &self.origin.kind,
        }
    }

    /// Whether `other` is the same source: equal identities (tool and origin kind).
    /// `version` is ignored, because a new version of a source is not a different
    /// source.
    ///
    /// A retract cancels assertions of the same source this way (ADR-018), so a
    /// fact written by an older version can still be retracted. The derived
    /// `PartialEq` stays the strict, field-by-field comparison.
    pub fn same_source(&self, other: &FactSource) -> bool {
        self.identity() == other.identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_source_ignores_version() {
        let a = FactSource::new("tool", "1.0.0", "FilesystemScan");
        assert!(a.same_source(&FactSource::new("tool", "2.0.0", "FilesystemScan")));
        assert!(!a.same_source(&FactSource::new("other", "1.0.0", "FilesystemScan")));
        assert!(!a.same_source(&FactSource::new("tool", "1.0.0", "User")));
    }

    #[test]
    fn identity_is_tool_and_origin_kind() {
        let a = FactSource::new("tool", "1.0.0", "User");
        let b = FactSource::new("tool", "2.0.0", "User");
        assert_eq!(a.identity(), b.identity());
        assert_ne!(
            a.identity(),
            FactSource::new("tool", "1.0.0", "Unknown").identity()
        );
    }

    #[test]
    fn empty_names_are_rejected() {
        assert!(FactSource::try_new("", "1", "User").is_err());
        assert!(FactSource::try_new("tool", "1", "").is_err());
        assert!(serde_json::from_str::<FactSource>(
            r#"{"tool":"","version":"1","origin":{"type":"User"}}"#
        )
        .is_err());
    }

    #[test]
    fn wire_shape_is_tool_version_origin_type() {
        let source = FactSource::new("mdma-bandcamp", "0.4.0", "Bandcamp");
        assert_eq!(
            serde_json::to_string(&source).unwrap(),
            r#"{"tool":"mdma-bandcamp","version":"0.4.0","origin":{"type":"Bandcamp"}}"#
        );
    }

    #[test]
    fn old_line_with_origin_data_parses_and_keeps_identity() {
        let old = r#"{"tool":"mdma-library","version":"0.3.0","origin":{"type":"FilesystemScan","data":{"scan_time":"2026-01-01T00:00:00Z","inferred_source":null}}}"#;
        let parsed: FactSource = serde_json::from_str(old).unwrap();
        assert_eq!(
            parsed.identity(),
            FactSource::new("mdma-library", "0.4.0", "FilesystemScan").identity()
        );
        // Writers never emit `data`.
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            r#"{"tool":"mdma-library","version":"0.3.0","origin":{"type":"FilesystemScan"}}"#
        );
    }

    #[test]
    fn old_unit_origins_parse() {
        for kind in ["Unknown", "User"] {
            let line = format!(r#"{{"tool":"t","version":"1","origin":{{"type":"{kind}"}}}}"#);
            let parsed: FactSource = serde_json::from_str(&line).unwrap();
            assert_eq!(parsed.origin.kind, kind);
        }
    }
}
