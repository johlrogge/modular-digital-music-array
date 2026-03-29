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
        match s {
            "M" | "major" | "Major" | "maj" | "Maj" => Ok(Mode::Major),
            "m" | "minor" | "Minor" | "min" | "Min" => Ok(Mode::Minor),
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

    /// Parse from Camelot notation (e.g. "8B" = C Major, "8A" = A Minor).
    ///
    /// The letter suffix determines the mode:
    /// - `B` = Major
    /// - `A` = Minor
    ///
    /// # Examples
    /// ```
    /// # use music_primitives::{Key, KeyError};
    /// let key = Key::from_camelot("8B")?;
    /// assert_eq!(key.to_traditional_sharp(), "C Major");
    ///
    /// let key = Key::from_camelot("8A")?;
    /// assert_eq!(key.to_traditional_sharp(), "A Minor");
    /// # Ok::<(), KeyError>(())
    /// ```
    pub fn from_camelot(s: &str) -> Result<Self, KeyError> {
        let s = s.trim();
        if s.len() < 2 {
            return Err(KeyError::InvalidNotation(s.to_string()));
        }

        let (num_part, letter) = s.split_at(s.len() - 1);
        let number: u8 = num_part
            .parse()
            .map_err(|_| KeyError::InvalidNotation(s.to_string()))?;

        let mode = match letter {
            "B" | "b" => Mode::Major,
            "A" | "a" => Mode::Minor,
            _ => return Err(KeyError::InvalidNotation(s.to_string())),
        };

        // Camelot wheel reverse lookup
        let pitch = match (number, mode) {
            (8, Mode::Major) => PitchClass::C,
            (5, Mode::Minor) => PitchClass::C,
            (3, Mode::Major) => PitchClass::CSharp,
            (12, Mode::Minor) => PitchClass::CSharp,
            (10, Mode::Major) => PitchClass::D,
            (7, Mode::Minor) => PitchClass::D,
            (5, Mode::Major) => PitchClass::DSharp,
            (2, Mode::Minor) => PitchClass::DSharp,
            (12, Mode::Major) => PitchClass::E,
            (9, Mode::Minor) => PitchClass::E,
            (7, Mode::Major) => PitchClass::F,
            (4, Mode::Minor) => PitchClass::F,
            (2, Mode::Major) => PitchClass::FSharp,
            (11, Mode::Minor) => PitchClass::FSharp,
            (9, Mode::Major) => PitchClass::G,
            (6, Mode::Minor) => PitchClass::G,
            (4, Mode::Major) => PitchClass::GSharp,
            (1, Mode::Minor) => PitchClass::GSharp,
            (11, Mode::Major) => PitchClass::A,
            (8, Mode::Minor) => PitchClass::A,
            (6, Mode::Major) => PitchClass::ASharp,
            (3, Mode::Minor) => PitchClass::ASharp,
            (1, Mode::Major) => PitchClass::B,
            (10, Mode::Minor) => PitchClass::B,
            _ => return Err(KeyError::InvalidNotation(s.to_string())),
        };

        Ok(Self { pitch, mode })
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
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case("C Major", PitchClass::C, Mode::Major)]
    #[case("A Minor", PitchClass::A, Mode::Minor)]
    #[case("Eb Major", PitchClass::DSharp, Mode::Major)]
    fn parse_traditional_notation(
        #[case] notation: &str,
        #[case] expected_pitch: PitchClass,
        #[case] expected_mode: Mode,
    ) {
        let key = Key::from_traditional(notation).unwrap();
        assert_eq!(key.pitch(), expected_pitch);
        assert_eq!(key.mode(), expected_mode);
    }

    #[rstest]
    #[case("F# Major", "F# Major", "Gb Major")]
    #[case("Gb Major", "F# Major", "Gb Major")]
    fn traditional_notation_round_trip(
        #[case] input: &str,
        #[case] expected_sharp: &str,
        #[case] expected_flat: &str,
    ) {
        let key = Key::from_traditional(input).unwrap();
        assert_eq!(key.to_traditional_sharp(), expected_sharp);
        assert_eq!(key.to_traditional_flat(), expected_flat);
    }

    #[rstest]
    #[case("C Major", "8B")]
    #[case("A Minor", "8A")]
    #[case("G Major", "9B")]
    #[case("E Minor", "9A")]
    fn camelot_conversion(#[case] notation: &str, #[case] expected: &str) {
        assert_eq!(
            Key::from_traditional(notation).unwrap().to_camelot(),
            expected
        );
    }

    #[rstest]
    #[case("C Major", "1d")]
    #[case("A Minor", "1m")]
    #[case("G Major", "2d")]
    fn open_key_conversion(#[case] notation: &str, #[case] expected: &str) {
        assert_eq!(
            Key::from_traditional(notation).unwrap().to_open_key(),
            expected
        );
    }

    #[rstest]
    #[case("m", Mode::Minor)]
    #[case("M", Mode::Major)]
    #[case("major", Mode::Major)]
    #[case("Major", Mode::Major)]
    #[case("minor", Mode::Minor)]
    #[case("Minor", Mode::Minor)]
    #[case("maj", Mode::Major)]
    #[case("min", Mode::Minor)]
    fn mode_from_str_aliases(#[case] input: &str, #[case] expected: Mode) {
        assert_eq!(input.parse::<Mode>().unwrap(), expected);
    }

    #[test]
    fn serialization() {
        let key = Key::from_traditional("C Major").unwrap();
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"C Major\"");

        let deserialized: Key = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, key);
    }

    #[rstest]
    #[case("8B", "C Major")]
    #[case("8A", "A Minor")]
    #[case("9B", "G Major")]
    #[case("9A", "E Minor")]
    #[case("1B", "B Major")]
    #[case("1A", "G# Minor")]
    #[case("3B", "C# Major")]
    #[case("12A", "C# Minor")]
    fn from_camelot_roundtrip(#[case] camelot: &str, #[case] traditional: &str) {
        let key = Key::from_camelot(camelot).unwrap();
        assert_eq!(key.to_camelot(), camelot);
        // Also check traditional (sharp notation)
        assert_eq!(key.to_traditional_sharp(), traditional);
    }

    #[test]
    fn from_camelot_invalid_letter_errors() {
        assert!(Key::from_camelot("8C").is_err());
        assert!(Key::from_camelot("13B").is_err());
        assert!(Key::from_camelot("0A").is_err());
        assert!(Key::from_camelot("B").is_err());
    }

    #[test]
    fn from_camelot_case_insensitive() {
        let key_upper = Key::from_camelot("8B").unwrap();
        let key_lower = Key::from_camelot("8b").unwrap();
        assert_eq!(key_upper, key_lower);
    }
}
