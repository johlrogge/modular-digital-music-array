#![allow(dead_code)]

use crate::selection::SelectionState;
use crossterm::event::KeyEvent;
use mdma_client::{ContentHash, PlaylistName};
use ratatui::layout::Rect;
use ratatui::Frame;

/// Identifies whether the focused pane can accept the currently-playing track,
/// and if so, what kind of target it is.
///
/// Returned by `Pane::add_playing_target()`.
#[derive(Debug, Clone, PartialEq)]
pub enum AddPlayingTarget {
    /// The pane is a playback queue — append to the queue.
    Queue,
    /// The pane is a named playlist — append to this playlist.
    Playlist(PlaylistName),
    /// The pane does not support receiving the playing track (e.g. Search, Browser).
    None,
}

/// Identifies the kind of pane for routing and display purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Search,
    Queue,
    Browser,
    Playlist,
    PlaylistsList,
}

/// Actions a pane can return from key handling.
pub enum PaneAction {
    /// Key was handled; no further action needed.
    Consumed,
    /// Key was not handled by this pane.
    Ignored,
    /// An error occurred; display to user.
    Error(String),
    /// An informational message; display to user.
    Info(String),
    /// Request to open a playlist pane.
    OpenPlaylist(PlaylistName),
}

/// The core pane abstraction.
///
/// Each panel in the TUI layout implements this trait. The `App` holds panes
/// as `Box<dyn Pane>` to allow heterogeneous panel types without match explosion.
pub trait Pane {
    /// Render this pane into the given frame area.
    fn render(&self, f: &mut Frame, area: Rect);

    /// Handle a key event, returning what action (if any) the app should take.
    fn handle_key(&mut self, key: KeyEvent) -> PaneAction;

    /// Return the data-layer content hashes for the current selection.
    fn resolve_selection(&self) -> Vec<ContentHash>;

    /// Immutable access to the selection state.
    fn selection_state(&self) -> &SelectionState;

    /// Mutable access to the selection state.
    fn selection_state_mut(&mut self) -> &mut SelectionState;

    /// Title shown in the pane border.
    fn title(&self) -> &str;

    /// Total number of items managed by this pane (visible or not).
    fn item_count(&self) -> usize;

    /// Discriminant for this pane's type.
    fn pane_kind(&self) -> PaneKind;

    /// Accept incoming tracks (e.g. dragged from another pane).
    /// Default: returns an error indicating this pane does not accept tracks.
    fn accept_tracks(&mut self, _hashes: &[ContentHash]) -> PaneAction {
        PaneAction::Error("Cannot add tracks to this pane type".to_string())
    }

    /// For playlist panes: the playlist name.
    fn playlist_name(&self) -> Option<&PlaylistName> {
        None
    }

    /// Refresh this pane's data from the backend.
    fn refresh(&mut self) -> PaneAction {
        PaneAction::Consumed
    }

    /// Return the display string for the item at the given *data* index.
    ///
    /// Used by the filter predicate to match text the user sees on screen.
    /// Returns `None` if the index is out of range.
    ///
    /// Default implementation returns `None` (no text available), which the filter
    /// treats as "keep" to avoid accidentally hiding items from panes that have not
    /// implemented this method yet.
    fn display_string(&self, _data_idx: usize) -> Option<String> {
        None
    }

    /// Return the target for the "add currently-playing track" action (`A` key).
    ///
    /// `QueuePane` returns `Queue`, `PlaylistPane` returns `Playlist(name)`,
    /// all other panes return `None` via this default.
    fn add_playing_target(&self) -> AddPlayingTarget {
        AddPlayingTarget::None
    }

    /// Clone this pane into a new `Box<dyn Pane>`.
    ///
    /// Required — each pane implements this by calling `Box::new(self.clone())`.
    /// This method exists so that `Box<dyn Pane>` can be cloned even though
    /// the trait is object-safe (sized-Clone is not object-safe).
    fn clone_box(&self) -> Box<dyn Pane>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::SelectionState;
    use crossterm::event::KeyEvent;
    use ratatui::layout::Rect;
    use ratatui::Frame;

    /// Stub pane that uses the trait defaults (search, browser, playlists-list behaviour).
    #[derive(Clone)]
    struct DefaultPane;
    impl Pane for DefaultPane {
        fn render(&self, _f: &mut Frame, _area: Rect) {}
        fn handle_key(&mut self, _key: KeyEvent) -> PaneAction {
            PaneAction::Ignored
        }
        fn resolve_selection(&self) -> Vec<ContentHash> {
            vec![]
        }
        fn selection_state(&self) -> &SelectionState {
            unimplemented!()
        }
        fn selection_state_mut(&mut self) -> &mut SelectionState {
            unimplemented!()
        }
        fn title(&self) -> &str {
            "stub"
        }
        fn item_count(&self) -> usize {
            0
        }
        fn pane_kind(&self) -> PaneKind {
            PaneKind::Search
        }
        fn clone_box(&self) -> Box<dyn Pane> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn default_pane_add_playing_target_is_none() {
        let pane = DefaultPane;
        assert_eq!(pane.add_playing_target(), AddPlayingTarget::None);
    }

    #[test]
    fn add_playing_target_queue_is_not_none() {
        assert_ne!(AddPlayingTarget::Queue, AddPlayingTarget::None);
    }

    #[test]
    fn add_playing_target_playlist_is_not_none() {
        let name = PlaylistName::new("test-list").unwrap();
        let target = AddPlayingTarget::Playlist(name);
        assert!(matches!(target, AddPlayingTarget::Playlist(_)));
    }
}
