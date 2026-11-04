//! Argument types shared by several commands.
//!
//! A type is shared only when its help text would be identical everywhere.
//! Plain newtypes for now; the command modules adopt them as they are converted.

// The newtypes are adopted command by command; until then most are unused.
#![allow(dead_code)]

/// Content hash of a track (full or partial, with or without `sha256:` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackHash(pub String);

/// Optional content hash of a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptTrackHash(pub Option<String>);

/// Name of an MDMA playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist(pub String);

/// Optional name of an MDMA playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptPlaylist(pub Option<String>);

/// Name of a music source (not to be confused with `mdma_client::SourceName`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceName(pub String);

/// Do not ask for confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Yes(pub bool);

/// Show what would happen without making changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DryRun(pub bool);

/// Start the job and return immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoWait(pub bool);

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Transfer the file without conversion (default)
    Original,
    Aiff,
    Wav,
}

impl ExportFormat {
    /// Return the fixed file extension for converted formats, or `None` for `Original`.
    pub fn static_extension(&self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aiff => Some("aiff"),
            Self::Wav => Some("wav"),
        }
    }

    /// Format string to pass in the `?format=` query parameter.
    pub fn format_param(&self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Aiff => "aiff",
            Self::Wav => "wav",
        }
    }
}
