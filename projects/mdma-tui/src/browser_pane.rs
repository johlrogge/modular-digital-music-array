#![allow(dead_code)]
use crate::browse_field::BrowseField;
use crate::link::Link;
use crate::pane::{Pane, PaneAction, PaneKind};
use crate::selection::SelectionState;
use crate::theme::RowState;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, TrackInfo};
use ratatui::{
    layout::Rect,
    text::Span,
    widgets::{Block, Borders, List, ListItem},
    Frame,
};
use std::collections::HashMap;

const ROOT_ITEMS: [&str; 4] = ["Tracks", "Artists", "Albums", "Genres"];
const ROOT_FIELDS: [BrowseField; 4] = [
    BrowseField::Title,
    BrowseField::Artist,
    BrowseField::Album,
    BrowseField::Genre,
];

/// An entry in a group list (artists, albums, genres).
#[derive(Clone)]
struct GroupEntry {
    name: String,
    count: usize,
}

/// The hierarchical drill-down level in the browser.
#[derive(Clone)]
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
#[derive(Clone)]
pub struct BrowserPane {
    level: BrowserLevel,
    link: Link,
    breadcrumbs: Vec<String>,
    /// The full track list, read from the facts once. Every group, genre included,
    /// is built from it: nothing is asked or counted again per group.
    all_tracks: Option<Vec<TrackInfo>>,
    /// Dummy selection state for the Root level (4 items, one per category).
    root_selection: SelectionState,
}

impl BrowserPane {
    pub fn new(link: Link) -> Self {
        let mut root_selection = SelectionState::new(ROOT_ITEMS.len());
        // Start with cursor on Artists (index 1).
        root_selection.list_state.select(Some(1));
        BrowserPane {
            level: BrowserLevel::Root { cursor: 1 },
            link,
            breadcrumbs: vec!["Browser".to_string()],
            all_tracks: None,
            root_selection,
        }
    }

    /// Ensure all_tracks is populated; reads it from the facts if not yet cached.
    ///
    /// Returns `Some(PaneAction::Error(...))` if the load fails, leaving
    /// `all_tracks` as `None` so the next call retries. Returns `None` on success.
    fn ensure_all_tracks(&mut self) -> Option<PaneAction> {
        if self.all_tracks.is_none() {
            match self.link.tracks() {
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

    /// Sort tracks by (disc_number asc, track_number asc) for album views.
    ///
    /// Tracks with no disc_number are treated as disc 1. Tracks with no
    /// track_number sort to the end (represented as `u32::MAX`).
    pub(crate) fn sort_album_tracks(tracks: &mut [TrackInfo]) {
        tracks.sort_by_key(|t| {
            (
                t.disc_number.unwrap_or(1),
                t.track_number.unwrap_or(u32::MAX),
            )
        });
    }

    /// Get tracks for a specific group (filtered by field == group_name).
    ///
    /// When the field is `Album`, tracks are sorted by (disc, track number).
    /// For all other fields, tracks are sorted by title ascending.
    fn tracks_for_group(all: &[TrackInfo], field: BrowseField, group_name: &str) -> Vec<TrackInfo> {
        let mut tracks: Vec<TrackInfo> = all
            .iter()
            .filter(|t| field.extract(t).as_deref() == Some(group_name))
            .cloned()
            .collect();

        if field == BrowseField::Album {
            Self::sort_album_tracks(&mut tracks);
        } else {
            tracks.sort_by(|a, b| {
                BrowseField::Title
                    .extract(a)
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .cmp(
                        &BrowseField::Title
                            .extract(b)
                            .unwrap_or_default()
                            .to_ascii_lowercase(),
                    )
            });
        }
        tracks
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
                // Songs: show all tracks directly, sorted by title ascending.
                let mut tracks = self.all_tracks.clone().unwrap_or_default();
                tracks.sort_by(|a, b| {
                    BrowseField::Title
                        .extract(a)
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .cmp(
                            &BrowseField::Title
                                .extract(b)
                                .unwrap_or_default()
                                .to_ascii_lowercase(),
                        )
                });
                let total = tracks.len();
                self.breadcrumbs.push(label.to_string());
                self.level = BrowserLevel::Tracks {
                    field,
                    group_name: None,
                    selection: SelectionState::new(total),
                    tracks,
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

    /// Drill into multiple selected group entries at once, merging their tracks.
    fn drill_multi_group(&mut self, field: BrowseField, names: Vec<String>) -> Option<PaneAction> {
        if let Some(err) = self.ensure_all_tracks() {
            return Some(err);
        }
        let all = self.all_tracks.as_deref().unwrap_or(&[]);

        // Collect tracks from all selected groups, preserving order, deduplicating by hash.
        let mut seen = std::collections::HashSet::new();
        let mut tracks: Vec<TrackInfo> = Vec::new();
        for name in &names {
            for t in Self::tracks_for_group(all, field, name) {
                if seen.insert(t.content_hash.clone()) {
                    tracks.push(t);
                }
            }
        }

        // Sort by track title ascending.
        tracks.sort_by(|a, b| {
            BrowseField::Title
                .extract(a)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .cmp(
                    &BrowseField::Title
                        .extract(b)
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                )
        });

        let label = format!("{} {}", names.len(), field.display_name());
        let total = tracks.len();
        self.breadcrumbs.push(label.clone());
        self.level = BrowserLevel::Tracks {
            field,
            group_name: Some(label), // non-None so pop_level goes back to Groups
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
            BrowserLevel::Root { cursor } => {
                // Already at root, nothing to do
                self.level = BrowserLevel::Root { cursor };
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
                    let groups = self.build_groups(field);
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
        .effective_selection()
        .into_iter()
        .filter_map(|vis_idx| selection.visible_to_data.get(vis_idx))
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
                for vis_idx in selection.effective_selection() {
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
                let state = RowState::from_flags(is_cursor, false);
                ListItem::new(Span::raw(name)).style(state.style())
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

                let state = RowState::from_flags(is_cursor, is_selected);
                ListItem::new(Span::raw(line)).style(state.style())
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
        let block = Block::default().borders(Borders::NONE);

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
                    KeyCode::Esc | KeyCode::Backspace => {
                        if let Some(err) = self.pop_level() {
                            return err;
                        }
                        PaneAction::Consumed
                    }
                    KeyCode::Char(',') => {
                        selection.clear_selection();
                        PaneAction::Consumed
                    }
                    KeyCode::Enter => {
                        // Collect names for all effectively-selected entries.
                        // Extract into a local Vec first so the borrow on `selection`
                        // and `groups` ends before we call the drill methods on `self`.
                        let selected_names: Vec<String> = {
                            selection
                                .effective_selection()
                                .into_iter()
                                .filter_map(|vis| selection.visible_to_data.get(vis).copied())
                                .filter_map(|data_idx| groups.get(data_idx))
                                .map(|g| g.name.clone())
                                .collect()
                        };

                        if selected_names.is_empty() {
                            return PaneAction::Consumed;
                        }

                        // The Groups level is replaced entirely by the drill methods,
                        // so there is no need to explicitly clear its selection state.
                        if selected_names.len() == 1 {
                            let name = selected_names.into_iter().next().unwrap();
                            if let Some(err) = self.drill_group(field, name) {
                                return err;
                            }
                        } else if let Some(err) = self.drill_multi_group(field, selected_names) {
                            return err;
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
                KeyCode::Esc | KeyCode::Backspace => {
                    if let Some(err) = self.pop_level() {
                        return err;
                    }
                    PaneAction::Consumed
                }
                KeyCode::Char(',') => {
                    selection.clear_selection();
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

    /// The facts were read again: go back to the top, and read the tracks anew on
    /// the next drill, so no group shown is older than the facts.
    fn refresh(&mut self) -> PaneAction {
        self.all_tracks = None;
        self.level = BrowserLevel::Root { cursor: 1 };
        self.root_selection = SelectionState::new(ROOT_ITEMS.len());
        self.root_selection.list_state.select(Some(1));
        self.breadcrumbs = vec!["Browser".to_string()];
        PaneAction::Consumed
    }

    fn display_string(&self, data_idx: usize) -> Option<String> {
        match &self.level {
            BrowserLevel::Root { .. } => ROOT_ITEMS.get(data_idx).map(|s| s.to_string()),
            BrowserLevel::Groups { groups, .. } => groups.get(data_idx).map(|g| g.name.clone()),
            BrowserLevel::Tracks { tracks, .. } => {
                let track = tracks.get(data_idx)?;
                let artist = track.artist.as_deref().unwrap_or("");
                let title = track.title.as_deref().unwrap_or("");
                let album = track.album.as_deref().unwrap_or("");
                Some(format!("{} {} {}", artist, title, album))
            }
        }
    }

    fn clone_box(&self) -> Box<dyn Pane> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{catalog, catalog_with_a_fourth, World};
    use mdma_client::ContentHash;

    fn make_track(
        hash: &str,
        artist: Option<&str>,
        album: Option<&str>,
        title: Option<&str>,
    ) -> TrackInfo {
        TrackInfo {
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: album.map(String::from),
            ..TrackInfo::new(ContentHash::new(hash))
        }
    }

    fn make_track_with_disc_track(hash: &str, disc: Option<u32>, track: Option<u32>) -> TrackInfo {
        TrackInfo {
            track_number: track,
            disc_number: disc,
            ..TrackInfo::new(ContentHash::new(hash))
        }
    }

    #[test]
    fn sort_album_tracks_orders_by_disc_then_track_number() {
        // (disc, track) input order: (2,1), (1,3), (1,1), (None→1, 2)
        let mut tracks = vec![
            make_track_with_disc_track("sha256:d21", Some(2), Some(1)),
            make_track_with_disc_track("sha256:d13", Some(1), Some(3)),
            make_track_with_disc_track("sha256:d11", Some(1), Some(1)),
            make_track_with_disc_track("sha256:dno", None, Some(2)), // no disc → treated as disc 1
        ];

        BrowserPane::sort_album_tracks(&mut tracks);

        let hashes: Vec<&str> = tracks.iter().map(|t| t.content_hash.as_str()).collect();

        // Expected order: disc1/track1, disc1/track2(nodisk), disc1/track3, disc2/track1
        assert_eq!(
            hashes,
            vec!["sha256:d11", "sha256:dno", "sha256:d13", "sha256:d21"]
        );
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

    /// The genre groups, their counts and their tracks all come from the genre each
    /// `TrackInfo` carries. The node is dead here: one `get_facts` or
    /// `get_fact_values` per track would show as an unreachable node.
    #[test]
    fn genre_groups_come_from_the_track_infos_without_asking_the_node() {
        let world = World::new(Some(catalog()));
        let link = world.link();
        let mut pane = BrowserPane::new(link.clone());
        let genres = ROOT_FIELDS
            .iter()
            .position(|&field| field == BrowseField::Genre)
            .unwrap();

        assert!(pane.drill_root(genres).is_none());

        let BrowserLevel::Groups { groups, .. } = &pane.level else {
            panic!("the genres are a list of groups");
        };
        let listed: Vec<(&str, usize)> =
            groups.iter().map(|g| (g.name.as_str(), g.count)).collect();
        assert_eq!(listed, [("New Wave", 1), ("Synthpop", 2)]);
        assert!(pane
            .drill_group(BrowseField::Genre, "Synthpop".into())
            .is_none());
        assert_eq!(pane.item_count(), 2);
        let status = link.status_text().unwrap();
        assert!(!status.contains("unreachable"), "{status}");
    }

    #[test]
    fn refreshing_goes_back_to_the_top_and_reads_the_tracks_anew() {
        let world = World::new(Some(catalog()));
        let mut pane = BrowserPane::new(world.link());
        assert!(pane.drill_root(1).is_none());
        assert_eq!(pane.item_count(), 2, "two artists");

        world.sync(catalog_with_a_fourth());
        pane.link.refold().unwrap();
        pane.refresh();

        assert_eq!(pane.item_count(), ROOT_ITEMS.len());
        assert!(pane.drill_root(0).is_none());
        assert_eq!(pane.item_count(), 4, "all four tracks");
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
                    for vis_idx in selection.effective_selection() {
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
