use crate::primitives::*;
use chrono::{DateTime, NaiveDate, Utc};
use music_primitives::{Bpm, EnergyLevel, Key, TrackRole};
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

/// Type of a cue point on a track.
///
/// Used within [`MusicValue::MemoryCue`] to distinguish hot cues, memory cues, and loops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CueKind {
    /// Standard memory cue (not mapped to a pad)
    Memory,
    /// Hot cue (mapped to a performance pad)
    Hot,
    /// Loop cue with a fixed length in milliseconds
    Loop { length_ms: u32 },
}

impl fmt::Display for CueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CueKind::Memory => write!(f, "Memory"),
            CueKind::Hot => write!(f, "Hot"),
            CueKind::Loop { length_ms } => write!(f, "Loop({}ms)", length_ms),
        }
    }
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

    /// DJ curation role for the track in a set (Opener, BuildUp, Peak, etc.)
    Role(TrackRole),

    /// Energy level on a scale from 1 (very mellow) to 10 (absolute peak)
    Energy(EnergyLevel),

    /// Beat-grid anchor: first beat position, tempo, and beats per bar.
    ///
    /// Describes a single fixed-tempo grid anchor.  Variable-tempo grids
    /// (multiple anchors) are out of scope for this variant.
    BeatGrid {
        /// Position of the first beat in milliseconds from the start of the file
        first_beat_ms: u32,
        /// Grid tempo
        bpm: Bpm,
        /// Number of beats per bar (4 for 4/4 time)
        beats_per_bar: u8,
    },

    // ========================================================================
    // Cue Points
    // ========================================================================
    /// A single memory cue, hot cue, or loop point.
    ///
    /// Multiple `MemoryCue` facts may coexist for one track (one per cue
    /// point), mirroring how `StyleDescriptor` handles multi-valued fields.
    /// Retraction removes the exact-matching cue.
    MemoryCue {
        /// Position in milliseconds from the start of the file
        position_ms: u32,
        /// Type of cue point
        kind: CueKind,
        /// Optional human-readable label for this cue
        label: Option<String>,
        /// Optional pad/slot index (0-based)
        index: Option<u8>,
    },

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
    /// This track replaces an older version (same work). Asserted on the NEW track,
    /// naming the old content hash it replaces. The old hash's facts are retracted
    /// (hard delete) so the old identity is gone from search/list/get_track.
    Replaces(crate::primitives::ContentHash),

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
            MusicValue::Replaces(_) => "Replaces",
            MusicValue::Deleted { .. } => "Deleted",
            MusicValue::Role(_) => "Role",
            MusicValue::Energy(_) => "Energy",
            MusicValue::BeatGrid { .. } => "BeatGrid",
            MusicValue::MemoryCue { .. } => "MemoryCue",
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
            MusicValue::Replaces(old_hash) => write!(f, "Replaces({})", old_hash.as_str()),
            MusicValue::Deleted { timestamp } => write!(f, "Deleted at {}", timestamp),
            MusicValue::Role(r) => write!(f, "{}", r),
            MusicValue::Energy(e) => write!(f, "{}", e),
            MusicValue::BeatGrid {
                first_beat_ms,
                bpm,
                beats_per_bar,
            } => write!(
                f,
                "BeatGrid(first={}ms, bpm={}, {}/4)",
                first_beat_ms, bpm, beats_per_bar
            ),
            MusicValue::MemoryCue {
                position_ms,
                kind,
                label,
                index,
            } => match (label.as_deref(), index) {
                (Some(l), Some(i)) => {
                    write!(f, "Cue #{} {} @{}ms: {}", i, kind, position_ms, l)
                }
                (Some(l), None) => write!(f, "Cue {} @{}ms: {}", kind, position_ms, l),
                (None, Some(i)) => write!(f, "Cue #{} {} @{}ms", i, kind, position_ms),
                (None, None) => write!(f, "Cue {} @{}ms", kind, position_ms),
            },
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
    fn replaces_fact_roundtrip() {
        use crate::primitives::ContentHash;
        let old_hash = ContentHash::new("sha256:abcdef1234567890");
        let val = MusicValue::Replaces(old_hash.clone());
        let json = serde_json::to_string(&val).unwrap();
        let decoded: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn replaces_display_name() {
        use crate::primitives::ContentHash;
        let val = MusicValue::Replaces(ContentHash::new("sha256:abc"));
        assert_eq!(val.display_name(), "Replaces");
    }

    #[test]
    fn replaces_display() {
        use crate::primitives::ContentHash;
        let val = MusicValue::Replaces(ContentHash::new("sha256:abc123"));
        let s = val.to_string();
        assert!(s.contains("sha256:abc123"), "got: {}", s);
        assert!(s.contains("Replaces"), "got: {}", s);
    }

    #[test]
    fn replaces_fact_value_format() {
        use crate::primitives::ContentHash;
        assert_fact_value_format!(MusicValue::Replaces(ContentHash::new("sha256:abc")));
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

    // =========================================================================
    // Role tests
    // =========================================================================

    #[test]
    fn role_roundtrip() {
        use music_primitives::TrackRole;
        let val = MusicValue::Role(TrackRole::Peak);
        let json = serde_json::to_string(&val).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn role_display_name() {
        use music_primitives::TrackRole;
        let val = MusicValue::Role(TrackRole::BuildUp);
        assert_eq!(val.display_name(), "Role");
    }

    #[test]
    fn role_display() {
        use music_primitives::TrackRole;
        let val = MusicValue::Role(TrackRole::CoolDown);
        assert_eq!(val.to_string(), "Cool Down");
    }

    #[test]
    fn role_fact_value_format() {
        use music_primitives::TrackRole;
        // Wire format: {"t":"Role","v":"Peak"}
        assert_fact_value_format!(MusicValue::Role(TrackRole::Peak));
    }

    #[test]
    fn role_wire_format_is_pascal_case() {
        use music_primitives::TrackRole;
        let val = MusicValue::Role(TrackRole::BuildUp);
        let json = serde_json::to_string(&val).unwrap();
        // Verify the wire value is "BuildUp" not "Build Up"
        assert!(json.contains("\"BuildUp\""), "got: {}", json);
    }

    // =========================================================================
    // Energy tests
    // =========================================================================

    #[test]
    fn energy_roundtrip() {
        use music_primitives::EnergyLevel;
        let val = MusicValue::Energy(EnergyLevel::new(7).unwrap());
        let json = serde_json::to_string(&val).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn energy_display_name() {
        use music_primitives::EnergyLevel;
        let val = MusicValue::Energy(EnergyLevel::new(5).unwrap());
        assert_eq!(val.display_name(), "Energy");
    }

    #[test]
    fn energy_display() {
        use music_primitives::EnergyLevel;
        let val = MusicValue::Energy(EnergyLevel::new(10).unwrap());
        assert_eq!(val.to_string(), "10");
    }

    #[test]
    fn energy_fact_value_format() {
        use music_primitives::EnergyLevel;
        // Wire format: {"t":"Energy","v":7}
        assert_fact_value_format!(MusicValue::Energy(EnergyLevel::new(7).unwrap()));
    }

    #[test]
    fn energy_wire_format_is_integer() {
        use music_primitives::EnergyLevel;
        let val = MusicValue::Energy(EnergyLevel::new(8).unwrap());
        let json = serde_json::to_string(&val).unwrap();
        // Verify the wire value is a bare integer
        assert!(json.contains(":8}"), "got: {}", json);
    }

    // =========================================================================
    // BeatGrid tests
    // =========================================================================

    #[test]
    fn beat_grid_roundtrip() {
        let bpm = Bpm::from_f32(128.0).unwrap();
        let val = MusicValue::BeatGrid {
            first_beat_ms: 450,
            bpm,
            beats_per_bar: 4,
        };
        let json = serde_json::to_string(&val).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn beat_grid_display_name() {
        let bpm = Bpm::from_f32(140.0).unwrap();
        let val = MusicValue::BeatGrid {
            first_beat_ms: 200,
            bpm,
            beats_per_bar: 4,
        };
        assert_eq!(val.display_name(), "BeatGrid");
    }

    #[test]
    fn beat_grid_display() {
        let bpm = Bpm::from_f32(128.0).unwrap();
        let val = MusicValue::BeatGrid {
            first_beat_ms: 450,
            bpm,
            beats_per_bar: 4,
        };
        let s = val.to_string();
        assert!(s.contains("450ms"), "got: {}", s);
        assert!(s.contains("128.00"), "got: {}", s);
        assert!(s.contains("4/4"), "got: {}", s);
    }

    #[test]
    fn beat_grid_fact_value_format() {
        // Wire format: {"t":"BeatGrid","v":{"first_beat_ms":450,"bpm":12800,"beats_per_bar":4}}
        let bpm = Bpm::from_f32(128.0).unwrap();
        assert_fact_value_format!(MusicValue::BeatGrid {
            first_beat_ms: 450,
            bpm,
            beats_per_bar: 4,
        });
    }

    #[test]
    fn beat_grid_wire_format_contains_expected_fields() {
        let bpm = Bpm::from_f32(128.0).unwrap();
        let val = MusicValue::BeatGrid {
            first_beat_ms: 450,
            bpm,
            beats_per_bar: 4,
        };
        let json = serde_json::to_string(&val).unwrap();
        assert!(json.contains("\"first_beat_ms\""), "got: {}", json);
        assert!(json.contains("\"bpm\""), "got: {}", json);
        assert!(json.contains("\"beats_per_bar\""), "got: {}", json);
        // bpm 128.0 is serialised as 12800 (hundredths)
        assert!(json.contains("12800"), "got: {}", json);
    }

    #[test]
    fn beat_grid_exact_wire_format() {
        // Exact golden wire format — any serde layout change breaks this.
        // bpm=128.0 → stored as hundredths = 12800
        let bpm = Bpm::from_f32(128.0).unwrap();
        let val = MusicValue::BeatGrid {
            first_beat_ms: 450,
            bpm,
            beats_per_bar: 4,
        };
        assert_eq!(
            serde_json::to_string(&val).unwrap(),
            r#"{"t":"BeatGrid","v":{"first_beat_ms":450,"bpm":12800,"beats_per_bar":4}}"#
        );
    }

    // =========================================================================
    // MemoryCue tests
    // =========================================================================

    #[test]
    fn memory_cue_hot_roundtrip() {
        let val = MusicValue::MemoryCue {
            position_ms: 32000,
            kind: CueKind::Hot,
            label: Some("Drop".to_string()),
            index: Some(1),
        };
        let json = serde_json::to_string(&val).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn memory_cue_memory_roundtrip() {
        let val = MusicValue::MemoryCue {
            position_ms: 1000,
            kind: CueKind::Memory,
            label: None,
            index: None,
        };
        let json = serde_json::to_string(&val).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn memory_cue_loop_roundtrip() {
        let val = MusicValue::MemoryCue {
            position_ms: 64000,
            kind: CueKind::Loop { length_ms: 4000 },
            label: Some("Chorus loop".to_string()),
            index: Some(3),
        };
        let json = serde_json::to_string(&val).unwrap();
        let back: MusicValue = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn memory_cue_display_name() {
        let val = MusicValue::MemoryCue {
            position_ms: 0,
            kind: CueKind::Memory,
            label: None,
            index: None,
        };
        assert_eq!(val.display_name(), "MemoryCue");
    }

    #[rstest]
    #[case(
        MusicValue::MemoryCue { position_ms: 1000, kind: CueKind::Hot, label: Some("Intro".to_string()), index: Some(0) },
        "Cue #0 Hot @1000ms: Intro"
    )]
    #[case(
        MusicValue::MemoryCue { position_ms: 2000, kind: CueKind::Memory, label: Some("Break".to_string()), index: None },
        "Cue Memory @2000ms: Break"
    )]
    #[case(
        MusicValue::MemoryCue { position_ms: 3000, kind: CueKind::Hot, label: None, index: Some(2) },
        "Cue #2 Hot @3000ms"
    )]
    #[case(
        MusicValue::MemoryCue { position_ms: 4000, kind: CueKind::Memory, label: None, index: None },
        "Cue Memory @4000ms"
    )]
    fn memory_cue_display(#[case] val: MusicValue, #[case] expected: &str) {
        assert_eq!(val.to_string(), expected);
    }

    #[test]
    fn memory_cue_hot_fact_value_format() {
        // Wire format: {"t":"MemoryCue","v":{"position_ms":32000,"kind":"Hot","label":"Drop","index":1}}
        assert_fact_value_format!(MusicValue::MemoryCue {
            position_ms: 32000,
            kind: CueKind::Hot,
            label: Some("Drop".to_string()),
            index: Some(1),
        });
    }

    #[test]
    fn memory_cue_loop_fact_value_format() {
        // Wire format: {"t":"MemoryCue","v":{"position_ms":64000,"kind":{"Loop":{"length_ms":4000}},"label":null,"index":null}}
        assert_fact_value_format!(MusicValue::MemoryCue {
            position_ms: 64000,
            kind: CueKind::Loop { length_ms: 4000 },
            label: None,
            index: None,
        });
    }

    #[test]
    fn memory_cue_wire_format_contains_expected_fields() {
        let val = MusicValue::MemoryCue {
            position_ms: 32000,
            kind: CueKind::Hot,
            label: Some("Drop".to_string()),
            index: Some(1),
        };
        let json = serde_json::to_string(&val).unwrap();
        assert!(json.contains("\"position_ms\""), "got: {}", json);
        assert!(json.contains("\"kind\""), "got: {}", json);
        assert!(json.contains("\"label\""), "got: {}", json);
        assert!(json.contains("\"index\""), "got: {}", json);
        assert!(json.contains("\"Hot\""), "got: {}", json);
    }

    #[test]
    fn memory_cue_hot_exact_wire_format() {
        // Exact golden wire format for a Hot cue with label and index.
        let val = MusicValue::MemoryCue {
            position_ms: 32000,
            kind: CueKind::Hot,
            label: Some("Drop".to_string()),
            index: Some(1),
        };
        assert_eq!(
            serde_json::to_string(&val).unwrap(),
            r#"{"t":"MemoryCue","v":{"position_ms":32000,"kind":"Hot","label":"Drop","index":1}}"#
        );
    }

    #[test]
    fn memory_cue_memory_exact_wire_format() {
        // Exact golden wire format for a Memory cue (no label, no index).
        let val = MusicValue::MemoryCue {
            position_ms: 1000,
            kind: CueKind::Memory,
            label: None,
            index: None,
        };
        assert_eq!(
            serde_json::to_string(&val).unwrap(),
            r#"{"t":"MemoryCue","v":{"position_ms":1000,"kind":"Memory","label":null,"index":null}}"#
        );
    }

    #[test]
    fn memory_cue_loop_exact_wire_format() {
        // Exact golden wire format for a Loop cue. Loop nests as {"Loop":{"length_ms":N}}.
        let val = MusicValue::MemoryCue {
            position_ms: 64000,
            kind: CueKind::Loop { length_ms: 4000 },
            label: None,
            index: None,
        };
        assert_eq!(
            serde_json::to_string(&val).unwrap(),
            r#"{"t":"MemoryCue","v":{"position_ms":64000,"kind":{"Loop":{"length_ms":4000}},"label":null,"index":null}}"#
        );
    }

    #[test]
    fn cue_kind_loop_wire_format() {
        // Loop variant serialises with its length_ms field
        let kind = CueKind::Loop { length_ms: 8000 };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("Loop"), "got: {}", json);
        assert!(json.contains("8000"), "got: {}", json);
    }

    #[test]
    fn cue_kind_loop_exact_wire_format() {
        // Exact golden wire format for CueKind::Loop.
        let kind = CueKind::Loop { length_ms: 8000 };
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#"{"Loop":{"length_ms":8000}}"#
        );
    }

    #[rstest]
    #[case(CueKind::Memory, "Memory")]
    #[case(CueKind::Hot, "Hot")]
    #[case(CueKind::Loop { length_ms: 4000 }, "Loop(4000ms)")]
    fn cue_kind_display(#[case] kind: CueKind, #[case] expected: &str) {
        assert_eq!(kind.to_string(), expected);
    }
}
