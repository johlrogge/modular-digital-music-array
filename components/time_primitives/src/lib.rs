use serde::{Deserialize, Serialize};
use std::ops::{Add, Sub};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TimeError {
    #[error("PPQN cannot be zero")]
    ZeroPpqn,
    #[error("Tempo must be between {min} and {max} BPM")]
    TempoOutOfRange { min: f64, max: f64, value: f64 },
}

/// Number of ticks in the musical timeline
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ticks(u64);

impl Ticks {
    pub const ZERO: Self = Self(0);

    pub fn new(ticks: u64) -> Self {
        Self(ticks)
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl Add for Ticks {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl Sub for Ticks {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

/// Pulses per quarter note - resolution of the musical timeline
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct Ppqn(u32);

impl Ppqn {
    pub const DEFAULT: Self = Self(960);

    pub fn new(ppqn: u32) -> Result<Self, TimeError> {
        if ppqn == 0 {
            return Err(TimeError::ZeroPpqn);
        }
        Ok(Self(ppqn))
    }

    pub fn raw(&self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for Ppqn {
    type Error = TimeError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ppqn::new(value)
    }
}

impl From<Ppqn> for u32 {
    fn from(p: Ppqn) -> u32 {
        p.0
    }
}

/// Tempo in beats per minute
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Tempo(f64);

impl Tempo {
    pub const MIN: f64 = 20.0;
    pub const MAX: f64 = 400.0;
    pub const DEFAULT: Self = Self(120.0);

    pub fn new(bpm: f64) -> Result<Self, TimeError> {
        if !(Self::MIN..=Self::MAX).contains(&bpm) {
            return Err(TimeError::TempoOutOfRange {
                min: Self::MIN,
                max: Self::MAX,
                value: bpm,
            });
        }
        Ok(Self(bpm))
    }

    pub fn raw(&self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Tempo {
    type Error = TimeError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Tempo::new(value)
    }
}

impl From<Tempo> for f64 {
    fn from(t: Tempo) -> f64 {
        t.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[test]
    fn ticks_addition() {
        let t1 = Ticks::new(100);
        let t2 = Ticks::new(50);
        assert_eq!(t1 + t2, Ticks::new(150));
    }

    #[test]
    fn ticks_subtraction() {
        let t1 = Ticks::new(100);
        let t2 = Ticks::new(50);
        assert_eq!(t1 - t2, Ticks::new(50));
    }

    #[test]
    fn ticks_saturating_sub() {
        let t1 = Ticks::new(100);
        let t2 = Ticks::new(50);
        assert_eq!(t2 - t1, Ticks::ZERO);
    }

    #[test]
    fn ppqn_zero_returns_error() {
        assert!(matches!(Ppqn::new(0).unwrap_err(), TimeError::ZeroPpqn));
    }

    #[test]
    fn ppqn_960_is_valid() {
        assert!(Ppqn::new(960).is_ok());
    }

    #[test]
    fn ppqn_default_is_960() {
        assert_eq!(Ppqn::DEFAULT.raw(), 960);
    }

    #[rstest]
    #[case(0.0)]
    #[case(500.0)]
    fn tempo_out_of_range_rejected(#[case] value: f64) {
        assert!(Tempo::new(value).is_err());
    }

    #[test]
    fn tempo_120_is_valid() {
        assert!(Tempo::new(120.0).is_ok());
    }

    #[test]
    fn tempo_default_is_120() {
        assert_eq!(Tempo::DEFAULT.raw(), 120.0);
    }

    #[test]
    fn serialization() {
        let ticks = Ticks::new(42);
        let json = serde_json::to_string(&ticks).unwrap();
        let decoded: Ticks = serde_json::from_str(&json).unwrap();
        assert_eq!(ticks, decoded);

        let tempo = Tempo::new(140.0).unwrap();
        let json = serde_json::to_string(&tempo).unwrap();
        let decoded: Tempo = serde_json::from_str(&json).unwrap();
        assert_eq!(tempo, decoded);

        let ppqn = Ppqn::new(480).unwrap();
        let json = serde_json::to_string(&ppqn).unwrap();
        let decoded: Ppqn = serde_json::from_str(&json).unwrap();
        assert_eq!(ppqn, decoded);
    }

    #[test]
    fn tempo_deserializing_out_of_range_returns_error() {
        let result: Result<Tempo, _> = serde_json::from_str("0.0");
        assert!(result.is_err(), "expected error for out-of-range tempo");
    }

    #[test]
    fn ppqn_deserializing_zero_returns_error() {
        let result: Result<Ppqn, _> = serde_json::from_str("0");
        assert!(result.is_err(), "expected error for zero PPQN");
    }
}
