use serde::{Deserialize, Serialize};
use std::fmt;

pub use music_primitives::ContentHash;

/// International Standard Recording Code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Isrc(String);

impl Isrc {
    pub fn new(val: impl Into<String>) -> Self {
        Self(val.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Isrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Track number on album
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackNumber(u32);

impl TrackNumber {
    pub fn new(val: u32) -> Self {
        Self(val)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for TrackNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Year of release
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Year(u32);

impl Year {
    pub fn new(val: u32) -> Self {
        Self(val)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Year {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Audio bit depth (16 or 24 bit typically)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitDepth(u8);

impl BitDepth {
    pub fn new(val: u8) -> Self {
        Self(val)
    }

    pub fn value(self) -> u8 {
        self.0
    }
}

impl fmt::Display for BitDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-bit", self.0)
    }
}

/// Number of audio channels (1 = mono, 2 = stereo)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Channels(u8);

impl Channels {
    pub fn new(val: u8) -> Self {
        Self(val)
    }

    pub fn value(self) -> u8 {
        self.0
    }
}

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
pub struct SampleRate(u32);

impl SampleRate {
    pub fn new(val: u32) -> Self {
        Self(val)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

/// Duration in seconds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationSeconds(u32);

impl DurationSeconds {
    pub fn new(val: u32) -> Self {
        Self(val)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for DurationSeconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mins = self.0 / 60;
        let secs = self.0 % 60;
        write!(f, "{}:{:02}", mins, secs)
    }
}

pub use storage_primitives::ByteSize as FileSizeBytes;

/// Bitrate in kbps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bitrate(u32);

impl Bitrate {
    pub fn new(val: u32) -> Self {
        Self(val)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Bitrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} kbps", self.0)
    }
}

/// Artist name
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct Artist(String);

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
pub struct Title(String);

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
pub struct Album(String);

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
