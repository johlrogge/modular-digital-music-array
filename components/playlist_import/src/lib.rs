//! Playlist import: match a source playlist against the library and write it.
//!
//! Shared by `mdma bandcamp import-playlist` and the rekordbox import (CLI) and
//! by the bandcamp service. Library access goes through the existing seams:
//! [`track_matcher::TrackLookup`] (via [`LibraryTrackLookup`]) for reads and
//! [`PlaylistWriteOps`] for writes.

mod lookup;
mod matching;
mod naming;
mod report;
mod wire;
mod write;

#[cfg(test)]
mod fixtures;

pub use lookup::{LibraryTrackLookup, LookupError};
pub use matching::{match_source_playlist, matched_hashes, TrackOutcome};
pub use naming::{default_import_name, sanitize_playlist_name};
pub use report::{format_import_status, format_write_outcome};
pub use wire::{has_owned_missing, to_import_reports};
pub use write::{resolve_and_format_playlist, write_playlist, PlaylistWriteOps};
