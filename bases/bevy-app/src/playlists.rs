use bevy::prelude::*;
use mdma_client::{PlaylistName, TrackInfo};

/// Holds the playlist sidebar state and the currently displayed tracks.
#[derive(Resource, Default)]
pub struct PlaylistState {
    /// All known playlist names (populated from IpcEvent::Playlists).
    pub names: Vec<PlaylistName>,
    /// Index into `names` of the currently-selected playlist, or None.
    pub selected: Option<usize>,
    /// Tracks belonging to the selected playlist.
    pub tracks: Vec<TrackInfo>,
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
