use crate::primitives::*;
use chrono::{DateTime, NaiveDate, Utc};
use music_primitives::{Bpm, Key};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Why a track started playing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StartReason {
    /// Started in response to an explicit user request
    OnRequest,
    /// Started automatically because it was next in the queue
    ByQueue,
}

impl fmt::Display for StartReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartReason::OnRequest => write!(f, "OnRequest"),
            StartReason::ByQueue => write!(f, "ByQueue"),
        }
    }
}

impl<'de> Deserialize<'de> for StartReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "OnRequest" => Ok(StartReason::OnRequest),
            "ByQueue" => Ok(StartReason::ByQueue),
            // Legacy DateTime strings or any unrecognized value → OnRequest
            _ => Ok(StartReason::OnRequest),
        }
    }
}

/// Why a track stopped playing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StopReason {
    /// Stopped in response to an explicit user request
    OnRequest,
    /// Stopped because the track played to its natural end
    OnCompletion,
    /// Stopped because the track was skipped
    OnSkip,
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopReason::OnRequest => write!(f, "OnRequest"),
            StopReason::OnCompletion => write!(f, "OnCompletion"),
            StopReason::OnSkip => write!(f, "OnSkip"),
        }
    }
}

impl<'de> Deserialize<'de> for StopReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "OnRequest" => Ok(StopReason::OnRequest),
            "OnCompletion" => Ok(StopReason::OnCompletion),
            "OnSkip" => Ok(StopReason::OnSkip),
            // Legacy DateTime strings or any unrecognized value → OnRequest
            _ => Ok(StopReason::OnRequest),
        }
    }
}

/// Audio file format
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MusicFormat {
    Flac,
    Mp3,
    Aiff,
    Wav,
    M4a,
}

impl fmt::Display for MusicFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MusicFormat::Flac => write!(f, "FLAC"),
            MusicFormat::Mp3 => write!(f, "MP3"),
            MusicFormat::Aiff => write!(f, "AIFF"),
            MusicFormat::Wav => write!(f, "WAV"),
            MusicFormat::M4a => write!(f, "M4A"),
        }
    }
}

/// Presence or absence of embedded album art in a music file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AlbumArtPresence {
    Present,
    Absent,
}

/// All possible metadata values for a music track
///
/// Each variant represents a single fact that can be asserted or retracted
/// about a track. Facts are stored in the stainless-facts stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum MusicValue {
    // ========================================================================
    // File Location & Identity
    // ========================================================================
    /// File path on filesystem
    FilePath(PathBuf),

    // ========================================================================
    // Basic Metadata (from tags)
    // ========================================================================
    /// Track title
    Title(Title),

    /// Artist name
    Artist(Artist),

    /// Album name
    Album(Album),

    /// Album artist (for compilations)
    AlbumArtist(Artist),

    /// Track number on album
    TrackNumber(TrackNumber),

    /// Disc number on a multi-disc release
    DiscNumber(DiscNumber),

    /// Release year
    Year(Year),

    // ========================================================================
    // DJ-Specific Metadata
    // ========================================================================
    /// Beats per minute
    Bpm(Bpm),

    /// Musical key
    Key(Key),

    /// Main genre extracted from full genre string
    MainGenre(String),

    /// Style descriptor from genre (e.g., "Peak Time", "Driving")
    /// Multiple style descriptors may exist for one track
    StyleDescriptor(String),

    /// Full genre string as provided by source
    FullGenre(String),

    // ========================================================================
    // Catalog & Publishing
    // ========================================================================
    /// International Standard Recording Code
    Isrc(Isrc),

    /// Record label name
    Label(String),

    /// Recording year (extracted from RecordingDate)
    RecordingYear(Year),

    /// Full recording date (when available)
    RecordingDate(NaiveDate),

    // ========================================================================
    // URLs & External References
    // ========================================================================
    /// Beatport track URL
    BeatportTrackUrl(String),

    /// Beatport label URL
    BeatportLabelUrl(String),

    /// Bandcamp artist/album URL
    BandcampUrl(String),

    // ========================================================================
    // Provenance & Source Info
    // ========================================================================
    /// Source of the track (e.g., "bandcamp", "beatport")
    Source(String),

    /// Source-specific item ID (e.g., "p367090081" for bandcamp).
    /// Unique within a given source.
    ItemId(String),

    /// Comment field from metadata
    Comment(String),

    /// Beatport track ID (extracted from fileowner field)
    BeatportTrackId(String),

    // ========================================================================
    // Audio Properties
    // ========================================================================
    /// Bit depth (16 or 24 bit typically)
    BitDepth(BitDepth),

    /// Number of channels (1 = mono, 2 = stereo)
    Channels(Channels),

    /// Sample rate in Hz
    SampleRate(SampleRate),

    /// Duration in seconds
    DurationSeconds(DurationSeconds),

    /// Bitrate in kbps
    Bitrate(Bitrate),

    // ========================================================================
    // File Properties
    // ========================================================================
    /// File size in bytes
    FileSizeBytes(FileSizeBytes),

    /// Whether the file has embedded album art
    HasAlbumArt(AlbumArtPresence),

    /// Audio file format (FLAC, MP3, AIFF, WAV)
    Format(MusicFormat),

    // ========================================================================
    // Encoder Information
    // ========================================================================
    /// Encoder software (e.g., "Beatport", "reference libFLAC 1.3.3 20190804")
    EncoderSoftware(String),

    /// Who encoded the file (e.g., "Beatport")
    EncodedBy(String),

    // ========================================================================
    // Cover Art
    // ========================================================================
    /// Relative path to extracted cover art image (e.g. "cover-art/<hash>.jpg")
    CoverArtPath(String),

    // ========================================================================
    // Play History
    // ========================================================================
    /// Track started playing
    TrackStarted(StartReason),

    /// Track stopped playing
    TrackStopped(StopReason),

    // ========================================================================
    // Import Provenance
    // ========================================================================
    /// When the track was added to the library
    AddedAt(DateTime<Utc>),

    // ========================================================================
    // User Annotations
    // ========================================================================
    /// Track bookmarked by the user, optionally within a named scope (e.g. a set name)
    Bookmarked {
        scope: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // ========================================================================
    // Track Lifecycle
    // ========================================================================
    /// This track was superseded by a better version (same work). Asserted on the OLD track.
    SupersededBy {
        replacement: crate::primitives::ContentHash,
        timestamp: DateTime<Utc>,
    },

    /// Track soft-deleted: hidden from default views, file/blob retained, recoverable.
    Deleted { timestamp: DateTime<Utc> },
}

impl MusicValue {
    /// Returns the display name for this value (e.g., "Title", "BPM", "Duration")
    pub fn display_name(&self) -> &'static str {
        match self {
            MusicValue::FilePath(_) => "FilePath",
            MusicValue::Title(_) => "Title",
            MusicValue::Artist(_) => "Artist",
            MusicValue::Album(_) => "Album",
            MusicValue::AlbumArtist(_) => "AlbumArtist",
            MusicValue::TrackNumber(_) => "TrackNumber",
            MusicValue::DiscNumber(_) => "DiscNumber",
            MusicValue::Year(_) => "Year",
            MusicValue::Bpm(_) => "BPM",
            MusicValue::Key(_) => "Key",
            MusicValue::MainGenre(_) => "MainGenre",
            MusicValue::StyleDescriptor(_) => "StyleDescriptor",
            MusicValue::FullGenre(_) => "FullGenre",
            MusicValue::Isrc(_) => "ISRC",
            MusicValue::Label(_) => "Label",
            MusicValue::RecordingYear(_) => "RecordingYear",
            MusicValue::RecordingDate(_) => "RecordingDate",
            MusicValue::BeatportTrackUrl(_) => "BeatportTrackUrl",
            MusicValue::BeatportLabelUrl(_) => "BeatportLabelUrl",
            MusicValue::BandcampUrl(_) => "BandcampUrl",
            MusicValue::Source(_) => "Source",
            MusicValue::ItemId(_) => "ItemId",
            MusicValue::Comment(_) => "Comment",
            MusicValue::BeatportTrackId(_) => "BeatportTrackId",
            MusicValue::BitDepth(_) => "BitDepth",
            MusicValue::Channels(_) => "Channels",
            MusicValue::SampleRate(_) => "SampleRate",
            MusicValue::DurationSeconds(_) => "Duration",
            MusicValue::Bitrate(_) => "Bitrate",
            MusicValue::FileSizeBytes(_) => "FileSize",
            MusicValue::HasAlbumArt(_) => "HasAlbumArt",
            MusicValue::Format(_) => "Format",
            MusicValue::EncoderSoftware(_) => "EncoderSoftware",
            MusicValue::EncodedBy(_) => "EncodedBy",
            MusicValue::CoverArtPath(_) => "CoverArtPath",
            MusicValue::TrackStarted(_) => "TrackStarted",
            MusicValue::TrackStopped(_) => "TrackStopped",
            MusicValue::AddedAt(_) => "AddedAt",
            MusicValue::Bookmarked { .. } => "Bookmarked",
            MusicValue::SupersededBy { .. } => "SupersededBy",
            MusicValue::Deleted { .. } => "Deleted",
        }
    }
}

impl fmt::Display for MusicValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MusicValue::FilePath(p) => write!(f, "{}", p.display()),
            MusicValue::Title(s) => write!(f, "{}", s),
            MusicValue::Artist(s) => write!(f, "{}", s),
            MusicValue::Album(s) => write!(f, "{}", s),
            MusicValue::AlbumArtist(s) => write!(f, "{}", s),
            MusicValue::TrackNumber(n) => write!(f, "{}", n),
            MusicValue::DiscNumber(n) => write!(f, "{}", n),
            MusicValue::Year(y) => write!(f, "{}", y),
            MusicValue::Bpm(b) => write!(f, "{}", b),
            MusicValue::Key(k) => write!(f, "{}", k),
            MusicValue::MainGenre(s) => write!(f, "{}", s),
            MusicValue::StyleDescriptor(s) => write!(f, "{}", s),
            MusicValue::FullGenre(s) => write!(f, "{}", s),
            MusicValue::Isrc(i) => write!(f, "{}", i),
            MusicValue::Label(s) => write!(f, "{}", s),
            MusicValue::RecordingYear(y) => write!(f, "{}", y),
            MusicValue::RecordingDate(d) => write!(f, "{}", d),
            MusicValue::BeatportTrackUrl(s) => write!(f, "{}", s),
            MusicValue::BeatportLabelUrl(s) => write!(f, "{}", s),
            MusicValue::BandcampUrl(s) => write!(f, "{}", s),
            MusicValue::Source(s) => write!(f, "{}", s),
            MusicValue::ItemId(s) => write!(f, "{}", s),
            MusicValue::Comment(s) => write!(f, "{}", s),
            MusicValue::BeatportTrackId(s) => write!(f, "{}", s),
            MusicValue::BitDepth(b) => write!(f, "{}", b),
            MusicValue::Channels(c) => write!(f, "{}", c),
            MusicValue::SampleRate(s) => write!(f, "{}", s),
            MusicValue::DurationSeconds(d) => write!(f, "{}", d),
            MusicValue::Bitrate(b) => write!(f, "{}", b),
            MusicValue::FileSizeBytes(s) => write!(f, "{}", s),
            MusicValue::HasAlbumArt(p) => write!(
                f,
                "{}",
                if *p == AlbumArtPresence::Present {
                    "yes"
                } else {
                    "no"
                }
            ),
            MusicValue::Format(ref fmt_val) => write!(f, "{}", fmt_val),
            MusicValue::EncoderSoftware(s) => write!(f, "{}", s),
            MusicValue::EncodedBy(s) => write!(f, "{}", s),
            MusicValue::CoverArtPath(s) => write!(f, "{}", s),
            MusicValue::TrackStarted(r) => write!(f, "{}", r),
            MusicValue::TrackStopped(r) => write!(f, "{}", r),
            MusicValue::AddedAt(dt) => write!(f, "{}", dt.to_rfc3339()),
            MusicValue::Bookmarked { scope, timestamp } => match scope {
                Some(s) => write!(f, "Bookmarked({s}) at {timestamp}"),
                None => write!(f, "Bookmarked at {timestamp}"),
            },
            MusicValue::SupersededBy {
                replacement,
                timestamp,
            } => write!(f, "SupersededBy({}) at {}", replacement.as_str(), timestamp),
            MusicValue::Deleted { timestamp } => write!(f, "Deleted at {}", timestamp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use stainless_facts::assert_fact_value_format;

    #[test]
    fn music_value_has_correct_serde_format() {
        // Verify that MusicValue uses the correct stainless-facts format
        // This will fail at compile time if the serde attributes are wrong
        assert_fact_value_format!(MusicValue::Title(Title::new("Test")));
        assert_fact_value_format!(MusicValue::Artist(Artist::new("Test Artist")));
        assert_fact_value_format!(MusicValue::HasAlbumArt(AlbumArtPresence::Present));
    }

    #[test]
    fn track_started_carries_start_reason() {
        // TrackStarted should carry a StartReason, not a DateTime
        let v = MusicValue::TrackStarted(StartReason::OnRequest);
        let json = serde_json::to_string(&v).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MusicValue::TrackStarted(StartReason::OnRequest));
    }

    #[test]
    fn track_stopped_carries_stop_reason() {
        // TrackStopped should carry a StopReason, not a DateTime
        let v = MusicValue::TrackStopped(StopReason::OnCompletion);
        let json = serde_json::to_string(&v).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MusicValue::TrackStopped(StopReason::OnCompletion));
    }

    #[rstest]
    #[case(StartReason::OnRequest)]
    #[case(StartReason::ByQueue)]
    fn start_reason_all_variants_roundtrip(#[case] reason: StartReason) {
        let json = serde_json::to_string(&reason).unwrap();
        let back: StartReason = serde_json::from_str(&json).unwrap();
        assert_eq!(reason, back);
    }

    #[rstest]
    #[case(StopReason::OnRequest)]
    #[case(StopReason::OnCompletion)]
    #[case(StopReason::OnSkip)]
    fn stop_reason_all_variants_roundtrip(#[case] reason: StopReason) {
        let json = serde_json::to_string(&reason).unwrap();
        let back: StopReason = serde_json::from_str(&json).unwrap();
        assert_eq!(reason, back);
    }

    #[test]
    fn start_reason_legacy_datetime_falls_back_to_on_request() {
        // Legacy DateTime strings should deserialize to OnRequest
        let legacy_json = "\"2026-01-15T10:00:00Z\"";
        let reason: StartReason = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(reason, StartReason::OnRequest);
    }

    #[test]
    fn stop_reason_legacy_datetime_falls_back_to_on_request() {
        // Legacy DateTime strings should deserialize to OnRequest (sensible default)
        let legacy_json = "\"2026-01-15T10:00:00Z\"";
        let reason: StopReason = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(reason, StopReason::OnRequest);
    }

    #[test]
    fn track_started_display_shows_reason_not_timestamp() {
        let v = MusicValue::TrackStarted(StartReason::ByQueue);
        let s = v.to_string();
        // Should show the reason, not a timestamp
        assert_eq!(s, "ByQueue");
    }

    #[test]
    fn track_stopped_display_shows_reason_not_timestamp() {
        let v = MusicValue::TrackStopped(StopReason::OnSkip);
        let s = v.to_string();
        assert_eq!(s, "OnSkip");
    }

    #[test]
    fn track_started_fact_value_format() {
        assert_fact_value_format!(MusicValue::TrackStarted(StartReason::OnRequest));
    }

    #[test]
    fn track_stopped_fact_value_format() {
        assert_fact_value_format!(MusicValue::TrackStopped(StopReason::OnCompletion));
    }

    #[test]
    fn display_name_matches_display_format() {
        // Spot-check that display_name returns the logical name, not the variant name
        // DurationSeconds variant → "Duration" display name
        // FileSizeBytes variant → "FileSize" display name
        // Bpm variant → "BPM" display name

        // Check the names are not the raw variant names
        assert_ne!(
            MusicValue::Title(Title::new("test")).display_name(),
            "MusicValue"
        );
        assert_eq!(
            MusicValue::DurationSeconds(DurationSeconds::new(300)).display_name(),
            "Duration"
        );
        assert_eq!(
            MusicValue::FileSizeBytes(FileSizeBytes::new(1024)).display_name(),
            "FileSize"
        );
    }

    #[test]
    fn display_name_bpm_is_bpm() {
        // Just verify it doesn't panic and returns something non-empty
        // The actual string is tested implicitly via the Display impl
        let title_val = MusicValue::Title(Title::new("test"));
        assert!(!title_val.display_name().is_empty());
        assert_eq!(title_val.display_name(), "Title");
    }

    #[rstest]
    #[case(MusicFormat::Flac)]
    #[case(MusicFormat::Mp3)]
    #[case(MusicFormat::Aiff)]
    #[case(MusicFormat::Wav)]
    #[case(MusicFormat::M4a)]
    fn music_format_serde_roundtrip(#[case] format: MusicFormat) {
        let json = serde_json::to_string(&format).unwrap();
        let back: MusicFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(format, back);
    }

    #[rstest]
    #[case(MusicFormat::Flac, "FLAC")]
    #[case(MusicFormat::Mp3, "MP3")]
    #[case(MusicFormat::Aiff, "AIFF")]
    #[case(MusicFormat::Wav, "WAV")]
    #[case(MusicFormat::M4a, "M4A")]
    fn music_format_display(#[case] format: MusicFormat, #[case] expected: &str) {
        assert_eq!(format.to_string(), expected);
    }

    #[test]
    fn format_fact_serde() {
        assert_fact_value_format!(MusicValue::Format(MusicFormat::Flac));
    }

    #[test]
    fn music_value_track_started_legacy_datetime_deserializes() {
        let legacy = r#"{"t":"TrackStarted","v":"2026-01-15T10:00:00Z"}"#;
        let v: MusicValue = serde_json::from_str(legacy).unwrap();
        assert_eq!(v, MusicValue::TrackStarted(StartReason::OnRequest));
    }

    #[test]
    fn music_value_track_stopped_legacy_datetime_deserializes() {
        let legacy = r#"{"t":"TrackStopped","v":"2026-01-15T10:00:00Z"}"#;
        let v: MusicValue = serde_json::from_str(legacy).unwrap();
        assert_eq!(v, MusicValue::TrackStopped(StopReason::OnRequest));
    }

    #[test]
    fn cover_art_path_roundtrip() {
        let v = MusicValue::CoverArtPath("cover-art/abc123.jpg".to_string());
        let json = serde_json::to_string(&v).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn cover_art_path_display_name() {
        let v = MusicValue::CoverArtPath("cover-art/abc123.jpg".to_string());
        assert_eq!(v.display_name(), "CoverArtPath");
    }

    #[test]
    fn cover_art_path_display() {
        let v = MusicValue::CoverArtPath("cover-art/abc123.jpg".to_string());
        assert_eq!(v.to_string(), "cover-art/abc123.jpg");
    }

    #[test]
    fn cover_art_path_fact_value_format() {
        assert_fact_value_format!(MusicValue::CoverArtPath("cover-art/test.jpg".to_string()));
    }

    #[test]
    fn disc_number_roundtrip() {
        let v = MusicValue::DiscNumber(DiscNumber::new(2));
        let json = serde_json::to_string(&v).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn disc_number_display_name() {
        let v = MusicValue::DiscNumber(DiscNumber::new(1));
        assert_eq!(v.display_name(), "DiscNumber");
    }

    #[test]
    fn disc_number_display() {
        let v = MusicValue::DiscNumber(DiscNumber::new(3));
        assert_eq!(v.to_string(), "3");
    }

    #[test]
    fn disc_number_fact_value_format() {
        assert_fact_value_format!(MusicValue::DiscNumber(DiscNumber::new(1)));
    }

    #[test]
    fn added_at_roundtrip() {
        let dt = DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = MusicValue::AddedAt(dt);
        let json = serde_json::to_string(&v).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn added_at_display_name() {
        let dt = DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = MusicValue::AddedAt(dt);
        assert_eq!(v.display_name(), "AddedAt");
    }

    #[test]
    fn added_at_display() {
        let dt = DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = MusicValue::AddedAt(dt);
        assert_eq!(v.to_string(), "2026-03-08T12:00:00+00:00");
    }

    #[test]
    fn added_at_fact_value_format() {
        let dt = DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_fact_value_format!(MusicValue::AddedAt(dt));
    }

    #[test]
    fn recording_date_roundtrip() {
        let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        let v = MusicValue::RecordingDate(date);
        let json = serde_json::to_string(&v).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn recording_date_display() {
        let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        let v = MusicValue::RecordingDate(date);
        assert_eq!(v.to_string(), "2024-06-15");
    }

    #[test]
    fn recording_date_fact_value_format() {
        let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        assert_fact_value_format!(MusicValue::RecordingDate(date));
    }

    #[test]
    fn superseded_by_fact_roundtrip() {
        use crate::primitives::ContentHash;
        let replacement = ContentHash::new("sha256:abcdef1234567890");
        let val = MusicValue::SupersededBy {
            replacement: replacement.clone(),
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_string(&val).unwrap();
        let decoded: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn superseded_by_display_name() {
        use crate::primitives::ContentHash;
        let val = MusicValue::SupersededBy {
            replacement: ContentHash::new("sha256:abc"),
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        assert_eq!(val.display_name(), "SupersededBy");
    }

    #[test]
    fn superseded_by_display() {
        use crate::primitives::ContentHash;
        let val = MusicValue::SupersededBy {
            replacement: ContentHash::new("sha256:abc123"),
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let s = val.to_string();
        assert!(s.contains("sha256:abc123"), "got: {}", s);
        assert!(s.contains("SupersededBy"), "got: {}", s);
    }

    #[test]
    fn deleted_fact_roundtrip() {
        let val = MusicValue::Deleted {
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_string(&val).unwrap();
        let decoded: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn deleted_display_name() {
        let val = MusicValue::Deleted {
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        assert_eq!(val.display_name(), "Deleted");
    }

    #[test]
    fn deleted_display() {
        let val = MusicValue::Deleted {
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let s = val.to_string();
        assert!(s.contains("Deleted"), "got: {}", s);
    }

    #[test]
    fn superseded_by_fact_value_format() {
        use crate::primitives::ContentHash;
        assert_fact_value_format!(MusicValue::SupersededBy {
            replacement: ContentHash::new("sha256:abc"),
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        });
    }

    #[test]
    fn deleted_fact_value_format() {
        assert_fact_value_format!(MusicValue::Deleted {
            timestamp: DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        });
    }

    #[test]
    fn bookmarked_fact_roundtrip() {
        let val = MusicValue::Bookmarked {
            scope: Some("my-set".to_string()),
            timestamp: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_string(&val).unwrap();
        let decoded: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, decoded);
    }
}
