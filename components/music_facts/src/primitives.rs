use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Content hash of audio file (SHA256)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct ContentHash(pub String);

/// International Standard Recording Code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Isrc(pub String);

/// Track number on album
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackNumber(pub u32);

/// Year of release
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Year(pub u32);

/// Audio bit depth (16 or 24 bit typically)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitDepth(pub u8);

/// Number of audio channels (1 = mono, 2 = stereo)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Channels(pub u8);

/// Sample rate in Hz (44100, 48000, etc)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleRate(pub u32);

/// Duration in seconds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationSeconds(pub u32);

/// File size in bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileSizeBytes(pub u64);

/// Bitrate in kbps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bitrate(pub u32);
