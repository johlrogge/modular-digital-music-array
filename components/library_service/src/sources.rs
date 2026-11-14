//! The library's own fact sources: the one place that names its tools and origin
//! kinds. Everything a client states (tool, origin kind, version) is opaque to the
//! library and never appears here.

use music_facts::FactSource;

/// Tool of the facts the library computes and reads itself.
pub(crate) const LIBRARY_TOOL: &str = "mdma-library";

/// Tool of the facts a dedup merge writes itself (`Replaces(loser)`).
pub(crate) const DEDUP_TOOL: &str = "mdma-dedup";

/// Origin kind of the library's derived facts, and of dedup's.
pub(crate) const UNKNOWN_ORIGIN: &str = "Unknown";

/// Origin kind of what the library reads from a file it ingests.
pub(crate) const SCAN_ORIGIN: &str = "FilesystemScan";

/// What the library reads from a file it ingests (file path, format, import time,
/// stream properties), and the tags when the ingest has no writer to state them.
pub(crate) fn scan() -> FactSource {
    FactSource::new(LIBRARY_TOOL, env!("CARGO_PKG_VERSION"), SCAN_ORIGIN)
}

/// What the library derives itself (cover art, identity hashes).
pub(crate) fn derived() -> FactSource {
    FactSource::new(LIBRARY_TOOL, env!("CARGO_PKG_VERSION"), UNKNOWN_ORIGIN)
}

/// What a dedup merge writes.
pub(crate) fn dedup() -> FactSource {
    FactSource::new(DEDUP_TOOL, env!("CARGO_PKG_VERSION"), UNKNOWN_ORIGIN)
}

/// The source of a write or retract that states none: the user's.
pub(crate) fn user_default() -> FactSource {
    crate::ipc::user_fact_source(env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_sources_are_distinct_identities() {
        let all = [scan(), derived(), dedup(), user_default()];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(!a.same_source(b), "{a:?} vs {b:?}");
            }
        }
        assert_eq!(user_default().tool, "mdma");
        assert_eq!(user_default().origin.kind, "User");
    }
}
