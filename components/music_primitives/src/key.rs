use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum KeyError {
    #[error("Invalid key notation: {0}")]
    InvalidNotation(String),

    #[error("Unknown pitch class: {0}")]
    UnknownPitchClass(String),
}

/// Musical pitch class (0-11, where 0 = C)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PitchClass {
    C = 0,
    CSharp = 1, // C# / Db
    D = 2,
    DSharp = 3, // D# / Eb
    E = 4,
    F = 5,
    FSharp = 6, // F# / Gb
    G = 7,
    GSharp = 8, // G# / Ab
    A = 9,
    ASharp = 10, // A# / Bb
    B = 11,
}

impl FromStr for PitchClass {
    type Err = KeyError;
    /// Parse from traditional notation (supports sharps and flats)
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "C" => Ok(PitchClass::C),
            "C#" | "Db" => Ok(PitchClass::CSharp),
            "D" => Ok(PitchClass::D),
            "D#" | "Eb" => Ok(PitchClass::DSharp),
            "E" => Ok(PitchClass::E),
            "F" => Ok(PitchClass::F),
            "F#" | "Gb" => Ok(PitchClass::FSharp),
            "G" => Ok(PitchClass::G),
            "G#" | "Ab" => Ok(PitchClass::GSharp),
            "A" => Ok(PitchClass::A),
            "A#" | "Bb" => Ok(PitchClass::ASharp),
            "B" => Ok(PitchClass::B),
            _ => Err(Self::Err::UnknownPitchClass(s.to_string())),
        }
    }
}

impl PitchClass {
    /// Get as sharp notation (e.g., "C#")
    pub fn as_sharp(&self) -> &'static str {
        match self {
            PitchClass::C => "C",
            PitchClass::CSharp => "C#",
            PitchClass::D => "D",
            PitchClass::DSharp => "D#",
            PitchClass::E => "E",
            PitchClass::F => "F",
            PitchClass::FSharp => "F#",
            PitchClass::G => "G",
            PitchClass::GSharp => "G#",
            PitchClass::A => "A",
            PitchClass::ASharp => "A#",
            PitchClass::B => "B",
        }
    }

    /// Get as flat notation (e.g., "Db")
    pub fn as_flat(&self) -> &'static str {
        match self {
            PitchClass::C => "C",
            PitchClass::CSharp => "Db",
            PitchClass::D => "D",
            PitchClass::DSharp => "Eb",
            PitchClass::E => "E",
            PitchClass::F => "F",
            PitchClass::FSharp => "Gb",
            PitchClass::G => "G",
            PitchClass::GSharp => "Ab",
            PitchClass::A => "A",
            PitchClass::ASharp => "Bb",
            PitchClass::B => "B",
        }
    }

    /// Get numeric value (0-11)
    pub fn as_number(&self) -> u8 {
        *self as u8
    }
}

/// Musical mode (Major or Minor)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Major,
    Minor,
}

impl FromStr for Mode {
    type Err = KeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "major" | "maj" | "m" => Ok(Mode::Major),
            "minor" | "min" => Ok(Mode::Minor),
            _ => Err(Self::Err::InvalidNotation(format!("Unknown mode: {}", s))),
        }
    }
}

/// Musical key with support for multiple notation systems
///
/// Supports:
/// - Traditional: "C Major", "A Minor", "Eb Major"
/// - Camelot: "8B", "8A", "5B"
/// - Open Key: "1d", "1m", "10d"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pitch: PitchClass,
    mode: Mode,
}

impl Key {
    /// Create a new key
    pub fn new(pitch: PitchClass, mode: Mode) -> Self {
        Self { pitch, mode }
    }

    /// Parse from traditional notation (e.g., "C Major", "A Minor", "Eb Major")
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Key, KeyError};
    /// let key = Key::from_traditional("C Major")?;
    /// assert_eq!(key.to_traditional_sharp(), "C Major");
    ///
    /// let key = Key::from_traditional("Eb Major")?;
    /// assert_eq!(key.to_traditional_flat(), "Eb Major");
    /// # Ok::<(), KeyError>(())
    /// ```
    pub fn from_traditional(s: &str) -> Result<Self, KeyError> {
        // Split on whitespace
        let parts: Vec<&str> = s.split_whitespace().collect();

        if parts.len() != 2 {
            return Err(KeyError::InvalidNotation(s.to_string()));
        }

        let pitch = PitchClass::from_str(parts[0])?;
        let mode = Mode::from_str(parts[1])?;

        Ok(Self { pitch, mode })
    }

    /// Get traditional notation with sharps (e.g., "C# Major")
    pub fn to_traditional_sharp(&self) -> String {
        format!(
            "{} {}",
            self.pitch.as_sharp(),
            match self.mode {
                Mode::Major => "Major",
                Mode::Minor => "Minor",
            }
        )
    }

    /// Get traditional notation with flats (e.g., "Db Major")
    pub fn to_traditional_flat(&self) -> String {
        format!(
            "{} {}",
            self.pitch.as_flat(),
            match self.mode {
                Mode::Major => "Major",
                Mode::Minor => "Minor",
            }
        )
    }

    /// Get Camelot notation (DJ standard)
    ///
    /// Camelot Wheel maps keys to numbers 1-12 and letters A (minor) or B (major)
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Key, KeyError};
    /// let key = Key::from_traditional("C Major")?;
    /// assert_eq!(key.to_camelot(), "8B");
    ///
    /// let key = Key::from_traditional("A Minor")?;
    /// assert_eq!(key.to_camelot(), "8A");
    /// # Ok::<(), KeyError>(())
    /// ```
    pub fn to_camelot(&self) -> String {
        // Camelot wheel mapping
        let number = match (self.pitch, self.mode) {
            (PitchClass::C, Mode::Major) => 8,
            (PitchClass::C, Mode::Minor) => 5,
            (PitchClass::CSharp, Mode::Major) => 3,
            (PitchClass::CSharp, Mode::Minor) => 12,
            (PitchClass::D, Mode::Major) => 10,
            (PitchClass::D, Mode::Minor) => 7,
            (PitchClass::DSharp, Mode::Major) => 5,
            (PitchClass::DSharp, Mode::Minor) => 2,
            (PitchClass::E, Mode::Major) => 12,
            (PitchClass::E, Mode::Minor) => 9,
            (PitchClass::F, Mode::Major) => 7,
            (PitchClass::F, Mode::Minor) => 4,
            (PitchClass::FSharp, Mode::Major) => 2,
            (PitchClass::FSharp, Mode::Minor) => 11,
            (PitchClass::G, Mode::Major) => 9,
            (PitchClass::G, Mode::Minor) => 6,
            (PitchClass::GSharp, Mode::Major) => 4,
            (PitchClass::GSharp, Mode::Minor) => 1,
            (PitchClass::A, Mode::Major) => 11,
            (PitchClass::A, Mode::Minor) => 8,
            (PitchClass::ASharp, Mode::Major) => 6,
            (PitchClass::ASharp, Mode::Minor) => 3,
            (PitchClass::B, Mode::Major) => 1,
            (PitchClass::B, Mode::Minor) => 10,
        };

        let letter = match self.mode {
            Mode::Major => "B",
            Mode::Minor => "A",
        };

        format!("{}{}", number, letter)
    }

    /// Get Open Key notation (alternative DJ notation)
    ///
    /// Open Key uses numbers 1-12 and letters d (major) or m (minor)
    /// Open Key is offset from Camelot by +5 positions (counterclockwise)
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Key, KeyError};
    /// let key = Key::from_traditional("C Major")?;
    /// assert_eq!(key.to_open_key(), "1d");
    ///
    /// let key = Key::from_traditional("A Minor")?;
    /// assert_eq!(key.to_open_key(), "1m");
    /// # Ok::<(), KeyError>(())
    /// ```
    pub fn to_open_key(&self) -> String {
        // Extract Camelot number
        let camelot_str = self.to_camelot();
        let camelot_num = camelot_str[..camelot_str.len() - 1].parse::<u8>().unwrap();

        // Open Key is offset by +5 from Camelot (counterclockwise on wheel)
        // Formula: ((camelot + 4) mod 12) + 1
        let open_key_num = ((camelot_num + 4) % 12) + 1;

        let letter = match self.mode {
            Mode::Major => "d",
            Mode::Minor => "m",
        };

        format!("{}{}", open_key_num, letter)
    }

    pub fn pitch(&self) -> PitchClass {
        self.pitch
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_traditional_sharp())
    }
}

// Serialize as traditional notation
impl Serialize for Key {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_traditional_sharp())
    }
}

// Deserialize from traditional notation
impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Key::from_traditional(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_traditional_notation() {
        let key = Key::from_traditional("C Major").unwrap();
        assert_eq!(key.pitch(), PitchClass::C);
        assert_eq!(key.mode(), Mode::Major);

        let key = Key::from_traditional("A Minor").unwrap();
        assert_eq!(key.pitch(), PitchClass::A);
        assert_eq!(key.mode(), Mode::Minor);

        let key = Key::from_traditional("Eb Major").unwrap();
        assert_eq!(key.pitch(), PitchClass::DSharp);
        assert_eq!(key.mode(), Mode::Major);
    }

    #[test]
    fn traditional_notation_round_trip() {
        let key = Key::from_traditional("F# Major").unwrap();
        assert_eq!(key.to_traditional_sharp(), "F# Major");

        let key = Key::from_traditional("Gb Major").unwrap();
        assert_eq!(key.to_traditional_flat(), "Gb Major");
    }

    #[test]
    fn camelot_conversion() {
        assert_eq!(Key::from_traditional("C Major").unwrap().to_camelot(), "8B");
        assert_eq!(Key::from_traditional("A Minor").unwrap().to_camelot(), "8A");
        assert_eq!(Key::from_traditional("G Major").unwrap().to_camelot(), "9B");
        assert_eq!(Key::from_traditional("E Minor").unwrap().to_camelot(), "9A");
    }

    #[test]
    fn open_key_conversion() {
        assert_eq!(
            Key::from_traditional("C Major").unwrap().to_open_key(),
            "1d"
        );
        assert_eq!(
            Key::from_traditional("A Minor").unwrap().to_open_key(),
            "1m"
        );
        assert_eq!(
            Key::from_traditional("G Major").unwrap().to_open_key(),
            "2d"
        );
    }

    #[test]
    fn serialization() {
        let key = Key::from_traditional("C Major").unwrap();
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"C Major\"");

        let deserialized: Key = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, key);
    }
}
