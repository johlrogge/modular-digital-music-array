//! Why a conversion did not happen (ADR-037 decision 5).
//!
//! Two kinds, because the caller reacts differently:
//! - a [`Refusal`] is about the file: it cannot be converted or verified, and
//!   asking again gives the same answer. The caller stores the AIFF as an AIFF
//!   blob and shows the reason;
//! - an [`EnvironmentError`] is about the machine: no `flac`, a full disk, a
//!   killed child. The file may well convert later, so the caller leaves the
//!   ingest pending and retries.

use std::fmt;

/// The reason a file is refused, as a value callers can match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RefusalReason {
    #[error("the source cannot be read as AIFF")]
    UnreadableSource,
    #[error("float or compressed audio")]
    FloatOrCompressed,
    #[error("more than 2 channels")]
    TooManyChannels,
    #[error("unsupported bit depth")]
    UnsupportedDepth,
    #[error("the sample rate is not an integer")]
    NonIntegerRate,
    #[error("unsupported format")]
    UnsupportedFormat,
    /// flac ran and rejected this file.
    #[error("the encoder failed")]
    EncoderFailed,
    #[error("the tags could not be written")]
    TagWriteFailed,
    #[error("stream properties differ")]
    StreamInfoMismatch,
    #[error("decoded audio differs")]
    PcmMismatch,
    #[error("the original file is not rebuilt byte for byte")]
    RebuildMismatch,
    #[error("tag facts differ")]
    TagMismatch,
    #[error("pictures differ")]
    PictureMismatch,
}

/// A refused file: the reason and what was found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}: {detail}")]
pub struct Refusal {
    pub reason: RefusalReason,
    pub detail: String,
}

impl Refusal {
    pub(crate) fn new(reason: RefusalReason, detail: impl fmt::Display) -> Self {
        Refusal {
            reason,
            detail: detail.to_string(),
        }
    }
}

/// The machine could not do the work; the file is not to blame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvironmentError {
    #[error("the flac program was not found: {0}")]
    FlacMissing(String),
    #[error("the flac program is not version 1.4 or newer: {0}")]
    FlacTooOld(String),
    /// flac could not be started, or its output could not be collected.
    #[error("flac could not be run: {0}")]
    Spawn(String),
    /// flac ended without an exit status (killed by a signal).
    #[error("flac was killed: {0}")]
    FlacKilled(String),
    #[error("staging directory: {0}")]
    Staging(String),
    /// I/O on the staged FLAC or the rebuilt AIFF.
    #[error("staged file: {0}")]
    StagedFile(String),
}

/// Why [`crate::encode_aiff_to_flac`] returned no FLAC.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// About the file: store it as an AIFF blob.
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    /// About the machine: retry later.
    #[error("environment: {0}")]
    Environment(#[from] EnvironmentError),
}
