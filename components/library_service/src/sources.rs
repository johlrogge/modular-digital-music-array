//! The library's own fact sources: the one place that names its tools and origin
//! kinds. Everything a client states (tool, origin kind, version) is opaque to the
//! library and never appears here.
//!
//! The version is the release version of the binary that writes (ADR-020), handed to
//! the service at construction; it is not this component's own crate version.

use music_facts::FactSource;

/// Tool of the facts the library computes and reads itself.
pub(crate) const LIBRARY_TOOL: &str = "mdma-library";

/// Tool of the facts a dedup merge writes itself (`Replaces(loser)`).
pub(crate) const DEDUP_TOOL: &str = "mdma-dedup";

/// Origin kind of the library's derived facts, and of dedup's.
pub(crate) const UNKNOWN_ORIGIN: &str = "Unknown";

/// Origin kind of what the library reads from a file it ingests.
pub(crate) const SCAN_ORIGIN: &str = "FilesystemScan";

/// A version string was empty.
#[derive(Debug, thiserror::Error)]
#[error("a release version must not be empty")]
pub struct EmptyReleaseVersion;

/// The release version of the binary hosting the service (ADR-020), e.g.
/// `ReleaseVersion::new(env!("CARGO_PKG_VERSION"))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion(String);

impl ReleaseVersion {
    pub fn new(version: &str) -> Result<Self, EmptyReleaseVersion> {
        if version.trim().is_empty() {
            return Err(EmptyReleaseVersion);
        }
        Ok(Self(version.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Every source the library writes under, built once from the release version.
#[derive(Debug, Clone)]
pub(crate) struct LibrarySources {
    /// The version every source below carries; `GetStatus` reports it.
    pub(crate) version: ReleaseVersion,
    /// What the library reads from a file it ingests (file path, format, import time,
    /// stream properties), and the tags when the ingest has no writer to state them.
    pub(crate) scan: FactSource,
    /// What the library derives itself (cover art, identity hashes).
    pub(crate) derived: FactSource,
    /// What a dedup merge writes.
    pub(crate) dedup: FactSource,
    /// The source of a write or retract that states none: the user's.
    pub(crate) user_default: FactSource,
}

impl LibrarySources {
    pub(crate) fn new(version: &ReleaseVersion) -> Self {
        let v = version.as_str();
        Self {
            version: version.clone(),
            scan: FactSource::new(LIBRARY_TOOL, v, SCAN_ORIGIN),
            derived: FactSource::new(LIBRARY_TOOL, v, UNKNOWN_ORIGIN),
            dedup: FactSource::new(DEDUP_TOOL, v, UNKNOWN_ORIGIN),
            user_default: crate::ipc::user_fact_source(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version() -> ReleaseVersion {
        ReleaseVersion::new("9.8.7").unwrap()
    }

    fn all(s: &LibrarySources) -> [&FactSource; 4] {
        [&s.scan, &s.derived, &s.dedup, &s.user_default]
    }

    #[test]
    fn the_library_sources_are_distinct_identities() {
        let sources = LibrarySources::new(&version());
        let all = all(&sources);
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(!a.same_source(b), "{a:?} vs {b:?}");
            }
        }
        assert_eq!(sources.user_default.tool, "mdma");
        assert_eq!(sources.user_default.origin.kind, "User");
    }

    #[test]
    fn every_library_source_carries_the_release_version() {
        let sources = LibrarySources::new(&version());
        for s in all(&sources) {
            assert_eq!(s.version, "9.8.7", "{s:?}");
        }
    }

    #[test]
    fn an_empty_release_version_is_refused() {
        assert!(ReleaseVersion::new("").is_err());
        assert!(ReleaseVersion::new("  ").is_err());
        assert_eq!(version().as_str(), "9.8.7");
    }
}
