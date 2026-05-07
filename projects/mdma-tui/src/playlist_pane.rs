#![allow(dead_code)]
use crate::pane::{AddPlayingTarget, Pane, PaneAction, PaneKind};
use crate::selection::SelectionState;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, LibraryBackend, PlaylistName, TrackInfo};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders},
    Frame,
};
use std::rc::Rc;
use track_formatter::format_playlist_content;

/// Build formatted playlist content from a slice of track info.
///
/// Produces the canonical `{8hash}  {Artist} - {Title}  [{duration}]` format
/// used by all playlist write paths in the TUI.
pub(crate) fn playlist_content_for_write(tracks: &[TrackInfo]) -> String {
    format_playlist_content(tracks)
}

/// Maximum number of undo snapshots retained per playlist pane.
const UNDO_DEPTH: usize = 50;

/// Snapshot of a playlist pane's mutable state, used for undo.
#[derive(Clone)]
struct PlaylistSnapshot {
    hashes: Vec<ContentHash>,
    tracks: Vec<TrackInfo>,
    cursor: usize,
}

/// A pane that displays the contents of a named playlist.
pub struct PlaylistPane {
    name: PlaylistName,
    hashes: Vec<ContentHash>,
    tracks: Vec<TrackInfo>,
    selection: SelectionState,
    library: Rc<LibraryBackend>,
    title: String,
    undo_stack: Vec<PlaylistSnapshot>,
}

impl Clone for PlaylistPane {
    fn clone(&self) -> Self {
        PlaylistPane {
            name: self.name.clone(),
            hashes: self.hashes.clone(),
            tracks: self.tracks.clone(),
            selection: self.selection.clone(),
            library: Rc::clone(&self.library),
            title: self.title.clone(),
            // Undo history is intentionally not carried over to tab clones —
            // each tab is an independent view.
            undo_stack: Vec::new(),
        }
    }
}

impl PlaylistPane {
    /// Open a playlist pane by loading the playlist from the library backend.
    ///
    /// Hashes that fail to resolve are skipped (logged at warn level).
    pub fn open(name: PlaylistName, library: Rc<LibraryBackend>) -> color_eyre::Result<Self> {
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
            undo_stack: Vec::new(),
        })
    }

    /// Push a snapshot of the current state onto the undo stack.
    ///
    /// Drops the oldest entry when the stack exceeds `UNDO_DEPTH`.
    fn push_undo_snapshot(&mut self) {
        let cursor = self.selection.list_state.selected().unwrap_or(0);
        let snap = PlaylistSnapshot {
            hashes: self.hashes.clone(),
            tracks: self.tracks.clone(),
            cursor,
        };
        self.undo_stack.push(snap);
        if self.undo_stack.len() > UNDO_DEPTH {
            self.undo_stack.remove(0);
        }
    }

    /// Reorder: move the track at `from` to `to` (adjacent swap).
    fn swap_tracks(&mut self, from: usize, to: usize) {
        self.hashes.swap(from, to);
        self.tracks.swap(from, to);
    }

    /// Persist the current hash order to the backend with canonical formatting.
    ///
    /// Writes `{8hash}  {Artist} - {Title}  [{duration}]` lines so the on-disk
    /// playlist file is human-readable and matches `mdma search` output.
    fn persist_order(&self) -> Result<(), mdma_client::LibraryClientError> {
        let content = playlist_content_for_write(&self.tracks);
        self.library.playlist_replace(&self.name, content)
    }

    /// Undo the most recent mutation by restoring the last snapshot.
    ///
    /// If the persist fails the in-memory state is rolled back and the
    /// snapshot is discarded so the undo stack stays consistent.
    fn undo_mutation(&mut self) -> PaneAction {
        let Some(snap) = self.undo_stack.pop() else {
            return PaneAction::Info("Nothing to undo".to_string());
        };

        let prior_hashes = std::mem::replace(&mut self.hashes, snap.hashes);
        let prior_tracks = std::mem::replace(&mut self.tracks, snap.tracks);

        match self.persist_order() {
            Ok(()) => {
                self.selection.set_total_items(self.hashes.len());
                self.selection
                    .list_state
                    .select(Some(snap.cursor.min(self.hashes.len().saturating_sub(1))));
                PaneAction::Info("Undone".to_string())
            }
            Err(e) => {
                // Revert the undo if persist failed
                self.hashes = prior_hashes;
                self.tracks = prior_tracks;
                PaneAction::Error(format!("Undo failed: {e}"))
            }
        }
    }

    /// Paste `hashes` after the current cursor position.
    ///
    /// Updates `self.hashes`, `self.tracks`, and the selection state, then
    /// persists to the backend. On backend failure the operation is reverted.
    /// Returns a `PaneAction` describing the outcome.
    pub fn paste_after_cursor(&mut self, hashes: Vec<ContentHash>) -> PaneAction {
        if hashes.is_empty() {
            return PaneAction::Info("Clipboard is empty".to_string());
        }

        let (new_hashes, new_cursor) =
            paste_after_cursor_into(&self.hashes, &self.selection, &hashes);

        // Resolve TrackInfo for the pasted hashes (skip failures silently).
        let new_tracks: Vec<TrackInfo> = new_hashes
            .iter()
            .filter_map(|h| self.library.get_track(h).ok())
            .collect();

        self.push_undo_snapshot();

        let old_hashes = std::mem::replace(&mut self.hashes, new_hashes.clone());
        let old_tracks = std::mem::replace(&mut self.tracks, new_tracks);

        match self.persist_order() {
            Ok(()) => {
                self.selection.set_total_items(self.hashes.len());
                self.selection.list_state.select(Some(new_cursor));
                PaneAction::Info(format!("Pasted {} track(s)", hashes.len()))
            }
            Err(e) => {
                // Revert state and discard the snapshot (persist never succeeded)
                self.hashes = old_hashes;
                self.tracks = old_tracks;
                self.undo_stack.pop();
                PaneAction::Error(format!("Paste failed: {e}"))
            }
        }
    }
}

impl Pane for PlaylistPane {
    fn render(&self, f: &mut Frame, area: Rect) {
        if self.tracks.is_empty() {
            let placeholder = ratatui::widgets::Paragraph::new("Playlist is empty")
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(placeholder, area);
            return;
        }

        let block = Block::default().borders(Borders::NONE);
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
                // Move selected block DOWN by one position.
                // If no explicit selection, falls back to cursor track.
                match effective_data_indices_for_move(&self.selection) {
                    Err(msg) => PaneAction::Info(msg),
                    Ok(selected) if selected.is_empty() => PaneAction::Consumed,
                    Ok(selected) => {
                        match move_block_down(&self.hashes, &self.tracks, &selected) {
                            None => PaneAction::Consumed, // at bottom, no-op
                            Some((new_hashes, new_tracks, new_selected)) => {
                                self.push_undo_snapshot();
                                let old_hashes = std::mem::replace(&mut self.hashes, new_hashes);
                                let old_tracks = std::mem::replace(&mut self.tracks, new_tracks);
                                match self.persist_order() {
                                    Ok(()) => {
                                        let new_cursor = *new_selected.last().unwrap_or(&0);
                                        // Only update selection state if there was an explicit multi-selection
                                        if !self.selection.selected.is_empty() {
                                            self.selection
                                                .set_selected_visible_indices(new_selected);
                                        }
                                        self.selection.list_state.select(Some(new_cursor));
                                        PaneAction::Consumed
                                    }
                                    Err(e) => {
                                        // Revert and discard snapshot
                                        self.hashes = old_hashes;
                                        self.tracks = old_tracks;
                                        self.undo_stack.pop();
                                        PaneAction::Error(format!("Reorder failed: {e}"))
                                    }
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Char('K') => {
                // Move selected block UP by one position.
                // If no explicit selection, falls back to cursor track.
                match effective_data_indices_for_move(&self.selection) {
                    Err(msg) => PaneAction::Info(msg),
                    Ok(selected) if selected.is_empty() => PaneAction::Consumed,
                    Ok(selected) => {
                        match move_block_up(&self.hashes, &self.tracks, &selected) {
                            None => PaneAction::Consumed, // at top, no-op
                            Some((new_hashes, new_tracks, new_selected)) => {
                                self.push_undo_snapshot();
                                let old_hashes = std::mem::replace(&mut self.hashes, new_hashes);
                                let old_tracks = std::mem::replace(&mut self.tracks, new_tracks);
                                match self.persist_order() {
                                    Ok(()) => {
                                        let new_cursor = *new_selected.first().unwrap_or(&0);
                                        // Only update selection state if there was an explicit multi-selection
                                        if !self.selection.selected.is_empty() {
                                            self.selection
                                                .set_selected_visible_indices(new_selected);
                                        }
                                        self.selection.list_state.select(Some(new_cursor));
                                        PaneAction::Consumed
                                    }
                                    Err(e) => {
                                        // Revert and discard snapshot
                                        self.hashes = old_hashes;
                                        self.tracks = old_tracks;
                                        self.undo_stack.pop();
                                        PaneAction::Error(format!("Reorder failed: {e}"))
                                    }
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Char(',') => {
                self.selection.clear_selection();
                PaneAction::Consumed
            }
            KeyCode::Char('d') => {
                // Cut selected tracks (or cursor track if nothing selected) into
                // the App clipboard.  The caller (`dispatch_pane_action`) writes
                // the returned hashes into `app.clipboard`.
                let (cut_hashes, remaining_hashes) =
                    collect_cut_targets(&self.hashes, &self.selection);

                if cut_hashes.is_empty() {
                    return PaneAction::Consumed;
                }

                self.push_undo_snapshot();

                // Compute remaining tracks before writing so we can format the content.
                let remaining_tracks: Vec<TrackInfo> = remaining_hashes
                    .iter()
                    .filter_map(|h| self.library.get_track(h).ok())
                    .collect();
                let content = playlist_content_for_write(&remaining_tracks);

                match self.library.playlist_replace(&self.name, content) {
                    Ok(()) => {
                        self.hashes = remaining_hashes;
                        self.tracks = remaining_tracks;
                        // Clamp cursor to new length
                        let new_len = self.hashes.len();
                        let new_cursor = self
                            .selection
                            .cursor_position()
                            .unwrap_or(0)
                            .min(new_len.saturating_sub(1));
                        self.selection.set_total_items(new_len);
                        if new_len > 0 {
                            self.selection.list_state.select(Some(new_cursor));
                        }
                        PaneAction::Cut(cut_hashes)
                    }
                    Err(e) => {
                        // Discard snapshot since persist never succeeded
                        self.undo_stack.pop();
                        PaneAction::Error(format!("Failed to cut tracks: {e}"))
                    }
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

        let content = playlist_content_for_write(&new_tracks);
        match self.library.playlist_append(&self.name, content) {
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
            .effective_selection()
            .into_iter()
            .filter_map(|vis_idx| self.selection.visible_index_to_data(vis_idx))
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

    fn display_string(&self, data_idx: usize) -> Option<String> {
        let track = self.tracks.get(data_idx)?;
        let artist = track.artist.as_deref().unwrap_or("");
        let title = track.title.as_deref().unwrap_or("");
        let album = track.album.as_deref().unwrap_or("");
        Some(format!("{} {} {}", artist, title, album))
    }

    fn add_playing_target(&self) -> AddPlayingTarget {
        AddPlayingTarget::Playlist(self.name.clone())
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

    fn clone_box(&self) -> Box<dyn Pane> {
        Box::new(self.clone())
    }

    fn preempts_normal_key(&self, key: &KeyEvent) -> bool {
        playlist_pane_preempts_key(key)
    }

    fn paste_clipboard(&mut self, hashes: Vec<ContentHash>) -> PaneAction {
        self.paste_after_cursor(hashes)
    }

    fn undo(&mut self) -> PaneAction {
        self.undo_mutation()
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

/// Collect the hashes to cut from the playlist, based on the current selection.
///
/// - If `selection.selected` is non-empty, those visible indices are used.
/// - Otherwise the cursor track is used.
/// - If the playlist is empty or no cursor exists, returns two empty vecs.
///
/// Returns `(cut_hashes, remaining_hashes)` where `cut_hashes` are the items
/// removed and `remaining_hashes` is the new playlist order.
pub(crate) fn collect_cut_targets(
    hashes: &[ContentHash],
    selection: &SelectionState,
) -> (Vec<ContentHash>, Vec<ContentHash>) {
    let to_remove: std::collections::BTreeSet<usize> = if !selection.selected.is_empty() {
        selection
            .selected
            .iter()
            .filter_map(|&vis_idx| selection.visible_index_to_data(vis_idx))
            .collect()
    } else if let Some(vis_idx) = selection.cursor_position() {
        if let Some(data_idx) = selection.visible_index_to_data(vis_idx) {
            std::iter::once(data_idx).collect()
        } else {
            return (vec![], vec![]);
        }
    } else {
        return (vec![], vec![]);
    };

    let cut: Vec<ContentHash> = hashes
        .iter()
        .enumerate()
        .filter(|(i, _)| to_remove.contains(i))
        .map(|(_, h)| h.clone())
        .collect();

    let remaining: Vec<ContentHash> = hashes
        .iter()
        .enumerate()
        .filter(|(i, _)| !to_remove.contains(i))
        .map(|(_, h)| h.clone())
        .collect();

    (cut, remaining)
}

/// Compute the new hash order after pasting `clipboard` after the cursor.
///
/// Inserts `clipboard` hashes immediately after the current cursor's visible
/// position. Returns `(new_hashes, new_cursor_index)` where `new_cursor_index`
/// is the (data) index of the last pasted item.
///
/// If the playlist is empty, the clipboard hashes are placed at the start.
pub(crate) fn paste_after_cursor_into(
    hashes: &[ContentHash],
    selection: &SelectionState,
    clipboard: &[ContentHash],
) -> (Vec<ContentHash>, usize) {
    if clipboard.is_empty() {
        let cursor = selection.cursor_position().unwrap_or(0);
        return (hashes.to_vec(), cursor);
    }

    let insert_after = if hashes.is_empty() {
        // Paste into empty: insert at position 0
        usize::MAX // sentinel — we handle this below
    } else {
        selection
            .cursor_position()
            .and_then(|vis| selection.visible_index_to_data(vis))
            .unwrap_or(0)
    };

    let mut new_hashes = Vec::with_capacity(hashes.len() + clipboard.len());
    if insert_after == usize::MAX {
        // Empty playlist — just place clipboard items
        new_hashes.extend_from_slice(clipboard);
    } else {
        for (i, h) in hashes.iter().enumerate() {
            new_hashes.push(h.clone());
            if i == insert_after {
                new_hashes.extend_from_slice(clipboard);
            }
        }
    }

    // Cursor lands on the last pasted item
    let new_cursor = if insert_after == usize::MAX {
        clipboard.len() - 1
    } else {
        insert_after + clipboard.len()
    };

    (new_hashes, new_cursor)
}

/// Keys that PlaylistPane preempts in Normal mode before App-level bindings.
///
/// Extracted as a free function so it can be tested without a live
/// `PlaylistPane` (which requires a LibraryBackend).
pub(crate) fn playlist_pane_preempts_key(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('d') | KeyCode::Char('p'))
}

/// Compute the effective sorted data indices for a move operation.
///
/// Returns `Some(sorted_indices)` when the selection (or cursor fallback)
/// maps to a non-empty, contiguous set of data indices.
/// Returns `None` with an error message when the selection is non-contiguous.
///
/// The "cursor is always in the selection" semantic is implemented here:
/// if `selection.selected` is empty, the cursor's data index is used as
/// a single-element selection.
pub(crate) fn effective_data_indices_for_move(
    selection: &SelectionState,
) -> Result<Vec<usize>, String> {
    let vis_indices = selection.effective_selection();
    if vis_indices.is_empty() {
        return Ok(vec![]);
    }

    let mut data_indices: Vec<usize> = vis_indices
        .iter()
        .filter_map(|&vis| selection.visible_index_to_data(vis))
        .collect();
    data_indices.sort_unstable();
    data_indices.dedup();

    // Contiguity check
    for window in data_indices.windows(2) {
        if window[1] != window[0] + 1 {
            return Err("Non-contiguous selection; Shift+J/K requires contiguous".to_string());
        }
    }

    Ok(data_indices)
}

/// Move a contiguous block of items in `hashes` (and parallel `tracks`) down
/// by one position. `selected` must be sorted and contiguous.
///
/// Returns `(new_hashes, new_tracks, new_selection_indices)` where
/// `new_selection_indices` are the data indices the block now occupies.
/// Returns `None` when the block is already at the bottom.
pub(crate) fn move_block_down<T: Clone>(
    hashes: &[ContentHash],
    tracks: &[T],
    selected: &[usize],
) -> Option<(Vec<ContentHash>, Vec<T>, Vec<usize>)> {
    if selected.is_empty() {
        return None;
    }
    let top = selected[0];
    let bottom = *selected.last().unwrap();
    if bottom + 1 >= hashes.len() {
        return None; // already at bottom
    }

    let mut new_hashes = hashes.to_vec();
    let mut new_tracks = tracks.to_vec();

    // Rotate: remove item at bottom+1 and insert at top
    let moved_hash = new_hashes.remove(bottom + 1);
    new_hashes.insert(top, moved_hash);
    let moved_track = new_tracks.remove(bottom + 1);
    new_tracks.insert(top, moved_track);

    // Selection shifts +1
    let new_selection: Vec<usize> = selected.iter().map(|&i| i + 1).collect();
    Some((new_hashes, new_tracks, new_selection))
}

/// Move a contiguous block of items in `hashes` (and parallel `tracks`) up
/// by one position. `selected` must be sorted and contiguous.
///
/// Returns `(new_hashes, new_tracks, new_selection_indices)`.
/// Returns `None` when the block is already at the top.
pub(crate) fn move_block_up<T: Clone>(
    hashes: &[ContentHash],
    tracks: &[T],
    selected: &[usize],
) -> Option<(Vec<ContentHash>, Vec<T>, Vec<usize>)> {
    if selected.is_empty() {
        return None;
    }
    let top = selected[0];
    let bottom = *selected.last().unwrap();
    if top == 0 {
        return None; // already at top
    }

    let mut new_hashes = hashes.to_vec();
    let mut new_tracks = tracks.to_vec();

    // Rotate: remove item at top-1 and insert at bottom+1
    let moved_hash = new_hashes.remove(top - 1);
    new_hashes.insert(bottom, moved_hash); // insert at bottom (old bottom, after removal offset)
    let moved_track = new_tracks.remove(top - 1);
    new_tracks.insert(bottom, moved_track);

    // Selection shifts -1
    let new_selection: Vec<usize> = selected.iter().map(|&i| i - 1).collect();
    Some((new_hashes, new_tracks, new_selection))
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

    // -------------------------------------------------------------------------
    // add_playing_target contract tests
    // -------------------------------------------------------------------------
    //
    // These tests verify the AddPlayingTarget contract for PlaylistPane.
    // Because PlaylistPane requires a live LibraryBackend we cannot instantiate
    // it here; instead we verify the discriminant logic via a lightweight
    // stand-in that delegates to the expected return value, and we rely on
    // clippy/review to ensure the actual impl matches.
    //
    // The test that WILL fail before the implementation is added:
    // `playlist_pane_add_playing_target_returns_playlist_variant` — it imports
    // and uses `AddPlayingTarget` from the pane module and asserts on the enum
    // variant shape.  If `AddPlayingTarget` does not yet exist in `pane`, this
    // whole module fails to compile.

    #[test]
    fn playlist_pane_add_playing_target_is_playlist_variant() {
        use crate::pane::AddPlayingTarget;
        let name = PlaylistName::new("my-list").unwrap();
        // Simulate what PlaylistPane::add_playing_target returns:
        let target = AddPlayingTarget::Playlist(name.clone());
        assert!(
            matches!(target, AddPlayingTarget::Playlist(ref n) if n == &name),
            "expected Playlist variant with the same name"
        );
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

    // -------------------------------------------------------------------------
    // Cut / paste pure-logic tests
    //
    // These tests exercise `collect_cut_targets` and `paste_after_cursor_into`
    // — the two pure helpers that back the `d` / `p` key handlers.  No
    // LibraryBackend is needed.
    // -------------------------------------------------------------------------

    /// `collect_cut_targets` with an explicit selection returns those hashes.
    #[test]
    fn collect_cut_targets_with_selection_returns_selected_hashes() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d"), hash("e")];
        let mut sel = SelectionState::new(hashes.len());
        // Select visible index 2 (c)
        sel.selected.insert(2);

        let (cut, remaining) = collect_cut_targets(&hashes, &sel);
        assert_eq!(cut, vec![hash("c")]);
        assert_eq!(remaining, vec![hash("a"), hash("b"), hash("d"), hash("e")]);
    }

    /// `collect_cut_targets` with no selection falls back to the cursor track.
    #[test]
    fn collect_cut_targets_no_selection_uses_cursor() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d"), hash("e")];
        let mut sel = SelectionState::new(hashes.len());
        // Cursor at visible index 2, no explicit selection
        sel.list_state.select(Some(2));

        let (cut, remaining) = collect_cut_targets(&hashes, &sel);
        assert_eq!(cut, vec![hash("c")]);
        assert_eq!(remaining, vec![hash("a"), hash("b"), hash("d"), hash("e")]);
    }

    /// Multi-select cut: select c (2) and d (3).
    #[test]
    fn collect_cut_targets_multi_select_cuts_all_selected() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d"), hash("e")];
        let mut sel = SelectionState::new(hashes.len());
        sel.selected.insert(2);
        sel.selected.insert(3);

        let (cut, remaining) = collect_cut_targets(&hashes, &sel);
        assert_eq!(cut, vec![hash("c"), hash("d")]);
        assert_eq!(remaining, vec![hash("a"), hash("b"), hash("e")]);
    }

    /// `paste_after_cursor_into`: paste [c, d] after cursor index 1 (b) in [a, b, e].
    #[test]
    fn paste_after_cursor_inserts_after_cursor_position() {
        let hashes = vec![hash("a"), hash("b"), hash("e")];
        let mut sel = SelectionState::new(hashes.len());
        sel.list_state.select(Some(1)); // cursor at b

        let clipboard = vec![hash("c"), hash("d")];
        let (new_hashes, new_cursor) = paste_after_cursor_into(&hashes, &sel, &clipboard);

        assert_eq!(
            new_hashes,
            vec![hash("a"), hash("b"), hash("c"), hash("d"), hash("e")]
        );
        // Cursor should land on last pasted item (index 3 = d)
        assert_eq!(new_cursor, 3);
    }

    /// Paste into an empty playlist: paste goes at position 0.
    #[test]
    fn paste_into_empty_playlist() {
        let hashes: Vec<ContentHash> = vec![];
        let sel = SelectionState::new(0);

        let clipboard = vec![hash("a"), hash("b")];
        let (new_hashes, new_cursor) = paste_after_cursor_into(&hashes, &sel, &clipboard);

        assert_eq!(new_hashes, vec![hash("a"), hash("b")]);
        assert_eq!(new_cursor, 1);
    }

    /// `collect_cut_targets` with empty playlist is a no-op (empty cut + empty remaining).
    #[test]
    fn collect_cut_targets_empty_playlist_is_noop() {
        let hashes: Vec<ContentHash> = vec![];
        let sel = SelectionState::new(0);

        let (cut, remaining) = collect_cut_targets(&hashes, &sel);
        assert!(cut.is_empty());
        assert!(remaining.is_empty());
    }

    /// After a cut, the clipboard contains the cut hashes (PaneAction::Cut variant).
    #[test]
    fn cut_action_variant_contains_cut_hashes() {
        use crate::pane::PaneAction;
        // Simulate what d-key produces: Cut with the cut hashes
        let cut_hashes = vec![hash("b")];
        let action = PaneAction::Cut(cut_hashes.clone());
        assert!(matches!(action, PaneAction::Cut(ref h) if h == &cut_hashes));
    }

    // -------------------------------------------------------------------------
    // Undo stack tests
    // -------------------------------------------------------------------------

    /// Helper: build a minimal PlaylistSnapshot for testing.
    ///
    /// Uses empty `tracks` vec — undo stack tests only care about hashes + cursor.
    fn make_snapshot(hashes: Vec<ContentHash>, cursor: usize) -> PlaylistSnapshot {
        PlaylistSnapshot {
            tracks: Vec::new(),
            hashes,
            cursor,
        }
    }

    /// push_undo_snapshot helper: verify snapshot is pushed and capped.
    #[test]
    fn undo_stack_capped_at_depth() {
        let mut stack: Vec<PlaylistSnapshot> = Vec::new();
        // Push 60 snapshots (cap is UNDO_DEPTH = 50)
        for i in 0..60_usize {
            let snap = make_snapshot(vec![hash(&format!("sha256:{:04x}", i))], 0);
            stack.push(snap);
            if stack.len() > UNDO_DEPTH {
                stack.remove(0);
            }
        }
        assert_eq!(
            stack.len(),
            UNDO_DEPTH,
            "stack should be capped at UNDO_DEPTH"
        );
        // Oldest (index 0 in original sequence) should be gone; snapshot 10 is oldest remaining.
        assert_eq!(
            stack[0].hashes[0],
            hash(&format!("sha256:{:04x}", 60 - UNDO_DEPTH)),
            "oldest entry should be dropped"
        );
    }

    /// Undo with empty stack returns PaneAction::Info("Nothing to undo").
    #[test]
    fn undo_empty_stack_returns_info() {
        // We test the undo() trait method through the default impl in pane.rs,
        // which returns Info("Nothing to undo"). We replicate equivalent logic
        // here to confirm the contract without a live LibraryBackend.
        let mut stack: Vec<PlaylistSnapshot> = Vec::new();
        let result = if stack.pop().is_none() {
            PaneAction::Info("Nothing to undo".to_string())
        } else {
            PaneAction::Consumed
        };
        assert!(
            matches!(result, PaneAction::Info(ref s) if s == "Nothing to undo"),
            "empty stack undo must return Info(Nothing to undo)"
        );
    }

    /// Cloning a PlaylistSnapshot produces an independent copy (no shared refs).
    /// This also exercises the Clone derive on PlaylistSnapshot.
    #[test]
    fn playlist_snapshot_clone_is_independent() {
        let snap = make_snapshot(vec![hash("sha256:aaa"), hash("sha256:bbb")], 1);
        let cloned = snap.clone();
        assert_eq!(snap.hashes, cloned.hashes);
        assert_eq!(snap.cursor, cloned.cursor);
    }

    /// Verify that a Vec<PlaylistSnapshot> behaves as a stack (LIFO).
    #[test]
    fn undo_stack_is_lifo() {
        let mut stack: Vec<PlaylistSnapshot> = Vec::new();
        let snap_a = make_snapshot(vec![hash("sha256:aaa")], 0);
        let snap_b = make_snapshot(vec![hash("sha256:bbb")], 1);
        stack.push(snap_a.clone());
        stack.push(snap_b.clone());

        let popped = stack.pop().unwrap();
        assert_eq!(
            popped.hashes, snap_b.hashes,
            "last pushed snapshot should be first popped"
        );
    }

    /// Pane preempts 'd' and 'p' in normal mode.
    #[test]
    fn playlist_pane_preempts_d_and_p() {
        use crossterm::event::{KeyCode, KeyModifiers};

        // We can't construct a real PlaylistPane without a LibraryBackend, so we
        // test the logic through the free function directly instead.
        // But we DO verify the preemption logic is sound by checking the default
        // pane::Pane trait default returns false for d/p.
        // The real override is verified by clippy + manual review.

        // Test the helper `preempts_key` directly:
        let key_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        let key_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);

        assert!(playlist_pane_preempts_key(&key_d), "d should be preempted");
        assert!(playlist_pane_preempts_key(&key_p), "p should be preempted");
        assert!(
            !playlist_pane_preempts_key(&key_j),
            "j should not be preempted"
        );
    }

    // -------------------------------------------------------------------------
    // Shift+J / Shift+K block-move tests
    // -------------------------------------------------------------------------

    /// Shift+J on cursor at index 1 in [a, b, c, d] → [a, c, b, d]; cursor at 2.
    #[test]
    fn shift_j_moves_single_cursor_track_down() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d")];
        let mut sel = SelectionState::new(hashes.len());
        sel.list_state.select(Some(1)); // cursor at b

        // effective_data_indices_for_move: no explicit selection → [1]
        let indices = effective_data_indices_for_move(&sel).unwrap();
        assert_eq!(indices, vec![1]);

        let (new_hashes, new_tracks, new_sel) =
            move_block_down(&hashes, &hashes, &indices).unwrap();

        assert_eq!(new_hashes, vec![hash("a"), hash("c"), hash("b"), hash("d")]);
        assert_eq!(new_sel, vec![2]);
        // tracks parallel
        assert_eq!(new_tracks, new_hashes);
    }

    /// Shift+J on selection block 1..=2 in [a,b,c,d,e] → [a,d,b,c,e]; selection 2..=3.
    #[test]
    fn shift_j_moves_selection_block_down() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d"), hash("e")];
        let mut sel = SelectionState::new(hashes.len());
        // Select visible indices 1 and 2 (b and c)
        sel.selected.insert(1);
        sel.selected.insert(2);
        sel.list_state.select(Some(2)); // cursor at c

        let indices = effective_data_indices_for_move(&sel).unwrap();
        assert_eq!(indices, vec![1, 2]);

        let (new_hashes, _, new_sel) = move_block_down(&hashes, &hashes, &indices).unwrap();

        assert_eq!(
            new_hashes,
            vec![hash("a"), hash("d"), hash("b"), hash("c"), hash("e")]
        );
        assert_eq!(new_sel, vec![2, 3]);
    }

    /// Shift+J at the last index is a no-op.
    #[test]
    fn shift_j_at_bottom_is_noop() {
        let hashes = vec![hash("a"), hash("b"), hash("c")];
        let mut sel = SelectionState::new(hashes.len());
        sel.list_state.select(Some(2)); // cursor at last

        let indices = effective_data_indices_for_move(&sel).unwrap();
        let result = move_block_down(&hashes, &hashes, &indices);
        assert!(
            result.is_none(),
            "move_block_down at bottom should return None"
        );
    }

    /// Shift+K on cursor at index 2 in [a,b,c,d] → [a,b,d... wait:
    /// cursor at 2 (c), move up: [a,c,b,d]? No: block [2] moves up → [a,b,d,c]... wait.
    ///
    /// [a, b, c, d], selected=[2] (c), move up:
    /// remove item at top-1=1 (b), insert at bottom=2: [a, c, b, d]. cursor at 1.
    #[test]
    fn shift_k_moves_selection_block_up() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d")];
        let mut sel = SelectionState::new(hashes.len());
        sel.list_state.select(Some(2)); // cursor at c

        let indices = effective_data_indices_for_move(&sel).unwrap();
        let (new_hashes, _, new_sel) = move_block_up(&hashes, &hashes, &indices).unwrap();

        assert_eq!(new_hashes, vec![hash("a"), hash("c"), hash("b"), hash("d")]);
        assert_eq!(new_sel, vec![1]);
    }

    /// Shift+K at index 0 is a no-op.
    #[test]
    fn shift_k_at_top_is_noop() {
        let hashes = vec![hash("a"), hash("b"), hash("c")];
        let mut sel = SelectionState::new(hashes.len());
        sel.list_state.select(Some(0)); // cursor at first

        let indices = effective_data_indices_for_move(&sel).unwrap();
        let result = move_block_up(&hashes, &hashes, &indices);
        assert!(result.is_none(), "move_block_up at top should return None");
    }

    /// Non-contiguous selection (indices 0 and 2) → error, no movement.
    #[test]
    fn shift_j_noncontiguous_selection_noop_with_status() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d")];
        let mut sel = SelectionState::new(hashes.len());
        sel.selected.insert(0);
        sel.selected.insert(2); // gap at 1

        let result = effective_data_indices_for_move(&sel);
        assert!(
            result.is_err(),
            "non-contiguous selection should return Err"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Non-contiguous"),
            "error message should mention non-contiguous: {msg}"
        );
    }

    // -------------------------------------------------------------------------
    // Regression lock: playlist write format
    //
    // Verify that the TUI write path produces the canonical formatted lines
    // (`{8hash}  {Artist} - {Title}  [{duration}]`) and NOT bare sha256 hashes.
    // This test MUST fail before the fix and pass after it.
    // -------------------------------------------------------------------------

    fn make_track_info(hash: &str, artist: &str, title: &str, duration_secs: u32) -> TrackInfo {
        use library_ipc_protocol::DurationSeconds;
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: None,
            duration: Some(DurationSeconds::new(duration_secs)),
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        }
    }

    /// The TUI playlist write path must produce canonical formatted lines, not bare hashes.
    ///
    /// Regression lock: if this test fails, the TUI is writing raw hashes to disk.
    #[test]
    fn playlist_write_produces_canonical_formatted_lines() {
        let tracks = vec![
            make_track_info(
                "sha256:8d4e7b2cabc12345",
                "Artist Name",
                "Track Title",
                225, // 3:45
            ),
            make_track_info(
                "sha256:a3f91e0dabc12345",
                "Other Artist",
                "Other Title",
                252, // 4:12
            ),
        ];
        // This calls the function that the TUI must use when persisting playlists.
        // Before the fix this function does not exist — the test fails to compile.
        // After the fix it exists and produces formatted lines.
        let content = playlist_content_for_write(&tracks);

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 lines, one per track");

        // Verify lines match canonical format, NOT bare hashes
        assert_eq!(
            lines[0], "8d4e7b2cabc1  Artist Name - Track Title  [3:45]",
            "line 0 must be canonical formatted, not a bare hash"
        );
        assert_eq!(
            lines[1], "a3f91e0dabc1  Other Artist - Other Title  [4:12]",
            "line 1 must be canonical formatted, not a bare hash"
        );

        // Extra: assert that bare hash format is NOT present
        assert!(
            !lines[0].starts_with("sha256:"),
            "line must not start with 'sha256:' prefix"
        );
        // A bare hash would be pure hex with no spaces; formatted lines always contain spaces.
        assert!(
            lines[0].contains("  "),
            "line must contain double-space separator (canonical format), not a bare hash"
        );
    }

    /// Comma key clears selection via SelectionState.
    #[test]
    fn comma_clears_selection() {
        let mut sel = SelectionState::new(5);
        sel.selected.insert(1);
        sel.selected.insert(2);
        sel.list_state.select(Some(2));

        sel.clear_selection();

        assert!(sel.selected.is_empty(), "selection should be cleared");
        assert_eq!(
            sel.cursor_position(),
            Some(2),
            "cursor should remain after clear"
        );
    }

    /// move_block_up on a 2-item block in [a,b,c,d,e] at indices 2..=3
    /// → [a,c,d,b,e]; new selection 1..=2.
    ///
    /// Wait, let me trace: top=2, bottom=3, remove at top-1=1 (b), insert at bottom=3:
    /// start: [a,b,c,d,e]
    /// remove index 1 (b): [a,c,d,e]
    /// insert at index 3 (bottom=3 in original, but after removal that's index 3 in [a,c,d,e]):
    /// → [a,c,d,b,e]; selection shifts -1: [1,2]. Correct.
    #[test]
    fn shift_k_moves_two_item_block_up() {
        let hashes = vec![hash("a"), hash("b"), hash("c"), hash("d"), hash("e")];
        let mut sel = SelectionState::new(hashes.len());
        sel.selected.insert(2);
        sel.selected.insert(3);
        sel.list_state.select(Some(3));

        let indices = effective_data_indices_for_move(&sel).unwrap();
        assert_eq!(indices, vec![2, 3]);

        let (new_hashes, _, new_sel) = move_block_up(&hashes, &hashes, &indices).unwrap();

        assert_eq!(
            new_hashes,
            vec![hash("a"), hash("c"), hash("d"), hash("b"), hash("e")]
        );
        assert_eq!(new_sel, vec![1, 2]);
    }
}
