use crate::primitives::*;
use music_primitives::{Bpm, Key};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// All possible metadata values for a music track
///
/// Each variant represents a single fact that can be asserted or retracted
/// about a track. Facts are stored in the stainless-facts stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    Title(String),

    /// Artist name
    Artist(String),

    /// Album name
    Album(String),

    /// Album artist (for compilations)
    AlbumArtist(String),

    /// Track number on album
    TrackNumber(TrackNumber),

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

    /// Full recording date (when available, format: YYYY-MM-DD)
    RecordingDate(String),

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
    HasAlbumArt(bool),

    // ========================================================================
    // Encoder Information
    // ========================================================================
    /// Encoder software (e.g., "Beatport", "reference libFLAC 1.3.3 20190804")
    EncoderSoftware(String),

    /// Who encoded the file (e.g., "Beatport")
    EncodedBy(String),
}

impl MusicValue {
    /// Returns the variant name for display (e.g., "Title", "Artist", "BPM")
    pub fn variant_name(&self) -> &'static str {
        match self {
            MusicValue::FilePath(_) => "FilePath",
            MusicValue::Title(_) => "Title",
            MusicValue::Artist(_) => "Artist",
            MusicValue::Album(_) => "Album",
            MusicValue::AlbumArtist(_) => "AlbumArtist",
            MusicValue::TrackNumber(_) => "TrackNumber",
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
            MusicValue::Comment(_) => "Comment",
            MusicValue::BeatportTrackId(_) => "BeatportTrackId",
            MusicValue::BitDepth(_) => "BitDepth",
            MusicValue::Channels(_) => "Channels",
            MusicValue::SampleRate(_) => "SampleRate",
            MusicValue::DurationSeconds(_) => "Duration",
            MusicValue::Bitrate(_) => "Bitrate",
            MusicValue::FileSizeBytes(_) => "FileSize",
            MusicValue::HasAlbumArt(_) => "HasAlbumArt",
            MusicValue::EncoderSoftware(_) => "EncoderSoftware",
            MusicValue::EncodedBy(_) => "EncodedBy",
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
            MusicValue::Year(y) => write!(f, "{}", y),
            MusicValue::Bpm(b) => write!(f, "{}", b),
            MusicValue::Key(k) => write!(f, "{}", k),
            MusicValue::MainGenre(s) => write!(f, "{}", s),
            MusicValue::StyleDescriptor(s) => write!(f, "{}", s),
            MusicValue::FullGenre(s) => write!(f, "{}", s),
            MusicValue::Isrc(i) => write!(f, "{}", i),
            MusicValue::Label(s) => write!(f, "{}", s),
            MusicValue::RecordingYear(y) => write!(f, "{}", y),
            MusicValue::RecordingDate(s) => write!(f, "{}", s),
            MusicValue::BeatportTrackUrl(s) => write!(f, "{}", s),
            MusicValue::BeatportLabelUrl(s) => write!(f, "{}", s),
            MusicValue::BandcampUrl(s) => write!(f, "{}", s),
            MusicValue::Comment(s) => write!(f, "{}", s),
            MusicValue::BeatportTrackId(s) => write!(f, "{}", s),
            MusicValue::BitDepth(b) => write!(f, "{}", b),
            MusicValue::Channels(c) => write!(f, "{}", c),
            MusicValue::SampleRate(s) => write!(f, "{}", s),
            MusicValue::DurationSeconds(d) => write!(f, "{}", d),
            MusicValue::Bitrate(b) => write!(f, "{}", b),
            MusicValue::FileSizeBytes(s) => write!(f, "{}", s),
            MusicValue::HasAlbumArt(b) => write!(f, "{}", if *b { "yes" } else { "no" }),
            MusicValue::EncoderSoftware(s) => write!(f, "{}", s),
            MusicValue::EncodedBy(s) => write!(f, "{}", s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stainless_facts::assert_fact_value_format;

    #[test]
    fn music_value_has_correct_serde_format() {
        // Verify that MusicValue uses the correct stainless-facts format
        // This will fail at compile time if the serde attributes are wrong
        assert_fact_value_format!(MusicValue::Title("Test".to_string()));
        assert_fact_value_format!(MusicValue::Artist("Test Artist".to_string()));
        assert_fact_value_format!(MusicValue::HasAlbumArt(true));
    }
}
