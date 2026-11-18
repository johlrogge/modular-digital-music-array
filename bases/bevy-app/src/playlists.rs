use bevy::prelude::*;
use mdma_client::{PlaylistName, TrackInfo};

/// One playlist line as shown: a track, or a line the library could not
/// resolve, kept in place so positions match the playlist file.
#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "Rows are mostly tracks, so boxing them would add an allocation per row to save space on the rare unavailable one."
)]
pub enum PlaylistRow {
    Track(TrackInfo),
    /// The library could not resolve the line; carries the reason.
    Unavailable(String),
}

/// Holds the playlist sidebar state and the currently displayed rows.
#[derive(Resource, Default)]
pub struct PlaylistState {
    /// All known playlist names (populated from IpcEvent::Playlists).
    pub names: Vec<PlaylistName>,
    /// Index into `names` of the currently-selected playlist, or None.
    pub selected: Option<usize>,
    /// Rows of the selected playlist, one per entry, in order.
    pub rows: Vec<PlaylistRow>,
    /// True while a GetPlaylistTracks request is in flight.
    pub loading: bool,
}

/// Which content the central panel should show.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CentralView {
    /// Show the tracks of the selected playlist.
    #[default]
    Playlist,
    /// Reserved for WP3: candidate/search results.
    Candidates,
}
