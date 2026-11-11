use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Source of a fact - who/what created it and where it came from
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactSource {
    /// Tool that created the fact
    pub tool: String,

    /// Version of the tool
    pub version: String,

    /// Origin/provenance of the data
    pub origin: FactOrigin,
}

/// Origin/provenance of music data
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum FactOrigin {
    /// Downloaded/purchased from Beatport
    Beatport {
        track_url: Option<String>,
        label_url: Option<String>,
        track_id: Option<String>,
    },

    /// Downloaded/purchased from Bandcamp
    Bandcamp { artist_url: Option<String> },

    /// Found via filesystem scan
    FilesystemScan {
        scan_time: DateTime<Utc>,
        inferred_source: Option<String>,
    },

    /// Unknown/other source
    Unknown,

    /// Initiated directly by the user (e.g. a bookmark)
    User,
}

impl FactOrigin {
    /// Infer origin from file path and comment field
    ///
    /// Looks for "beatport" or "bandcamp" in path, and extracts URLs from comment
    pub fn infer(path: &Path, comment: &Option<String>) -> Self {
        let path_str = path.to_string_lossy().to_lowercase();

        // Check path for source indicators
        if path_str.contains("beatport") {
            return FactOrigin::Beatport {
                track_url: None,
                label_url: None,
                track_id: None,
            };
        }

        if path_str.contains("bandcamp") {
            // Try to extract bandcamp URL from comment
            let artist_url = comment.as_ref().and_then(|c| {
                if c.starts_with("Visit https://") && c.contains(".bandcamp.com") {
                    Some(c.trim_start_matches("Visit ").to_string())
                } else {
                    None
                }
            });

            return FactOrigin::Bandcamp { artist_url };
        }

        // Default to filesystem scan
        FactOrigin::FilesystemScan {
            scan_time: Utc::now(),
            inferred_source: None,
        }
    }

    /// Create a Beatport origin with full details
    pub fn beatport(
        track_url: Option<String>,
        label_url: Option<String>,
        track_id: Option<String>,
    ) -> Self {
        FactOrigin::Beatport {
            track_url,
            label_url,
            track_id,
        }
    }

    /// Create a Bandcamp origin
    pub fn bandcamp(artist_url: Option<String>) -> Self {
        FactOrigin::Bandcamp { artist_url }
    }

    /// Create a filesystem scan origin
    pub fn filesystem_scan(inferred_source: Option<String>) -> Self {
        FactOrigin::FilesystemScan {
            scan_time: Utc::now(),
            inferred_source,
        }
    }
}

impl FactSource {
    /// Create a new fact source
    pub fn new(tool: impl Into<String>, version: impl Into<String>, origin: FactOrigin) -> Self {
        Self {
            tool: tool.into(),
            version: version.into(),
            origin,
        }
    }

    /// Whether `other` is the same source: the same `tool` and the same kind of
    /// origin. `version` and the origin's payload (scan time, URLs, ids) are
    /// ignored, because a new version of a source is not a different source.
    ///
    /// A retract cancels assertions of the same source this way (ADR-018), so a
    /// fact written by an older version, or with a different scan time, can still
    /// be retracted. The derived `PartialEq` stays the strict, field-by-field
    /// comparison.
    pub fn same_source(&self, other: &FactSource) -> bool {
        self.tool == other.tool
            && std::mem::discriminant(&self.origin) == std::mem::discriminant(&other.origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn same_source_ignores_version_and_origin_payload() {
        let at = |h| Utc.with_ymd_and_hms(2026, 1, 1, h, 0, 0).unwrap();
        let scan = |h| FactOrigin::FilesystemScan {
            scan_time: at(h),
            inferred_source: None,
        };
        let a = FactSource::new("tool", "1.0.0", scan(1));
        assert!(a.same_source(&FactSource::new("tool", "2.0.0", scan(2))));
        assert!(!a.same_source(&FactSource::new("other", "1.0.0", scan(1))));
        assert!(!a.same_source(&FactSource::new("tool", "1.0.0", FactOrigin::User)));
    }
}
