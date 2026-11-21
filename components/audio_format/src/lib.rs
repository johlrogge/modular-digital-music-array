//! The audio formats MDMA knows, which of them library ingest accepts, their file
//! extensions, and the one content sniffer (`sniff`) that tells them apart by their bytes.
//!
//! Pure: no dependencies, so every layer (ingest, upload handling, the audio hash)
//! can share it.

mod sniff;

pub use sniff::{head_id3v2_len, is_frame_sync, sniff, zero_padding, Sniffed, MAX_ZERO_PADDING};

/// Supported audio formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Flac,
    Mp3,
    Aiff,
    Wav,
    M4a,
}

impl AudioFormat {
    /// Every format, in declaration order. A test over an exhaustive `match` fails to
    /// compile when a variant is added, as a reminder to extend this.
    pub const ALL: [AudioFormat; 5] = [Self::Flac, Self::Mp3, Self::Aiff, Self::Wav, Self::M4a];

    /// Every file extension (lower case) that names this format; the first is canonical.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Flac => &["flac"],
            Self::Mp3 => &["mp3"],
            Self::Aiff => &["aiff", "aif"],
            Self::Wav => &["wav"],
            Self::M4a => &["m4a"],
        }
    }

    /// Get canonical file extension
    pub fn extension(&self) -> &'static str {
        self.extensions()[0]
    }

    /// Detect format from file extension (case-insensitive)
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_lowercase();
        Self::ALL
            .into_iter()
            .find(|f| f.extensions().contains(&ext.as_str()))
    }

    /// Returns true if this format is accepted for library ingest.
    ///
    /// WAV is ingestible (no embedded tags expected; metadata derived from filename if absent).
    /// M4A (MPEG-4 Audio, wrapping AAC or ALAC) is ingestible; lofty reads its metadata.
    /// AIFF is ingestible (ADR-037): the library converts it to FLAC when that can be
    /// verified, and stores it as AIFF otherwise.
    ///
    /// Currently true for every format, but this stays the one place to say no to a
    /// future format. Every guard derives from this method.
    pub fn is_ingestible(&self) -> bool {
        matches!(
            self,
            Self::Flac | Self::Mp3 | Self::Aiff | Self::Wav | Self::M4a
        )
    }

    /// Every file extension (lower case) that names an ingestible format, aliases
    /// included (`aif` for AIFF). Derived from `extensions` and `is_ingestible`, so it
    /// cannot drift from them.
    pub fn ingestible_extensions() -> Vec<&'static str> {
        Self::ALL
            .into_iter()
            .filter(AudioFormat::is_ingestible)
            .flat_map(|f| f.extensions().iter().copied())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adding a variant fails to compile here until `ALL` is extended and checked.
    #[test]
    fn all_lists_every_variant() {
        fn index(f: AudioFormat) -> usize {
            match f {
                AudioFormat::Flac => 0,
                AudioFormat::Mp3 => 1,
                AudioFormat::Aiff => 2,
                AudioFormat::Wav => 3,
                AudioFormat::M4a => 4,
            }
        }
        let mut seen = [false; 5];
        for f in AudioFormat::ALL {
            seen[index(f)] = true;
        }
        assert!(seen.iter().all(|s| *s), "ALL misses a variant: {seen:?}");
        assert_eq!(AudioFormat::ALL.len(), seen.len());
    }

    #[test]
    fn extensions_round_trip_case_insensitively() {
        for f in AudioFormat::ALL {
            for ext in f.extensions() {
                assert_eq!(AudioFormat::from_extension(ext), Some(f));
                assert_eq!(AudioFormat::from_extension(&ext.to_uppercase()), Some(f));
            }
        }
        assert_eq!(AudioFormat::from_extension("aif"), Some(AudioFormat::Aiff));
        assert_eq!(AudioFormat::from_extension("ogg"), None);
    }

    #[test]
    fn ingestible_extensions_include_both_aiff_spellings() {
        let exts = AudioFormat::ingestible_extensions();
        for ext in ["flac", "mp3", "wav", "m4a", "aiff", "aif"] {
            assert!(exts.contains(&ext), "{ext} missing from {exts:?}");
        }
    }
}
