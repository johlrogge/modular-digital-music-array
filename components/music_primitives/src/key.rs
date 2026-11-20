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

    /// Every pitch class in chromatic order (index == `as_number`).
    const ALL: [PitchClass; 12] = [
        PitchClass::C,
        PitchClass::CSharp,
        PitchClass::D,
        PitchClass::DSharp,
        PitchClass::E,
        PitchClass::F,
        PitchClass::FSharp,
        PitchClass::G,
        PitchClass::GSharp,
        PitchClass::A,
        PitchClass::ASharp,
        PitchClass::B,
    ];
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

impl Mode {
    /// Camelot wheel letter: `A` (minor) or `B` (major).
    fn camelot_letter(self) -> char {
        match self {
            Mode::Major => 'B',
            Mode::Minor => 'A',
        }
    }

    /// Open Key letter: `d` (major) or `m` (minor).
    fn open_key_letter(self) -> char {
        match self {
            Mode::Major => 'd',
            Mode::Minor => 'm',
        }
    }

    /// Spelled-out name, as used by the traditional notation.
    fn name(self) -> &'static str {
        match self {
            Mode::Major => "Major",
            Mode::Minor => "Minor",
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

    /// All 24 keys: the minor keys, then the major keys, each in chromatic order from C.
    pub fn all() -> impl Iterator<Item = Key> {
        [Mode::Minor, Mode::Major].into_iter().flat_map(|mode| {
            PitchClass::ALL
                .into_iter()
                .map(move |pitch| Key::new(pitch, mode))
        })
    }

    /// All 24 keys in Camelot wheel order: 1A..12A (minor), then 1B..12B (major).
    pub fn camelot_wheel() -> impl Iterator<Item = Key> {
        [Mode::Minor, Mode::Major]
            .into_iter()
            .flat_map(|mode| (1..=12).filter_map(move |number| Key::from_wheel(number, mode)))
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
        format!("{} {}", self.pitch.as_sharp(), self.mode.name())
    }

    /// Get traditional notation with flats (e.g., "Db Major")
    pub fn to_traditional_flat(&self) -> String {
        format!("{} {}", self.pitch.as_flat(), self.mode.name())
    }

    /// Get the short traditional notation with flats (e.g. `Abm`, `C`, `Bb`): the
    /// pitch, then `m` for minor. At most three characters, as the ID3v2 `TKEY`
    /// frame requires; `FromStr` reads it back.
    pub fn to_short_flat(&self) -> String {
        let suffix = match self.mode {
            Mode::Major => "",
            Mode::Minor => "m",
        };
        format!("{}{}", self.pitch.as_flat(), suffix)
    }

    /// Return the Camelot wheel number (1–12) for this key.
    ///
    /// The number encodes harmonic position on the Camelot wheel independently
    /// of mode (A = minor, B = major). Use [`to_camelot`] to get the full
    /// `"8B"` / `"8A"` string, or this method when you need the bare number for
    /// sorting or arithmetic (e.g. `camelot_number()` + 5 mod 12 = Open Key).
    pub fn camelot_number(&self) -> u8 {
        match (self.pitch, self.mode) {
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
        }
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
        format!("{}{}", self.camelot_number(), self.mode.camelot_letter())
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
        // Open Key is offset by +5 from Camelot (counterclockwise on wheel)
        // Formula: ((camelot + 4) mod 12) + 1
        let open_key_num = ((self.camelot_number() + 4) % 12) + 1;

        format!("{}{}", open_key_num, self.mode.open_key_letter())
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

        Self::from_wheel(number, mode).ok_or_else(|| KeyError::InvalidNotation(s.to_string()))
    }

    /// The key at Camelot position `number` (1-12) in `mode`; `None` outside the wheel.
    ///
    /// The reverse of `camelot_number`, found by searching the 24 keys so the wheel
    /// is only written down once.
    fn from_wheel(number: u8, mode: Mode) -> Option<Self> {
        PitchClass::ALL
            .into_iter()
            .map(|pitch| Self { pitch, mode })
            .find(|key| key.camelot_number() == number)
    }

    /// Parse from Open Key notation (e.g. "1m" = A Minor, "1d" = C Major).
    fn from_open_key(number: u8, mode: Mode) -> Option<Self> {
        // Inverse of `to_open_key`: camelot = open + 7 (mod 12), with 0 meaning 12.
        if !(1..=12).contains(&number) {
            return None;
        }
        let camelot = match (number + 7) % 12 {
            0 => 12,
            n => n,
        };
        Self::from_wheel(camelot, mode)
    }

    pub fn pitch(&self) -> PitchClass {
        self.pitch
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The relative key: same Camelot number, other mode (8A <-> 8B).
    pub fn relative(&self) -> Self {
        // The relative minor sits a minor third below its major (9 semitones up mod 12).
        let (mode, up) = match self.mode {
            Mode::Major => (Mode::Minor, 9),
            Mode::Minor => (Mode::Major, 3),
        };
        let pitch = PitchClass::ALL[((self.pitch.as_number() + up) % 12) as usize];
        Self { pitch, mode }
    }

    /// True when the two keys are in different modes (A vs B on the wheel).
    pub fn mode_differs(&self, other: Key) -> bool {
        self.mode != other.mode
    }

    /// Clockwise steps around the Camelot wheel from `centre` to this key
    /// (0-11, a fifth up per step); the wheel wraps, so 12A -> 1A is one step.
    pub fn camelot_steps_from(&self, centre: Key) -> u8 {
        (self.camelot_number() + 12 - centre.camelot_number()) % 12
    }
}

impl FromStr for Key {
    type Err = KeyError;

    /// Parse any common notation, case-insensitively: Camelot (`8A`), Open Key
    /// (`1m`, `1d`), short (`Am`, `C#`, `Eb`; no suffix means major) and long
    /// (`A minor`, `C# Major`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || KeyError::InvalidNotation(s.to_string());
        let s = s.trim();

        // Camelot / Open Key: case-insensitive.
        let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        if digits_end > 0 {
            let number: u8 = s[..digits_end].parse().map_err(|_| invalid())?;
            let key = match s[digits_end..].to_ascii_lowercase().as_str() {
                "a" => Self::from_wheel(number, Mode::Minor),
                "b" => Self::from_wheel(number, Mode::Major),
                "m" => Self::from_open_key(number, Mode::Minor),
                "d" => Self::from_open_key(number, Mode::Major),
                _ => None,
            };
            return key.ok_or_else(invalid);
        }

        // Pitch letter and accidental are case-insensitive...
        let mut chars = s.chars();
        let letter = chars.next().map(|c| c.to_ascii_uppercase());
        let letter = letter.filter(|c| ('A'..='G').contains(c));
        let letter = letter.ok_or_else(invalid)?;
        let rest = chars.as_str();
        let (accidental, rest) = match rest.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some(a @ ('#' | 'b')) => (Some(a), &rest[1..]),
            _ => (None, rest),
        };
        let pitch_name: String = std::iter::once(letter).chain(accidental).collect();
        let pitch = PitchClass::from_str(&pitch_name)?;

        // ...but a single-letter mode is not: `m` is minor, `M` is major.
        let mode = match rest.trim() {
            "" => Mode::Major,
            single if single.len() == 1 => Mode::from_str(single)?,
            word => Mode::from_str(&word.to_ascii_lowercase())?,
        };
        Ok(Self { pitch, mode })
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

    #[test]
    fn all_lists_minor_then_major_in_chromatic_order() {
        let keys: Vec<Key> = Key::all().collect();
        assert_eq!(keys.len(), 24);
        assert_eq!(keys[0], Key::new(PitchClass::C, Mode::Minor));
        assert_eq!(keys[11], Key::new(PitchClass::B, Mode::Minor));
        assert_eq!(keys[12], Key::new(PitchClass::C, Mode::Major));
        assert_eq!(keys[23], Key::new(PitchClass::B, Mode::Major));
    }

    #[test]
    fn camelot_wheel_runs_1a_to_12a_then_1b_to_12b() {
        let names: Vec<String> = Key::camelot_wheel().map(|k| k.to_camelot()).collect();
        let expected: Vec<String> = ["A", "B"]
            .iter()
            .flat_map(|l| (1..=12).map(move |n| format!("{n}{l}")))
            .collect();
        assert_eq!(names, expected);
    }

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
    #[case("C Major", 8)]
    #[case("A Minor", 8)]
    #[case("G Major", 9)]
    #[case("E Minor", 9)]
    #[case("B Major", 1)]
    #[case("G# Minor", 1)]
    #[case("F# Minor", 11)]
    fn camelot_number_known_keys(#[case] notation: &str, #[case] expected: u8) {
        assert_eq!(
            Key::from_traditional(notation).unwrap().camelot_number(),
            expected
        );
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

    #[rstest]
    #[case("1A", "12A", 1)]
    #[case("12A", "1A", 11)]
    #[case("8A", "8A", 0)]
    #[case("8B", "8A", 0)]
    #[case("9A", "8A", 1)]
    fn camelot_steps_wrap_12_to_1(#[case] key: &str, #[case] centre: &str, #[case] steps: u8) {
        let key: Key = key.parse().unwrap();
        let centre: Key = centre.parse().unwrap();
        assert_eq!(key.camelot_steps_from(centre), steps);
    }

    #[test]
    fn relative_flips_mode_and_keeps_number() {
        let am: Key = "Am".parse().unwrap();
        assert_eq!(am.relative().to_camelot(), "8B");
        assert_eq!(am.relative().relative(), am);
        assert!(am.mode_differs(am.relative()));
    }

    #[rstest]
    #[case("8A", "A Minor")]
    #[case("8a", "A Minor")]
    #[case("8B", "C Major")]
    #[case("12A", "C# Minor")]
    #[case("1m", "A Minor")]
    #[case("1d", "C Major")]
    #[case("Am", "A Minor")]
    #[case("am", "A Minor")]
    #[case("A", "A Major")]
    #[case("C#", "C# Major")]
    #[case("Eb", "D# Major")]
    #[case("eb", "D# Major")]
    #[case("Bb", "A# Major")]
    #[case("bb", "A# Major")]
    #[case("Bbm", "A# Minor")]
    #[case("bm", "B Minor")]
    #[case("b", "B Major")]
    #[case("A minor", "A Minor")]
    #[case("a MINOR", "A Minor")]
    #[case("C# major", "C# Major")]
    #[case("Eb Major", "D# Major")]
    #[case("CM", "C Major")]
    #[case("C M", "C Major")]
    fn key_from_str_accepts_every_notation(#[case] input: &str, #[case] sharp: &str) {
        assert_eq!(input.parse::<Key>().unwrap().to_traditional_sharp(), sharp);
    }

    #[rstest]
    #[case("")]
    #[case("H")]
    #[case("13A")]
    #[case("0A")]
    #[case("8")]
    #[case("8C")]
    #[case("13m")]
    #[case("A sideways")]
    // Enharmonic spellings with a natural pitch name are deliberately not accepted.
    #[case("Cb")]
    #[case("Fb")]
    #[case("E#")]
    #[case("B#")]
    fn key_from_str_rejects_garbage(#[case] input: &str) {
        assert!(input.parse::<Key>().is_err(), "{input}");
    }

    #[test]
    fn open_key_round_trips_for_every_key() {
        for n in 1..=12 {
            for letter in ['A', 'B'] {
                let key = Key::from_camelot(&format!("{n}{letter}")).unwrap();
                assert_eq!(key.to_open_key().parse::<Key>().unwrap(), key);
            }
        }
    }

    #[rstest]
    #[case("G#m", "Abm")]
    #[case("C", "C")]
    #[case("A#", "Bb")]
    #[case("F#m", "Gbm")]
    #[case("Am", "Am")]
    fn short_flat_notation_is_at_most_three_chars_and_parses_back(
        #[case] input: &str,
        #[case] expected: &str,
    ) {
        let key: Key = input.parse().unwrap();
        let short = key.to_short_flat();
        assert_eq!(short, expected);
        assert!(short.len() <= 3);
        assert_eq!(short.parse::<Key>().unwrap(), key);
    }

    #[test]
    fn from_camelot_case_insensitive() {
        let key_upper = Key::from_camelot("8B").unwrap();
        let key_lower = Key::from_camelot("8b").unwrap();
        assert_eq!(key_upper, key_lower);
    }
}
