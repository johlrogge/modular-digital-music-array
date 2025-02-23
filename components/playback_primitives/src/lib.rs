use serde::{Deserialize, Serialize};
use thiserror::Error;

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

impl Db for Volume {
    fn to_linear(&self) -> f32 {
        10.0f32.powf(self.0 / 20.0)
    }

    fn raw(&self) -> f32 {
        self.0
    }
}

/// Identifies a playback channel (deck)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Deck {
    A,
    B,
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

    mod volume_tests {
        use super::*;

        #[test]
        fn unity_is_linear_one() {
            assert_eq!(Volume::UNITY.to_linear(), 1.0);
        }

        #[test]
        fn silent_is_near_zero() {
            assert!(Volume::SILENT.to_linear() < 0.0001);
        }

        #[test]
        fn converts_common_values() {
            let test_points: [(f32, f32); 3] = [
                (0.0, 1.0),    // 0 dBFS = 1.0
                (-6.0, 0.501), // -6 dBFS ≈ 0.501
                (-20.0, 0.1),  // -20 dBFS = 0.1
            ];

            for (db, expected) in test_points {
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
        }

        #[test]
        fn rejects_positive_dbfs() {
            assert!(matches!(
                Volume::new(1.0),
                Err(PlaybackError::ValueOutOfRange)
            ));
        }

        #[test]
        fn test_serialization() {
            let vol = Volume::new(-6.0).unwrap();
            let json = serde_json::to_string(&vol).unwrap();
            let decoded: Volume = serde_json::from_str(&json).unwrap();
            assert_eq!(vol, decoded);
        }
    }

    mod channel_tests {
        use super::*;

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
        fn test_serialization() {
            let channel = Deck::A;
            let json = serde_json::to_string(&channel).unwrap();
            let decoded: Deck = serde_json::from_str(&json).unwrap();
            assert_eq!(channel, decoded);
        }
    }
}
