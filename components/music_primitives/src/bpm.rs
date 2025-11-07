use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BpmError {
    #[error("BPM value out of range: {0}")]
    OutOfRange(f32),

    #[error("Invalid BPM value: {0}")]
    Invalid(String),
}

/// Beats per minute, stored as integer hundredths for precision without floats.
///
/// Internal representation: BPM * 100
/// - 125.45 BPM → Bpm(12545)
/// - 128.00 BPM → Bpm(12800)
///
/// Valid range: 20.0 to 999.99 BPM
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bpm(u32);

impl Bpm {
    /// Minimum valid BPM (20.0)
    pub const MIN: f32 = 20.0;

    /// Maximum valid BPM (999.99)
    pub const MAX: f32 = 999.99;

    /// Create BPM from floating point value
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Bpm, BpmError};
    /// let bpm = Bpm::from_f32(125.45)?;
    /// assert_eq!(bpm.as_f32(), 125.45);
    /// # Ok::<(), BpmError>(())
    /// ```
    pub fn from_f32(bpm: f32) -> Result<Self, BpmError> {
        if bpm < Self::MIN || bpm > Self::MAX {
            return Err(BpmError::OutOfRange(bpm));
        }

        // Round to 2 decimal places and convert to hundredths
        let hundredths = (bpm * 100.0).round() as u32;
        Ok(Bpm(hundredths))
    }

    /// Create BPM from integer value (whole BPM)
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Bpm, BpmError};
    /// let bpm = Bpm::from_u32(128)?;
    /// assert_eq!(bpm.as_f32(), 128.0);
    /// # Ok::<(), BpmError>(())
    /// ```
    pub fn from_u32(bpm: u32) -> Result<Self, BpmError> {
        let bpm_f32 = bpm as f32;
        if bpm_f32 < Self::MIN || bpm_f32 > Self::MAX {
            return Err(BpmError::OutOfRange(bpm_f32));
        }

        Ok(Bpm(bpm * 100))
    }

    /// Get BPM as floating point
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Bpm, BpmError};
    /// let bpm = Bpm::from_f32(125.45)?;
    /// assert_eq!(bpm.as_f32(), 125.45);
    /// # Ok::<(), BpmError>(())
    /// ```
    pub fn as_f32(&self) -> f32 {
        self.0 as f32 / 100.0
    }

    /// Get BPM as integer (rounded)
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Bpm, BpmError};
    /// let bpm = Bpm::from_f32(125.45)?;
    /// assert_eq!(bpm.as_u32(), 125);
    /// # Ok::<(), BpmError>(())
    /// ```
    pub fn as_u32(&self) -> u32 {
        (self.0 + 50) / 100 // Round to nearest
    }

    /// Get internal representation (hundredths)
    pub fn as_hundredths(&self) -> u32 {
        self.0
    }
}

impl fmt::Display for Bpm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.as_f32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_from_f32_stores_correctly() {
        let bpm = Bpm::from_f32(125.45).unwrap();
        assert_eq!(bpm.as_f32(), 125.45);
        assert_eq!(bpm.as_hundredths(), 12545);
    }

    #[test]
    fn bpm_from_u32_stores_correctly() {
        let bpm = Bpm::from_u32(128).unwrap();
        assert_eq!(bpm.as_f32(), 128.0);
        assert_eq!(bpm.as_u32(), 128);
    }

    #[test]
    fn bpm_rounding_works() {
        let bpm = Bpm::from_f32(125.456).unwrap();
        assert_eq!(bpm.as_f32(), 125.46); // Rounded to 2 decimals
    }

    #[test]
    fn bpm_out_of_range_errors() {
        assert!(Bpm::from_f32(10.0).is_err());
        assert!(Bpm::from_f32(1000.0).is_err());
    }

    #[test]
    fn bpm_display_formatting() {
        let bpm = Bpm::from_f32(125.45).unwrap();
        assert_eq!(format!("{}", bpm), "125.45");

        let bpm = Bpm::from_u32(128).unwrap();
        assert_eq!(format!("{}", bpm), "128.00");
    }

    #[test]
    fn bpm_ordering() {
        let bpm1 = Bpm::from_f32(125.0).unwrap();
        let bpm2 = Bpm::from_f32(128.0).unwrap();
        assert!(bpm1 < bpm2);
    }

    #[test]
    fn bpm_serialization() {
        let bpm = Bpm::from_f32(125.45).unwrap();
        let json = serde_json::to_string(&bpm).unwrap();
        assert_eq!(json, "12545");

        let deserialized: Bpm = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, bpm);
    }

    #[test]
    fn bpm_deserialize_from_integer() {
        let json = "12800";
        let bpm: Bpm = serde_json::from_str(json).unwrap();
        assert_eq!(bpm.as_f32(), 128.0);
    }
}
