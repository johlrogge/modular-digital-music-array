//! What each compare-and-swap request (ADR-027) answers and writes, given the
//! playlist's current bytes. Pure: the caller reads the bytes under its lock,
//! stores what the plan says to write, and answers its outcome. The library
//! service and its stub both go through here, so they cannot drift apart.

use library_ipc_protocol::{
    playlist_hashes, EditOp, EditOutcome, PlaylistEntry, PlaylistSnapshot, PlaylistVersion,
    ProtocolError, Skipped, StoredPlaylist, TrackInfo,
};
use library_projection::{resolve_entry, HashedTrack};

use crate::{apply, PlaylistDoc};

/// What to do for one `PlaylistEdit` or `PlaylistRevert`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    /// Nothing is written: a conflict, or an edit that leaves the bytes as they are.
    Answer(EditOutcome),
    /// Store `bytes`, then answer `outcome`.
    Write { bytes: String, outcome: EditOutcome },
}

impl Planned {
    /// The bytes to store, if any, and the outcome to answer.
    pub fn into_parts(self) -> (Option<String>, EditOutcome) {
        match self {
            Planned::Answer(outcome) => (None, outcome),
            Planned::Write { bytes, outcome } => (Some(bytes), outcome),
        }
    }
}

/// `content` with `added` after it, starting on a new line: the one rule of
/// `PlaylistAppend`.
pub fn appended(content: &str, added: &str) -> String {
    let mut text = content.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(added);
    text
}

/// The ADR-026 read model of `content`: one entry per entry line, in file order.
pub fn entries<T: HashedTrack>(content: &str, tracks: &[T]) -> Vec<PlaylistEntry> {
    playlist_hashes(content)
        .map(|token| resolve_entry(tracks, token))
        .collect()
}

/// The entries of `content` with the version of its bytes.
pub fn snapshot<T: HashedTrack>(content: &str, tracks: &[T]) -> PlaylistSnapshot {
    PlaylistSnapshot {
        version: PlaylistVersion::of(content.as_bytes()),
        entries: entries(content, tracks),
    }
}

fn is_current(current: &str, version: &PlaylistVersion) -> bool {
    PlaylistVersion::of(current.as_bytes()) == *version
}

fn conflict<T: HashedTrack>(current: &str, tracks: &[T]) -> Planned {
    Planned::Answer(EditOutcome::Conflict {
        current: snapshot(current, tracks),
    })
}

/// The file `current` becomes `new`; it is written only if the bytes differ
/// (which is when the versions differ).
fn applied<T: HashedTrack>(
    current: &str,
    new: String,
    skipped: Vec<Skipped>,
    tracks: &[T],
) -> Planned {
    let outcome = EditOutcome::Applied {
        snapshot: snapshot(&new, tracks),
        skipped,
        previous: StoredPlaylist::new(current),
    };
    if new == current {
        Planned::Answer(outcome)
    } else {
        Planned::Write {
            bytes: new,
            outcome,
        }
    }
}

/// `PlaylistEdit`: apply `ops` to `current` if `version` is its version.
/// A refused edit is an error and writes nothing.
pub fn plan_edit<T: HashedTrack>(
    current: &str,
    version: &PlaylistVersion,
    ops: &[EditOp],
    tracks: &[T],
    to_info: impl Fn(&T) -> TrackInfo,
) -> Result<Planned, ProtocolError> {
    if !is_current(current, version) {
        return Ok(conflict(current, tracks));
    }
    let edited = apply(&PlaylistDoc::parse(current), ops, tracks, to_info)?;
    Ok(applied(
        current,
        edited.doc.render(),
        edited.skipped,
        tracks,
    ))
}

/// `PlaylistRevert`: put `content` back, verbatim, if `version` is the version
/// of `current`.
pub fn plan_revert<T: HashedTrack>(
    current: &str,
    version: &PlaylistVersion,
    content: &StoredPlaylist,
    tracks: &[T],
) -> Planned {
    if !is_current(current, version) {
        return conflict(current, tracks);
    }
    applied(current, content.as_str().to_string(), Vec::new(), tracks)
}

/// `PlaylistDelete`: `Ok` when `version` is the version of `current` and the
/// playlist may go; otherwise the current snapshot, for the conflict answer.
pub fn plan_delete<T: HashedTrack>(
    current: &str,
    version: &PlaylistVersion,
    tracks: &[T],
) -> Result<(), PlaylistSnapshot> {
    if is_current(current, version) {
        Ok(())
    } else {
        Err(snapshot(current, tracks))
    }
}
