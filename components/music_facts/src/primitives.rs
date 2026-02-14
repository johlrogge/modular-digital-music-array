use serde::{Deserialize, Serialize};
use std::fmt;

/// Content hash of audio file (SHA256)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct ContentHash(pub String);

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// International Standard Recording Code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Isrc(pub String);

impl fmt::Display for Isrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Track number on album
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackNumber(pub u32);

impl fmt::Display for TrackNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Year of release
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Year(pub u32);

impl fmt::Display for Year {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Audio bit depth (16 or 24 bit typically)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitDepth(pub u8);

impl fmt::Display for BitDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-bit", self.0)
    }
}

/// Number of audio channels (1 = mono, 2 = stereo)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Channels(pub u8);

impl fmt::Display for Channels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            1 => write!(f, "mono"),
            2 => write!(f, "stereo"),
            n => write!(f, "{} channels", n),
        }
    }
}

/// Sample rate in Hz (44100, 48000, etc)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleRate(pub u32);

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

/// Duration in seconds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationSeconds(pub u32);

impl fmt::Display for DurationSeconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mins = self.0 / 60;
        let secs = self.0 % 60;
        write!(f, "{}:{:02}", mins, secs)
    }
}

/// File size in bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileSizeBytes(pub u64);

impl fmt::Display for FileSizeBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0;
        if bytes >= 1_000_000_000 {
            write!(f, "{:.2} GB", bytes as f64 / 1_000_000_000.0)
        } else if bytes >= 1_000_000 {
            write!(f, "{:.2} MB", bytes as f64 / 1_000_000.0)
        } else if bytes >= 1_000 {
            write!(f, "{:.2} KB", bytes as f64 / 1_000.0)
        } else {
            write!(f, "{} bytes", bytes)
        }
    }
}

/// Bitrate in kbps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bitrate(pub u32);

impl fmt::Display for Bitrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} kbps", self.0)
    }
}

/// Artist name
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct Artist(pub String);

impl fmt::Display for Artist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Artist {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Track or album title
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct Title(pub String);

impl fmt::Display for Title {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Title {
    pub fn new(title: impl Into<String>) -> Self {
        Self(title.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Album name
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct Album(pub String);

impl fmt::Display for Album {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Album {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
