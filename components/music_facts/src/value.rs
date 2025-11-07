use crate::primitives::*;
use music_primitives::{Bpm, Key};
use serde::{Deserialize, Serialize};
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
