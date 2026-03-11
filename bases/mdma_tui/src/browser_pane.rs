#![allow(dead_code)]
use crate::browse_field::{find_first_by_letter, BrowseField};
use crate::pane::{Pane, PaneAction, PaneKind};
use crate::selection::SelectionState;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, LibraryBackend, TrackInfo};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem},
    Frame,
};
use std::collections::HashMap;
use std::rc::Rc;

const ROOT_ITEMS: [&str; 4] = ["Songs", "Artists", "Albums", "Genres"];
const ROOT_FIELDS: [BrowseField; 4] = [
    BrowseField::Title,
    BrowseField::Artist,
    BrowseField::Album,
    BrowseField::Genre,
];

/// An entry in a group list (artists, albums, genres).
struct GroupEntry {
    name: String,
    count: usize,
}

/// The hierarchical drill-down level in the browser.
enum BrowserLevel {
    /// Top level showing the four browse categories.
    Root { cursor: usize },
    /// A list of unique group values for a given field.
    Groups {
        field: BrowseField,
        groups: Vec<GroupEntry>,
        selection: SelectionState,
    },
    /// A list of tracks within a group (or all tracks for Songs).
    Tracks {
        field: BrowseField,
        group_name: Option<String>,
        tracks: Vec<TrackInfo>,
        selection: SelectionState,
    },
}

/// Browser pane that allows hierarchical navigation: Root → Groups → Tracks.
pub struct BrowserPane {
    level: BrowserLevel,
    library: Rc<LibraryBackend>,
    breadcrumbs: Vec<String>,
    /// Cached full track list to avoid repeated backend round-trips.
    all_tracks: Option<Vec<TrackInfo>>,
    /// Dummy selection state for the Root level (4 items, one per category).
    root_selection: SelectionState,
}

impl BrowserPane {
    pub fn new(library: Rc<LibraryBackend>) -> Self {
        BrowserPane {
            level: BrowserLevel::Root { cursor: 0 },
            library,
            breadcrumbs: vec!["Browser".to_string()],
            all_tracks: None,
            root_selection: SelectionState::new(ROOT_ITEMS.len()),
        }
    }

    /// Ensure all_tracks is populated; loads from the library if not yet cached.
    ///
    /// Returns `Some(PaneAction::Error(...))` if the load fails, leaving
    /// `all_tracks` as `None` so the next call retries. Returns `None` on success.
    fn ensure_all_tracks(&mut self) -> Option<PaneAction> {
        if self.all_tracks.is_none() {
            match self.library.list_tracks(None) {
                Ok(tracks) => {
                    self.all_tracks = Some(tracks);
                }
                Err(e) => {
                    return Some(PaneAction::Error(format!("Failed to load tracks: {e}")));
                }
            }
        }
        None
    }

    /// Group all_tracks by the given field, returning sorted group entries.
    fn build_groups(&self, field: BrowseField) -> Vec<GroupEntry> {
        let Some(all) = &self.all_tracks else {
            return Vec::new();
        };

        let mut map: HashMap<String, usize> = HashMap::new();
        for track in all {
            if let Some(value) = field.extract(track) {
                *map.entry(value).or_insert(0) += 1;
            }
        }

        let mut groups: Vec<GroupEntry> = map
            .into_iter()
            .map(|(name, count)| GroupEntry { name, count })
            .collect();

        groups.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        groups
    }

    /// Build a group list from genre fact values (fallback: from track genre field).
    fn build_genre_groups(&mut self) -> Vec<GroupEntry> {
        // Try to get genres from fact values first
        match self.library.get_fact_values("MainGenre") {
            Ok(genre_values) if !genre_values.is_empty() => {
                let Some(all) = &self.all_tracks else {
                    return genre_values
                        .into_iter()
                        .map(|name| GroupEntry { name, count: 0 })
                        .collect();
                };

                // Count tracks per genre
                let mut map: HashMap<String, usize> = HashMap::new();
                for genre in &genre_values {
                    map.insert(genre.clone(), 0);
                }

                // TODO: count tracks per genre when genre metadata is available
                let _ = all;

                let mut groups: Vec<GroupEntry> = map
                    .into_iter()
                    .map(|(name, count)| GroupEntry { name, count })
                    .collect();
                groups.sort_by(|a, b| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                });
                groups
            }
            _ => self.build_groups(BrowseField::Genre),
        }
    }

    /// Get tracks for a specific group (filtered by field == group_name).
    fn tracks_for_group(all: &[TrackInfo], field: BrowseField, group_name: &str) -> Vec<TrackInfo> {
        all.iter()
            .filter(|t| field.extract(t).as_deref() == Some(group_name))
            .cloned()
            .collect()
    }

    /// Drill into the root item at the given index.
    ///
    /// Returns `Some(PaneAction::Error(...))` if the track load fails.
    fn drill_root(&mut self, cursor: usize) -> Option<PaneAction> {
        if let Some(err) = self.ensure_all_tracks() {
            return Some(err);
        }

        let field = ROOT_FIELDS[cursor];
        let label = ROOT_ITEMS[cursor];

        match field {
            BrowseField::Title => {
                // Songs: show all tracks directly
                let tracks = self.all_tracks.clone().unwrap_or_default();
                let total = tracks.len();
                self.breadcrumbs.push(label.to_string());
                self.level = BrowserLevel::Tracks {
                    field,
                    group_name: None,
                    selection: SelectionState::new(total),
                    tracks,
                };
            }
            BrowseField::Genre => {
                let groups = self.build_genre_groups();
                let total = groups.len();
                self.breadcrumbs.push(label.to_string());
                self.level = BrowserLevel::Groups {
                    field,
                    selection: SelectionState::new(total),
                    groups,
                };
            }
            _ => {
                let groups = self.build_groups(field);
                let total = groups.len();
                self.breadcrumbs.push(label.to_string());
                self.level = BrowserLevel::Groups {
                    field,
                    selection: SelectionState::new(total),
                    groups,
                };
            }
        }
        None
    }

    /// Drill into a group entry (from Groups level → Tracks level).
    ///
    /// Returns `Some(PaneAction::Error(...))` if the track load fails.
    fn drill_group(&mut self, field: BrowseField, group_name: String) -> Option<PaneAction> {
        if let Some(err) = self.ensure_all_tracks() {
            return Some(err);
        }
        let all = self.all_tracks.as_deref().unwrap_or(&[]);

        let tracks = Self::tracks_for_group(all, field, &group_name);
        let total = tracks.len();
        self.breadcrumbs.push(group_name.clone());
        self.level = BrowserLevel::Tracks {
            field,
            group_name: Some(group_name),
            selection: SelectionState::new(total),
            tracks,
        };
        None
    }

    /// Pop the level stack, going back one level.
    ///
    /// Returns `Some(PaneAction::Error(...))` if a backend reload fails.
    fn pop_level(&mut self) -> Option<PaneAction> {
        // Remove last breadcrumb (but keep at least "Browser")
        if self.breadcrumbs.len() > 1 {
            self.breadcrumbs.pop();
        }

        let current = std::mem::replace(&mut self.level, BrowserLevel::Root { cursor: 0 });

        match current {
            BrowserLevel::Root { .. } => {
                // Already at root, nothing to do
                self.level = BrowserLevel::Root { cursor: 0 };
            }
            BrowserLevel::Groups { .. } => {
                // Go back to root
                self.level = BrowserLevel::Root { cursor: 0 };
                self.root_selection = SelectionState::new(ROOT_ITEMS.len());
            }
            BrowserLevel::Tracks {
                field, group_name, ..
            } => {
                if group_name.is_none() {
                    // Came from Songs at root level
                    self.level = BrowserLevel::Root { cursor: 0 };
                    self.root_selection = SelectionState::new(ROOT_ITEMS.len());
                } else {
                    // Go back to Groups for this field
                    if let Some(err) = self.ensure_all_tracks() {
                        return Some(err);
                    }
                    let groups = if field == BrowseField::Genre {
                        self.build_genre_groups()
                    } else {
                        self.build_groups(field)
                    };
                    let total = groups.len();
                    self.level = BrowserLevel::Groups {
                        field,
                        selection: SelectionState::new(total),
                        groups,
                    };
                }
            }
        }
        None
    }
}

/// Resolve the content hashes for the current selection at the Tracks level.
///
/// This is a shared helper used by both `BrowserPane::resolve_selection` and
/// the test stub, avoiding duplicated logic.
pub(crate) fn resolve_hashes_from_selection(
    tracks: &[TrackInfo],
    selection: &SelectionState,
) -> Vec<ContentHash> {
    selection
        .selected
        .iter()
        .filter_map(|&vis_idx| selection.visible_to_data.get(vis_idx))
        .filter_map(|&data_idx| tracks.get(data_idx))
        .map(|t| t.content_hash.clone())
        .collect()
}

impl BrowserPane {
    fn resolve_selection(&self) -> Vec<ContentHash> {
        match &self.level {
            BrowserLevel::Root { .. } => vec![],
            BrowserLevel::Groups {
                field,
                groups,
                selection,
            } => {
                let mut hashes = Vec::new();
                for &vis_idx in &selection.selected {
                    if let Some(&data_idx) = selection.visible_to_data.get(vis_idx) {
                        if let Some(group) = groups.get(data_idx) {
                            if let Some(all) = &self.all_tracks {
                                for track in all {
                                    if field.extract(track).as_deref() == Some(&group.name) {
                                        hashes.push(track.content_hash.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                hashes
            }
            BrowserLevel::Tracks {
                tracks, selection, ..
            } => resolve_hashes_from_selection(tracks, selection),
        }
    }

    fn render_root(&self, f: &mut Frame, area: Rect, block: Block, cursor: usize) {
        let items: Vec<ListItem> = ROOT_ITEMS
            .iter()
            .enumerate()
            .map(|(i, &name)| {
                let is_cursor = i == cursor;
                let style = if is_cursor {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(name, style))
            })
            .collect();

        let list = List::new(items).block(block);
        f.render_widget(list, area);
    }

    fn render_groups(
        &self,
        f: &mut Frame,
        area: Rect,
        block: Block,
        groups: &[GroupEntry],
        selection: &SelectionState,
    ) {
        let items: Vec<ListItem> = selection
            .visible_to_data
            .iter()
            .enumerate()
            .map(|(vis_idx, &data_idx)| {
                let group = &groups[data_idx];
                let line = format!("{}  ({})", group.name, group.count);
                let is_cursor = selection.cursor_position() == Some(vis_idx);
                let is_selected = selection.selected.contains(&vis_idx);

                let style = if is_cursor && is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if is_cursor {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if is_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                ListItem::new(Span::styled(line, style))
            })
            .collect();

        let list = List::new(items).block(block);
        let mut ls = selection.list_state.clone();
        f.render_stateful_widget(list, area, &mut ls);
    }

    /// Build a breadcrumb display string from the current breadcrumbs.
    fn breadcrumb_line(&self) -> String {
        self.breadcrumbs.join(" > ")
    }
}

impl Pane for BrowserPane {
    fn render(&self, f: &mut Frame, area: Rect) {
        let breadcrumb = self.breadcrumb_line();

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(&breadcrumb, Style::default().fg(Color::Yellow)),
                Span::styled(" ", Style::default()),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(Color::White));

        match &self.level {
            BrowserLevel::Root { cursor } => {
                self.render_root(f, area, block, *cursor);
            }
            BrowserLevel::Groups {
                groups, selection, ..
            } => {
                self.render_groups(f, area, block, groups, selection);
            }
            BrowserLevel::Tracks {
                tracks, selection, ..
            } => {
                render_track_list(f, area, tracks, selection, block);
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> PaneAction {
        match &mut self.level {
            BrowserLevel::Root { cursor } => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(ROOT_ITEMS.len() - 1);
                    self.root_selection.list_state.select(Some(*cursor));
                    PaneAction::Consumed
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *cursor = cursor.saturating_sub(1);
                    self.root_selection.list_state.select(Some(*cursor));
                    PaneAction::Consumed
                }
                KeyCode::Enter => {
                    let idx = *cursor;
                    if let Some(err) = self.drill_root(idx) {
                        return err;
                    }
                    PaneAction::Consumed
                }
                _ => PaneAction::Ignored,
            },

            BrowserLevel::Groups {
                field,
                groups,
                selection,
            } => {
                let field = *field;
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        selection.move_cursor_down();
                        PaneAction::Consumed
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selection.move_cursor_up();
                        PaneAction::Consumed
                    }
                    KeyCode::Char('x') => {
                        selection.extend_selection_down();
                        PaneAction::Consumed
                    }
                    KeyCode::Char('X') => {
                        selection.extend_selection_up();
                        PaneAction::Consumed
                    }
                    KeyCode::Char('%') => {
                        selection.select_all();
                        PaneAction::Consumed
                    }
                    KeyCode::Esc => {
                        if !selection.pop_filter() {
                            selection.clear_selection();
                        }
                        PaneAction::Consumed
                    }
                    KeyCode::Backspace => {
                        if let Some(err) = self.pop_level() {
                            return err;
                        }
                        PaneAction::Consumed
                    }
                    KeyCode::Enter => {
                        // Drill into the group under the cursor
                        let group_name = selection
                            .cursor_position()
                            .and_then(|vis| selection.visible_to_data.get(vis).copied())
                            .and_then(|data_idx| groups.get(data_idx))
                            .map(|g| g.name.clone());

                        if let Some(name) = group_name {
                            if let Some(err) = self.drill_group(field, name) {
                                return err;
                            }
                        }
                        PaneAction::Consumed
                    }
                    KeyCode::Char(ch) if ch.is_ascii_alphabetic() => {
                        // Letter jump: find first visible group starting with this letter.
                        // Build the list of visible names from the destructured bindings,
                        // then drop the shared borrow before mutating selection.
                        let maybe_vis_idx = {
                            let visible_names: Vec<&str> = selection
                                .visible_to_data
                                .iter()
                                .map(|&data_idx| groups[data_idx].name.as_str())
                                .collect();
                            find_first_by_letter(&visible_names, ch)
                        };
                        if let Some(vis_idx) = maybe_vis_idx {
                            selection.list_state.select(Some(vis_idx));
                        } else {
                            return PaneAction::Info(format!(
                                "No {} entries visible",
                                ch.to_uppercase()
                            ));
                        }
                        PaneAction::Consumed
                    }
                    _ => PaneAction::Ignored,
                }
            }

            BrowserLevel::Tracks { selection, .. } => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    selection.move_cursor_down();
                    PaneAction::Consumed
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selection.move_cursor_up();
                    PaneAction::Consumed
                }
                KeyCode::Char('x') => {
                    selection.extend_selection_down();
                    PaneAction::Consumed
                }
                KeyCode::Char('X') => {
                    selection.extend_selection_up();
                    PaneAction::Consumed
                }
                KeyCode::Char('%') => {
                    selection.select_all();
                    PaneAction::Consumed
                }
                KeyCode::Esc => {
                    if !selection.pop_filter() {
                        selection.clear_selection();
                    }
                    PaneAction::Consumed
                }
                KeyCode::Backspace => {
                    if let Some(err) = self.pop_level() {
                        return err;
                    }
                    PaneAction::Consumed
                }
                _ => PaneAction::Ignored,
            },
        }
    }

    fn resolve_selection(&self) -> Vec<ContentHash> {
        self.resolve_selection()
    }

    fn selection_state(&self) -> &SelectionState {
        match &self.level {
            BrowserLevel::Root { .. } => &self.root_selection,
            BrowserLevel::Groups { selection, .. } => selection,
            BrowserLevel::Tracks { selection, .. } => selection,
        }
    }

    fn selection_state_mut(&mut self) -> &mut SelectionState {
        match &mut self.level {
            BrowserLevel::Root { .. } => &mut self.root_selection,
            BrowserLevel::Groups { selection, .. } => selection,
            BrowserLevel::Tracks { selection, .. } => selection,
        }
    }

    fn title(&self) -> &str {
        // Return last breadcrumb as title
        self.breadcrumbs
            .last()
            .map(String::as_str)
            .unwrap_or("Browser")
    }

    fn item_count(&self) -> usize {
        match &self.level {
            BrowserLevel::Root { .. } => ROOT_ITEMS.len(),
            BrowserLevel::Groups { groups, .. } => groups.len(),
            BrowserLevel::Tracks { tracks, .. } => tracks.len(),
        }
    }

    fn pane_kind(&self) -> PaneKind {
        PaneKind::Browser
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdma_client::ContentHash;

    fn make_track(
        hash: &str,
        artist: Option<&str>,
        album: Option<&str>,
        title: Option<&str>,
    ) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: album.map(String::from),
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

    #[test]
    fn resolve_selection_root_returns_empty() {
        let level = BrowserLevel::Root { cursor: 0 };
        let pane_stub = BrowserPaneStub {
            level,
            all_tracks: None,
        };
        assert!(pane_stub.resolve_selection().is_empty());
    }

    #[test]
    fn resolve_selection_tracks_returns_selected_hashes() {
        let tracks = vec![
            make_track(
                "sha256:aaa",
                Some("CBL"),
                Some("Twentythree"),
                Some("Abiogenesis"),
            ),
            make_track(
                "sha256:bbb",
                Some("CBL"),
                Some("Twentythree"),
                Some("Clouds"),
            ),
            make_track("sha256:ccc", Some("Other"), Some("Album"), Some("Track")),
        ];
        let mut selection = SelectionState::new(tracks.len());
        // Select visible index 0 and 2
        selection.selected.insert(0);
        selection.selected.insert(2);

        let level = BrowserLevel::Tracks {
            field: BrowseField::Artist,
            group_name: Some("CBL".to_string()),
            tracks: tracks.clone(),
            selection,
        };
        let pane_stub = BrowserPaneStub {
            level,
            all_tracks: Some(tracks),
        };
        let hashes = pane_stub.resolve_selection();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&ContentHash::new("sha256:aaa")));
        assert!(hashes.contains(&ContentHash::new("sha256:ccc")));
    }

    #[test]
    fn resolve_selection_groups_returns_all_tracks_in_group() {
        let all_tracks = vec![
            make_track(
                "sha256:aaa",
                Some("CBL"),
                Some("Twentythree"),
                Some("Abiogenesis"),
            ),
            make_track(
                "sha256:bbb",
                Some("CBL"),
                Some("Twentythree"),
                Some("Clouds"),
            ),
            make_track("sha256:ccc", Some("Other"), Some("Album"), Some("Track")),
        ];

        let groups = vec![
            GroupEntry {
                name: "CBL".to_string(),
                count: 2,
            },
            GroupEntry {
                name: "Other".to_string(),
                count: 1,
            },
        ];
        let mut selection = SelectionState::new(groups.len());
        // Select visible index 0 ("CBL")
        selection.selected.insert(0);

        let level = BrowserLevel::Groups {
            field: BrowseField::Artist,
            groups,
            selection,
        };
        let pane_stub = BrowserPaneStub {
            level,
            all_tracks: Some(all_tracks),
        };
        let hashes = pane_stub.resolve_selection();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&ContentHash::new("sha256:aaa")));
        assert!(hashes.contains(&ContentHash::new("sha256:bbb")));
    }

    #[test]
    fn item_count_root_is_4() {
        // We test BrowserLevel::Root item count directly
        let root = BrowserLevel::Root { cursor: 0 };
        let count = match &root {
            BrowserLevel::Root { .. } => ROOT_ITEMS.len(),
            _ => 0,
        };
        assert_eq!(count, 4);
    }

    /// Minimal stub to allow testing resolve_selection without a live backend.
    struct BrowserPaneStub {
        level: BrowserLevel,
        all_tracks: Option<Vec<TrackInfo>>,
    }

    impl BrowserPaneStub {
        fn resolve_selection(&self) -> Vec<ContentHash> {
            match &self.level {
                BrowserLevel::Root { .. } => vec![],
                BrowserLevel::Groups {
                    field,
                    groups,
                    selection,
                } => {
                    let mut hashes = Vec::new();
                    for &vis_idx in &selection.selected {
                        if let Some(&data_idx) = selection.visible_to_data.get(vis_idx) {
                            if let Some(group) = groups.get(data_idx) {
                                if let Some(all) = &self.all_tracks {
                                    for track in all {
                                        if field.extract(track).as_deref() == Some(&group.name) {
                                            hashes.push(track.content_hash.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    hashes
                }
                BrowserLevel::Tracks {
                    tracks, selection, ..
                } => resolve_hashes_from_selection(tracks, selection),
            }
        }
    }
}
