#![allow(dead_code)]
use crate::error::TuiError;
use crate::pane::{Pane, PaneAction, PaneKind};
use crate::selection::SelectionState;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, LibraryBackend, PlaylistName, TrackInfo};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, BorderType, Borders},
    Frame,
};
use std::rc::Rc;

/// A pane that displays the contents of a named playlist.
pub struct PlaylistPane {
    name: PlaylistName,
    hashes: Vec<ContentHash>,
    tracks: Vec<TrackInfo>,
    selection: SelectionState,
    library: Rc<LibraryBackend>,
    title: String,
}

impl PlaylistPane {
    /// Open a playlist pane by loading the playlist from the library backend.
    ///
    /// Hashes that fail to resolve are skipped (logged at warn level).
    pub fn open(name: PlaylistName, library: Rc<LibraryBackend>) -> Result<Self, TuiError> {
        let hashes = library.playlist_get(&name)?;
        let tracks: Vec<TrackInfo> = hashes
            .iter()
            .filter_map(|h| match library.get_track(h) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!("Failed to resolve hash {}: {}", h.as_str(), e);
                    None
                }
            })
            .collect();
        let total = tracks.len();
        let title = format!("Playlist: {}", name);
        Ok(PlaylistPane {
            name,
            hashes,
            tracks,
            selection: SelectionState::new(total),
            library,
            title,
        })
    }

    /// Reorder: move the track at `from` to `to` (adjacent swap).
    fn swap_tracks(&mut self, from: usize, to: usize) {
        self.hashes.swap(from, to);
        self.tracks.swap(from, to);
    }

    /// Persist the current hash order to the backend.
    fn persist_order(&self) -> Result<(), mdma_client::LibraryClientError> {
        self.library.playlist_replace(&self.name, &self.hashes)
    }
}

impl Pane for PlaylistPane {
    fn render(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(self.title.as_str())
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(Color::White));

        if self.tracks.is_empty() {
            let inner = block.inner(area);
            f.render_widget(block, area);
            let placeholder = ratatui::widgets::Paragraph::new("Playlist is empty")
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(placeholder, inner);
            return;
        }

        render_track_list(f, area, &self.tracks, &self.selection, block);
    }

    fn handle_key(&mut self, key: KeyEvent) -> PaneAction {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.selection.move_cursor_down();
                PaneAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selection.move_cursor_up();
                PaneAction::Consumed
            }
            KeyCode::Char('x') => {
                self.selection.extend_selection_down();
                PaneAction::Consumed
            }
            KeyCode::Char('X') => {
                self.selection.extend_selection_up();
                PaneAction::Consumed
            }
            KeyCode::Char('%') => {
                self.selection.select_all();
                PaneAction::Consumed
            }
            KeyCode::Esc => {
                if !self.selection.pop_filter() {
                    self.selection.clear_selection();
                }
                PaneAction::Consumed
            }
            KeyCode::Char('J') => {
                // Move cursor track DOWN (swap with next)
                if let Some(vis_idx) = self.selection.cursor_position() {
                    if let Some(data_idx) = self.selection.visible_index_to_data(vis_idx) {
                        let next = data_idx + 1;
                        if next < self.hashes.len() {
                            self.swap_tracks(data_idx, next);
                            match self.persist_order() {
                                Ok(()) => {
                                    self.selection.move_cursor_down();
                                    PaneAction::Consumed
                                }
                                Err(e) => {
                                    // Revert
                                    self.swap_tracks(data_idx, next);
                                    PaneAction::Error(format!("Reorder failed: {e}"))
                                }
                            }
                        } else {
                            PaneAction::Consumed
                        }
                    } else {
                        PaneAction::Consumed
                    }
                } else {
                    PaneAction::Consumed
                }
            }
            KeyCode::Char('K') => {
                // Move cursor track UP (swap with previous)
                if let Some(vis_idx) = self.selection.cursor_position() {
                    if let Some(data_idx) = self.selection.visible_index_to_data(vis_idx) {
                        if data_idx > 0 {
                            let prev = data_idx - 1;
                            self.swap_tracks(data_idx, prev);
                            match self.persist_order() {
                                Ok(()) => {
                                    self.selection.move_cursor_up();
                                    PaneAction::Consumed
                                }
                                Err(e) => {
                                    // Revert
                                    self.swap_tracks(data_idx, prev);
                                    PaneAction::Error(format!("Reorder failed: {e}"))
                                }
                            }
                        } else {
                            PaneAction::Consumed
                        }
                    } else {
                        PaneAction::Consumed
                    }
                } else {
                    PaneAction::Consumed
                }
            }
            KeyCode::Char('d') => {
                // Remove selected tracks (or cursor track if nothing selected)
                let to_remove: std::collections::BTreeSet<usize> =
                    if self.selection.selected.is_empty() {
                        // Nothing explicitly selected — remove cursor track
                        if let Some(vis_idx) = self.selection.cursor_position() {
                            if let Some(data_idx) = self.selection.visible_index_to_data(vis_idx) {
                                std::iter::once(data_idx).collect()
                            } else {
                                return PaneAction::Consumed;
                            }
                        } else {
                            return PaneAction::Consumed;
                        }
                    } else {
                        // Map selected visible indices to data indices
                        self.selection
                            .selected
                            .iter()
                            .filter_map(|&vis_idx| self.selection.visible_index_to_data(vis_idx))
                            .collect()
                    };

                let remaining_hashes: Vec<ContentHash> = self
                    .hashes
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !to_remove.contains(i))
                    .map(|(_, h)| h.clone())
                    .collect();

                match self.library.playlist_replace(&self.name, &remaining_hashes) {
                    Ok(()) => {
                        // Rebuild tracks list from remaining hashes
                        let remaining_tracks: Vec<TrackInfo> = self
                            .tracks
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| !to_remove.contains(i))
                            .map(|(_, t)| t.clone())
                            .collect();
                        self.hashes = remaining_hashes;
                        self.tracks = remaining_tracks;
                        self.selection.set_total_items(self.tracks.len());
                        PaneAction::Info(format!("Removed {} track(s)", to_remove.len()))
                    }
                    Err(e) => PaneAction::Error(format!("Failed to remove tracks: {e}")),
                }
            }
            _ => PaneAction::Ignored,
        }
    }

    fn accept_tracks(&mut self, hashes: &[ContentHash]) -> PaneAction {
        let new_hashes: Vec<ContentHash> = deduplicate_hashes(hashes, &self.hashes)
            .into_iter()
            .cloned()
            .collect();

        if new_hashes.is_empty() {
            return PaneAction::Info("All selected tracks already in playlist".to_string());
        }

        let new_tracks: Vec<TrackInfo> = new_hashes
            .iter()
            .filter_map(|h| self.library.get_track(h).ok())
            .collect();

        match self.library.playlist_append(&self.name, &new_hashes) {
            Ok(()) => {
                let added = new_hashes.len();
                self.hashes.extend(new_hashes);
                self.tracks.extend(new_tracks);
                self.selection.set_total_items(self.tracks.len());
                PaneAction::Info(format!("Added {} track(s)", added))
            }
            Err(e) => PaneAction::Error(format!("Failed to add tracks: {e}")),
        }
    }

    fn resolve_selection(&self) -> Vec<ContentHash> {
        self.selection
            .selected
            .iter()
            .filter_map(|&vis_idx| self.selection.visible_index_to_data(vis_idx))
            .map(|data_idx| self.hashes[data_idx].clone())
            .collect()
    }

    fn selection_state(&self) -> &SelectionState {
        &self.selection
    }

    fn selection_state_mut(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn item_count(&self) -> usize {
        self.tracks.len()
    }

    fn pane_kind(&self) -> PaneKind {
        PaneKind::Playlist
    }

    fn playlist_name(&self) -> Option<&PlaylistName> {
        Some(&self.name)
    }

    fn refresh(&mut self) -> PaneAction {
        match self.library.playlist_get(&self.name) {
            Ok(hashes) => {
                let tracks: Vec<TrackInfo> = hashes
                    .iter()
                    .filter_map(|h| match self.library.get_track(h) {
                        Ok(t) => Some(t),
                        Err(e) => {
                            tracing::warn!("Failed to resolve hash {}: {}", h.as_str(), e);
                            None
                        }
                    })
                    .collect();
                self.hashes = hashes;
                self.tracks = tracks;
                self.selection.set_total_items(self.tracks.len());
                PaneAction::Consumed
            }
            Err(e) => PaneAction::Error(format!("Failed to refresh playlist: {e}")),
        }
    }
}

// =========================================================================
// Pure-logic helpers exposed for testing
// =========================================================================

/// Filter `incoming` hashes, removing any that are already in `existing`.
///
/// Returns the deduplicated slice of new hashes.
pub(crate) fn deduplicate_hashes<'a>(
    incoming: &'a [ContentHash],
    existing: &[ContentHash],
) -> Vec<&'a ContentHash> {
    let existing_set: std::collections::HashSet<&ContentHash> = existing.iter().collect();
    incoming
        .iter()
        .filter(|h| !existing_set.contains(h))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    #[test]
    fn deduplicate_hashes_filters_already_present() {
        let existing = vec![hash("sha256:aaa"), hash("sha256:bbb")];
        let incoming = vec![hash("sha256:bbb"), hash("sha256:ccc")];
        let result = deduplicate_hashes(&incoming, &existing);
        assert_eq!(result.len(), 1);
        assert_eq!(*result[0], hash("sha256:ccc"));
    }

    #[test]
    fn deduplicate_hashes_all_new_passes_through() {
        let existing = vec![hash("sha256:aaa")];
        let incoming = vec![hash("sha256:bbb"), hash("sha256:ccc")];
        let result = deduplicate_hashes(&incoming, &existing);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn deduplicate_hashes_all_duplicates_returns_empty() {
        let existing = vec![hash("sha256:aaa"), hash("sha256:bbb")];
        let incoming = vec![hash("sha256:aaa"), hash("sha256:bbb")];
        let result = deduplicate_hashes(&incoming, &existing);
        assert!(result.is_empty());
    }

    #[test]
    fn deduplicate_hashes_empty_incoming_returns_empty() {
        let existing = vec![hash("sha256:aaa")];
        let result = deduplicate_hashes(&[], &existing);
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_selection_maps_visible_indices_to_hashes() {
        // Build a minimal SelectionState manually to test the mapping logic
        let mut sel = SelectionState::new(3);
        // Select visible index 0 and 2
        sel.selected.insert(0);
        sel.selected.insert(2);

        let hashes = vec![hash("sha256:aaa"), hash("sha256:bbb"), hash("sha256:ccc")];

        // Replicate the resolve_selection logic
        let result: Vec<ContentHash> = sel
            .selected
            .iter()
            .filter_map(|&vis_idx| sel.visible_index_to_data(vis_idx))
            .map(|data_idx| hashes[data_idx].clone())
            .collect();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], hash("sha256:aaa"));
        assert_eq!(result[1], hash("sha256:ccc"));
    }
}
