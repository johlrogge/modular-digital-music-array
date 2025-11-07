use serde::{Deserialize, Serialize};

// ============================================================================
// Primitive Types (newtype pattern)
// ============================================================================

/// SHA256 hash of file contents - used as entity ID
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct ContentHash(pub String);

impl ContentHash {
    pub fn new(hash: String) -> Self {
        Self(hash)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Beats per minute as integer (125, 126, 128, etc)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Bpm(pub u16);

impl Bpm {
    pub fn new(bpm: u16) -> Self {
        Self(bpm)
    }

    pub fn value(&self) -> u16 {
        self.0
    }
}

/// Musical key (e.g., "F Major", "Eb Major", "D Minor")
/// Note: Different sources may use different representations
/// Each source should convert to a canonical format
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct MusicalKey(pub String);

impl MusicalKey {
    pub fn new(key: String) -> Self {
        Self(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// International Standard Recording Code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct Isrc(pub String);

impl Isrc {
    pub fn new(isrc: String) -> Self {
        Self(isrc)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Track number within an album
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct TrackNumber(pub u32);

impl TrackNumber {
    pub fn new(number: u32) -> Self {
        Self(number)
    }

    pub fn value(&self) -> u32 {
        self.0
    }
}

/// Year of recording or release
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Year(pub u32);

impl Year {
    pub fn new(year: u32) -> Self {
        Self(year)
    }

    pub fn value(&self) -> u32 {
        self.0
    }
}

/// Audio bit depth (16 or 24 bit)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct BitDepth(pub u8);

impl BitDepth {
    pub fn new(depth: u8) -> Self {
        Self(depth)
    }

    pub fn value(&self) -> u8 {
        self.0
    }
}

/// Number of audio channels (1 = mono, 2 = stereo)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Channels(pub u8);

impl Channels {
    pub fn new(channels: u8) -> Self {
        Self(channels)
    }

    pub fn value(&self) -> u8 {
        self.0
    }
}

/// Sample rate in Hz (44100, 48000, etc)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct SampleRate(pub u32);

impl SampleRate {
    pub fn new(rate: u32) -> Self {
        Self(rate)
    }

    pub fn value(&self) -> u32 {
        self.0
    }
}

/// Duration in whole seconds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct DurationSeconds(pub u32);

impl DurationSeconds {
    pub fn new(seconds: u32) -> Self {
        Self(seconds)
    }

    pub fn value(&self) -> u32 {
        self.0
    }

    pub fn from_f64(seconds: f64) -> Self {
        Self(seconds.round() as u32)
    }
}

/// File size in bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct FileSizeBytes(pub u64);

impl FileSizeBytes {
    pub fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

/// Bitrate in kbps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Bitrate(pub u32);

impl Bitrate {
    pub fn new(kbps: u32) -> Self {
        Self(kbps)
    }

    pub fn value(&self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_ordering() {
        let bpm1 = Bpm::new(120);
        let bpm2 = Bpm::new(140);
        assert!(bpm1 < bpm2);
    }

    #[test]
    fn duration_from_float() {
        let duration = DurationSeconds::from_f64(400.789);
        assert_eq!(duration.value(), 401);
    }

    #[test]
    fn content_hash_equality() {
        let hash1 = ContentHash::new("abc123".to_string());
        let hash2 = ContentHash::new("abc123".to_string());
        let hash3 = ContentHash::new("def456".to_string());
        
        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }
}
