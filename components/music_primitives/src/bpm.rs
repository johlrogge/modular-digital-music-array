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
#[serde(try_from = "u32", into = "u32")]
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
        if !(Self::MIN..=Self::MAX).contains(&bpm) {
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
        if !(Self::MIN..=Self::MAX).contains(&bpm_f32) {
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

/// Minimum valid value in hundredths (20.0 BPM → 2000)
const MIN_HUNDREDTHS: u32 = 2000;
/// Maximum valid value in hundredths (999.99 BPM → 99999)
const MAX_HUNDREDTHS: u32 = 99999;

impl TryFrom<u32> for Bpm {
    type Error = BpmError;

    /// Convert from hundredths representation (the wire format).
    ///
    /// The value `12545` represents 125.45 BPM.
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if !(MIN_HUNDREDTHS..=MAX_HUNDREDTHS).contains(&value) {
            return Err(BpmError::OutOfRange(value as f32 / 100.0));
        }
        Ok(Bpm(value))
    }
}

impl From<Bpm> for u32 {
    fn from(b: Bpm) -> u32 {
        b.0
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
    use pretty_assertions::assert_eq;
    use rstest::rstest;

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

    #[rstest]
    #[case(10.0)]
    #[case(1000.0)]
    fn bpm_out_of_range_errors(#[case] bpm: f32) {
        assert!(Bpm::from_f32(bpm).is_err());
    }

    #[rstest]
    #[case(125.45, "125.45")]
    #[case(128.0, "128.00")]
    fn bpm_display_formatting(#[case] value: f32, #[case] expected: &str) {
        let bpm = Bpm::from_f32(value).unwrap();
        assert_eq!(format!("{}", bpm), expected);
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

    #[test]
    fn bpm_deserializing_out_of_range_returns_error() {
        // 500 hundredths = 5.0 BPM, below minimum of 20.0 BPM
        let result: Result<Bpm, _> = serde_json::from_str("500");
        assert!(
            result.is_err(),
            "expected error for out-of-range BPM hundredths"
        );
    }
}
