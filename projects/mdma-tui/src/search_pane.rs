use crate::pane::{Pane, PaneAction, PaneKind};
use crate::search_parse::parse_query;
use crate::selection::SelectionState;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, LibraryBackend, PlaylistName, TrackInfo};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Span,
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::rc::Rc;

/// Search pane — lets the user type a query and browse the results.
pub struct SearchPane {
    query_text: String,
    editing: bool,
    tracks: Vec<TrackInfo>,
    selection: SelectionState,
    library: Rc<LibraryBackend>,
    /// The query string that produced the current `tracks` list (empty = no search run yet).
    last_executed_query: String,
}

impl SearchPane {
    pub fn new(library: Rc<LibraryBackend>) -> Self {
        Self {
            query_text: String::new(),
            editing: false,
            tracks: Vec::new(),
            selection: SelectionState::new(0),
            library,
            last_executed_query: String::new(),
        }
    }

    /// Construct a `SearchPane` with a pre-filled query and immediately execute it.
    ///
    /// The pane starts in non-editing mode so the user can browse results right away.
    pub fn with_query(library: Rc<LibraryBackend>, query: String) -> (Self, PaneAction) {
        let mut pane = Self {
            query_text: query,
            editing: false,
            tracks: Vec::new(),
            selection: SelectionState::new(0),
            library,
            last_executed_query: String::new(),
        };
        let action = pane.execute_search();
        (pane, action)
    }

    /// Execute the current `query_text` against the library backend.
    ///
    /// On success updates `tracks` and resets selection.
    /// On failure returns a `PaneAction::Error`.
    fn execute_search(&mut self) -> PaneAction {
        let query = parse_query(&self.query_text);
        match self.library.search(&query) {
            Ok(tracks) => {
                self.last_executed_query = self.query_text.clone();
                let len = tracks.len();
                self.tracks = tracks;
                self.selection.set_total_items(len);
                PaneAction::Consumed
            }
            Err(e) => PaneAction::Error(format!("Search failed: {e}")),
        }
    }

    /// Run the search only if the query text has changed since the last execution.
    /// Called on each keystroke to provide live results.
    fn maybe_execute_search(&mut self) -> PaneAction {
        if self.query_text != self.last_executed_query {
            self.execute_search()
        } else {
            PaneAction::Consumed
        }
    }
}

impl Pane for SearchPane {
    fn title(&self) -> &str {
        // The outer frame in ui.rs renders the title. We return a static string
        // so the pane type is visible; the current query is shown inline in the
        // input row rendered by render().
        "Search"
    }

    fn render(&self, f: &mut Frame, area: Rect) {
        // Split the area: one line for the query input, rest for results.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);

        let input_area = chunks[0];
        let list_area = chunks[1];

        // Render the query input line.
        let cursor = if self.editing { "_" } else { "" };
        let input_text = format!("> {}{}", self.query_text, cursor);
        let input_style = if self.editing {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(Span::styled(input_text, input_style)),
            input_area,
        );

        // Render the track list (no inner block — outer frame provides the border).
        let block = Block::default().borders(Borders::NONE);
        // If a search has been run and returned no results, show a hint.
        if self.tracks.is_empty() && !self.last_executed_query.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "(no matches)",
                    Style::default().fg(Color::DarkGray),
                )),
                list_area,
            );
        } else {
            render_track_list(f, list_area, &self.tracks, &self.selection, block);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> PaneAction {
        if self.editing {
            match key.code {
                KeyCode::Char(c) => {
                    self.query_text.push(c);
                    self.maybe_execute_search()
                }
                KeyCode::Backspace => {
                    self.query_text.pop();
                    self.maybe_execute_search()
                }
                KeyCode::Enter => {
                    // Commit: run the search (no-op if already current) and exit editing.
                    let action = self.maybe_execute_search();
                    self.editing = false;
                    action
                }
                KeyCode::Esc => {
                    // Cancel — restore query text to last executed, keep old results.
                    self.query_text = self.last_executed_query.clone();
                    self.editing = false;
                    PaneAction::Consumed
                }
                _ => PaneAction::Consumed,
            }
        } else {
            match key.code {
                KeyCode::Char('/') | KeyCode::Enter => {
                    self.editing = true;
                    self.query_text.clear();
                    PaneAction::Consumed
                }
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
                _ => PaneAction::Ignored,
            }
        }
    }

    fn resolve_selection(&self) -> Vec<ContentHash> {
        self.selection
            .effective_selection()
            .into_iter()
            .filter_map(|vis_idx| self.selection.visible_index_to_data(vis_idx))
            .map(|data_idx| self.tracks[data_idx].content_hash.clone())
            .collect()
    }

    fn selection_state(&self) -> &SelectionState {
        &self.selection
    }

    fn selection_state_mut(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    fn item_count(&self) -> usize {
        self.tracks.len()
    }

    fn pane_kind(&self) -> PaneKind {
        PaneKind::Search
    }

    fn playlist_name(&self) -> Option<&PlaylistName> {
        None
    }

    fn display_string(&self, data_idx: usize) -> Option<String> {
        let track = self.tracks.get(data_idx)?;
        let artist = track.artist.as_deref().unwrap_or("");
        let title = track.title.as_deref().unwrap_or("");
        let album = track.album.as_deref().unwrap_or("");
        Some(format!("{} {} {}", artist, title, album))
    }
}

// =========================================================================
// Tests
// =========================================================================
//
// NOTE: `SearchPane` requires a live `LibraryBackend` (IPC socket) to
// construct. Tests instead exercise the pure logic — the same resolve logic
// used in `resolve_selection` — directly on `SelectionState` + a track slice,
// without instantiating `SearchPane` or connecting to any backend.

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `TrackInfo` with just the content_hash set.
    fn make_track(hash: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
        }
    }

    /// Reproduce the resolve_selection logic for test purposes.
    fn resolve(selection: &SelectionState, tracks: &[TrackInfo]) -> Vec<ContentHash> {
        selection
            .effective_selection()
            .into_iter()
            .filter_map(|vis_idx| selection.visible_index_to_data(vis_idx))
            .map(|data_idx| tracks[data_idx].content_hash.clone())
            .collect()
    }

    #[test]
    fn resolve_selection_maps_visible_to_data() {
        let tracks = vec![
            make_track("sha256:aaa"),
            make_track("sha256:bbb"),
            make_track("sha256:ccc"),
        ];
        let mut selection = SelectionState::new(tracks.len());
        // Select visible indices 0 and 2.
        selection.selected.insert(0);
        selection.selected.insert(2);

        let hashes = resolve(&selection, &tracks);

        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], ContentHash::new("sha256:aaa"));
        assert_eq!(hashes[1], ContentHash::new("sha256:ccc"));
    }

    #[test]
    fn selection_reset_on_new_search() {
        // Verify that set_total_items clears selection and resets visible items,
        // which is exactly what execute_search calls after a successful result.
        let mut selection = SelectionState::new(3);
        selection.select_all();
        assert_eq!(selection.selected.len(), 3);

        // Simulate what execute_search does after getting results.
        let new_track_count = 5;
        selection.set_total_items(new_track_count);

        assert!(
            selection.selected.is_empty(),
            "selection should be cleared after set_total_items"
        );
        assert_eq!(selection.visible_count(), new_track_count);
        assert_eq!(selection.cursor_position(), Some(0));
    }

    #[test]
    fn resolve_selection_falls_back_to_cursor_when_nothing_explicit() {
        // With effective_selection, the cursor position is an implicit selection.
        let tracks = vec![make_track("sha256:aaa"), make_track("sha256:bbb")];
        let selection = SelectionState::new(tracks.len());
        // No explicit selection — cursor at 0 → resolves to first track.
        let hashes = resolve(&selection, &tracks);
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0], ContentHash::new("sha256:aaa"));
    }

    #[test]
    fn resolve_selection_respects_filter_visibility() {
        // If a filter has narrowed visibility, selected visible indices map
        // through the filter to the correct data indices.
        let tracks = vec![
            make_track("sha256:aaa"), // data idx 0
            make_track("sha256:bbb"), // data idx 1
            make_track("sha256:ccc"), // data idx 2
            make_track("sha256:ddd"), // data idx 3
        ];
        let mut selection = SelectionState::new(tracks.len());
        // Push filter: only keep even data indices → visible = [0, 2]
        selection.push_filter(|i| i % 2 == 0);
        // Select visible index 1 → data index 2
        selection.selected.insert(1);

        let hashes = resolve(&selection, &tracks);

        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0], ContentHash::new("sha256:ccc"));
    }
}
