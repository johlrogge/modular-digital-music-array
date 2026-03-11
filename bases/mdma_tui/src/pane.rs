#![allow(dead_code)]

use crate::selection::SelectionState;
use crossterm::event::KeyEvent;
use mdma_client::{ContentHash, PlaylistName};
use ratatui::layout::Rect;
use ratatui::Frame;

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
}
