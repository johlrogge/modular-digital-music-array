use std::ops::{Add, Sub};
use thiserror::Error;
use serde::{Serialize, Deserialize};

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
        Self(self.0 + other.0)
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

/// Tempo in beats per minute
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ticks_operations() {
        let t1 = Ticks::new(100);
        let t2 = Ticks::new(50);

        assert_eq!(t1 + t2, Ticks::new(150));
        assert_eq!(t1 - t2, Ticks::new(50));
        assert_eq!(t2 - t1, Ticks::ZERO); // Tests saturation
    }

    #[test]
    fn test_ppqn_validation() {
        assert!(matches!(
            Ppqn::new(0).unwrap_err(),
            TimeError::ZeroPpqn
        ));
        assert!(Ppqn::new(960).is_ok());
        assert_eq!(Ppqn::DEFAULT.raw(), 960);
    }

    #[test]
    fn test_tempo_validation() {
        assert!(matches!(
            Tempo::new(0.0).unwrap_err(),
            TimeError::TempoOutOfRange { min: 20.0, max: 400.0, value: 0.0 }
        ));
        assert!(matches!(
            Tempo::new(500.0).unwrap_err(),
            TimeError::TempoOutOfRange { min: 20.0, max: 400.0, value: 500.0 }
        ));
        assert!(Tempo::new(120.0).is_ok());
        assert_eq!(Tempo::DEFAULT.raw(), 120.0);
    }

    #[test]
    fn test_serialization() {
        let ticks = Ticks::new(42);
        let json = serde_json::to_string(&ticks).unwrap();
        let decoded: Ticks = serde_json::from_str(&json).unwrap();
        assert_eq!(ticks, decoded);

        let tempo = Tempo::new(140.0).unwrap();
        let json = serde_json::to_string(&tempo).unwrap();
        let decoded: Tempo = serde_json::from_str(&json).unwrap();
        assert_eq!(tempo, decoded);
    }
}