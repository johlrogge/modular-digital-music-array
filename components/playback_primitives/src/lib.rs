use std::fmt::Display;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Describes an available audio output device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSinkInfo {
    pub name: String,
    pub description: Option<String>,
    pub max_sample_rate: Option<u32>,
}

/// The currently active audio output configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOutputConfig {
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

/// Identifies a playback session — spans from the first track playing to the queue emptying.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Create a new session ID using the current UTC timestamp in RFC 3339 format.
    pub fn now() -> Self {
        Self(chrono::Utc::now().to_rfc3339())
    }
}

impl Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub use music_primitives::ContentHash;

#[derive(Debug, Error)]
pub enum PlaybackError {
    #[error("Invalid channel number")]
    InvalidChannel,
    #[error("Value out of range")]
    ValueOutOfRange,
}

/// Common behavior for decibel-based measurements
pub trait Db {
    fn to_linear(&self) -> f32;
    fn raw(&self) -> f32;
}

/// Volume level in dBFS (decibels full scale)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct Volume(f32);

impl Volume {
    const MIN_DB: f32 = -96.0;
    const MAX_DB: f32 = 0.0; // dBFS can't go above 0

    pub const SILENT: Self = Self(-96.0);
    pub const UNITY: Self = Self(0.0);

    pub fn new(dbfs: f32) -> Result<Self, PlaybackError> {
        if (Self::MIN_DB..=Self::MAX_DB).contains(&dbfs) {
            Ok(Self(dbfs))
        } else {
            Err(PlaybackError::ValueOutOfRange)
        }
    }
}

impl TryFrom<f32> for Volume {
    type Error = PlaybackError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Volume::new(value)
    }
}

impl From<Volume> for f32 {
    fn from(v: Volume) -> f32 {
        v.0
    }
}

impl Db for Volume {
    fn to_linear(&self) -> f32 {
        10.0f32.powf(self.0 / 20.0)
    }

    fn raw(&self) -> f32 {
        self.0
    }
}

/// Identifies a named audio source (e.g. "audio", "bandcamp").
///
/// This is a newtype around `String` to prevent confusion with other string fields
/// and to centralise the canonical "audio" default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceName(String);

impl SourceName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the canonical audio source name.
    pub fn audio() -> Self {
        Self("audio".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<SourceName> for String {
    fn from(s: SourceName) -> Self {
        s.0
    }
}

/// Identifies a playback channel (deck)
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum Deck {
    A,
    B,
}

impl Display for Deck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Deck::A => write!(f, "A"),
            Deck::B => write!(f, "B"),
        }
    }
}

impl Deck {
    pub fn new(deck: u8) -> Result<Self, PlaybackError> {
        match deck {
            0 => Ok(Self::A),
            1 => Ok(Self::B),
            _ => Err(PlaybackError::InvalidChannel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod volume {
        use super::*;
        use pretty_assertions::assert_eq;
        use rstest::rstest;

        #[test]
        fn unity_is_linear_one() {
            assert_eq!(Volume::UNITY.to_linear(), 1.0);
        }

        #[test]
        fn silent_is_near_zero() {
            assert!(Volume::SILENT.to_linear() < 0.0001);
        }

        #[rstest]
        #[case(0.0_f32, 1.0_f32)]
        #[case(-6.0, 0.501)]
        #[case(-20.0, 0.1)]
        fn converts_common_values(#[case] db: f32, #[case] expected: f32) {
            let vol = Volume::new(db).unwrap();
            let actual = vol.to_linear();
            let tolerance = expected * 0.001; // 0.1% tolerance
            assert!(
                (actual - expected).abs() <= tolerance,
                "For {}dBFS: expected {}, got {}",
                db,
                expected,
                actual
            );
        }

        #[test]
        fn rejects_positive_dbfs() {
            assert!(matches!(
                Volume::new(1.0),
                Err(PlaybackError::ValueOutOfRange)
            ));
        }

        #[test]
        fn serialization() {
            let vol = Volume::new(-6.0).unwrap();
            let json = serde_json::to_string(&vol).unwrap();
            let decoded: Volume = serde_json::from_str(&json).unwrap();
            assert_eq!(vol, decoded);
        }

        #[test]
        fn deserializing_out_of_range_returns_error() {
            let result: Result<Volume, _> = serde_json::from_str("10.0");
            assert!(
                result.is_err(),
                "expected error for out-of-range dBFS value"
            );
        }
    }

    mod source_name {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn audio_constructor_returns_audio_string() {
            let s = SourceName::audio();
            assert_eq!(s.as_str(), "audio");
        }

        #[test]
        fn new_constructor_wraps_string() {
            let s = SourceName::new("bandcamp");
            assert_eq!(s.as_str(), "bandcamp");
        }

        #[test]
        fn display_shows_inner_string() {
            let s = SourceName::new("beatport");
            assert_eq!(s.to_string(), "beatport");
        }

        #[test]
        fn serde_round_trip_transparent() {
            let s = SourceName::audio();
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, "\"audio\"");
            let decoded: SourceName = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, s);
        }

        #[test]
        fn from_source_name_into_string() {
            let s = SourceName::new("audio");
            let st: String = s.into();
            assert_eq!(st, "audio");
        }
    }

    mod channel {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn creates_channel_a() {
            assert!(matches!(Deck::new(0), Ok(Deck::A)));
        }

        #[test]
        fn creates_channel_b() {
            assert!(matches!(Deck::new(1), Ok(Deck::B)));
        }

        #[test]
        fn rejects_invalid_channel() {
            assert!(matches!(Deck::new(2), Err(PlaybackError::InvalidChannel)));
        }

        #[test]
        fn serialization() {
            let channel = Deck::A;
            let json = serde_json::to_string(&channel).unwrap();
            let decoded: Deck = serde_json::from_str(&json).unwrap();
            assert_eq!(channel, decoded);
        }
    }
}
