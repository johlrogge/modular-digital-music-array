use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EnergyLevelError {
    #[error("Energy level out of range: {0} (must be 1..=10)")]
    OutOfRange(u8),
}

/// Energy level on a scale from 1 (very mellow) to 10 (absolute peak).
///
/// Stored as a validated `u8` in the range 1..=10.
/// Serialises as a bare integer on the wire.
///
/// # Examples
/// ```
/// # use music_primitives::{EnergyLevel, EnergyLevelError};
/// let e = EnergyLevel::new(7)?;
/// assert_eq!(e.value(), 7);
/// assert_eq!(e.to_string(), "7");
///
/// assert!(EnergyLevel::new(0).is_err());
/// assert!(EnergyLevel::new(11).is_err());
/// # Ok::<(), EnergyLevelError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct EnergyLevel(u8);

impl EnergyLevel {
    /// Create an `EnergyLevel` from a raw value (1..=10).
    pub fn new(level: u8) -> Result<Self, EnergyLevelError> {
        if !(1..=10).contains(&level) {
            return Err(EnergyLevelError::OutOfRange(level));
        }
        Ok(EnergyLevel(level))
    }

    /// Get the raw value (1..=10).
    pub fn value(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for EnergyLevel {
    type Error = EnergyLevelError;

    /// Convert from raw integer wire representation (1..=10).
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EnergyLevel> for u8 {
    fn from(e: EnergyLevel) -> u8 {
        e.0
    }
}

impl fmt::Display for EnergyLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json;

    #[rstest]
    #[case(1)]
    #[case(5)]
    #[case(10)]
    fn valid_range(#[case] level: u8) {
        assert!(EnergyLevel::new(level).is_ok());
    }

    #[rstest]
    #[case(0)]
    #[case(11)]
    #[case(255)]
    fn out_of_range_errors(#[case] level: u8) {
        assert!(EnergyLevel::new(level).is_err());
    }

    #[rstest]
    #[case(1, "1")]
    #[case(7, "7")]
    #[case(10, "10")]
    fn display_shows_number(#[case] level: u8, #[case] expected: &str) {
        let e = EnergyLevel::new(level).unwrap();
        assert_eq!(e.to_string(), expected);
    }

    #[rstest]
    #[case(1)]
    #[case(5)]
    #[case(10)]
    fn serde_roundtrip(#[case] level: u8) {
        let e = EnergyLevel::new(level).unwrap();
        let json = serde_json::to_string(&e).unwrap();
        let back: EnergyLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn serializes_as_integer() {
        let e = EnergyLevel::new(7).unwrap();
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, "7");
    }

    #[test]
    fn deserialize_out_of_range_errors() {
        // 0 is out of range
        let result: Result<EnergyLevel, _> = serde_json::from_str("0");
        assert!(result.is_err());

        // 11 is out of range
        let result: Result<EnergyLevel, _> = serde_json::from_str("11");
        assert!(result.is_err());
    }

    #[test]
    fn ordering_works() {
        let low = EnergyLevel::new(3).unwrap();
        let high = EnergyLevel::new(8).unwrap();
        assert!(low < high);
    }
}
