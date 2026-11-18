use crate::link::{Link, ReadError};
use crate::pane::{AddPlayingTarget, Pane, PaneAction, PaneKind};
use crate::rows::{rows_for_hashes, ListRow};
use crate::selection::SelectionState;
use crate::theme::MUTED;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, PlaylistName, SourceName, TrackInfo};
use ratatui::{
    layout::{Alignment, Rect},
    style::Style,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// What to tell the user after asking playback to remove tracks: what playback
/// reports it removed, which can be fewer than were asked for (or none).
fn removed_message(removed: usize) -> String {
    match removed {
        0 => "Nothing removed from queue".to_string(),
        n => format!("Removed {n} track(s) from queue"),
    }
}

/// Build the queue's rows from the answer to listing it: one row per queue
/// entry, in order. An entry the facts have no track for stays as an
/// unavailable row, so it can still be removed. A failed listing or any
/// other failed lookup fails the load with its message.
fn rows_for_queue<E: std::fmt::Display>(
    queue: Result<Vec<ContentHash>, E>,
    get_track: impl FnMut(&ContentHash) -> Result<TrackInfo, ReadError>,
) -> Result<Vec<ListRow>, String> {
    let queue = queue.map_err(|e| format!("Could not list the queue: {e}"))?;
    rows_for_hashes(&queue, get_track).map_err(|e| format!("Could not load the queue: {e}"))
}

/// The hash of each row the selection covers, in selection order.
fn selected_hashes(rows: &[ListRow], selection: &SelectionState) -> Vec<ContentHash> {
    selection
        .effective_selection()
        .into_iter()
        .filter_map(|vis_idx| selection.visible_index_to_data(vis_idx))
        .map(|data_idx| rows[data_idx].hash())
        .collect()
}

/// The playback queue. The queue comes from the node, and each row's metadata
/// from the facts, so a node that cannot be reached leaves the pane showing why.
#[allow(dead_code)]
#[derive(Clone)]
pub struct QueuePane {
    rows: Vec<ListRow>,
    /// Why the queue could not be loaded; shown instead of the (then unknown)
    /// queue contents.
    load_error: Option<String>,
    selection: SelectionState,
    title: String,
    link: Link,
}

impl QueuePane {
    /// Create a new QueuePane, loading the queue from the node and resolving
    /// each hash against the facts.
    ///
    /// If the queue cannot be loaded the pane shows why instead of a queue.
    pub fn new(link: Link) -> Self {
        let mut pane = QueuePane {
            rows: Vec::new(),
            load_error: None,
            selection: SelectionState::new(0),
            title: "Queue".to_string(),
            link,
        };
        pane.reload();
        pane
    }

    /// Load queue hashes from playback then resolve each via the facts.
    fn load_queue(link: &Link) -> Result<Vec<ListRow>, String> {
        let queue = link
            .playback()
            .map_err(|e| e.to_string())
            .and_then(|playback| playback.queue_list().map_err(|e| e.to_string()));
        // The metadata of a row is the node's, not the replica's: the rows are what
        // the node's queue holds, and what `d` removes.
        rows_for_queue(queue, |h| link.node_track(h))
    }

    /// Replace the rows with the loaded queue. A failed load leaves no rows,
    /// so nothing stale can be acted on, and is returned as an error.
    fn reload(&mut self) -> PaneAction {
        let loaded = Self::load_queue(&self.link);
        let action = match &loaded {
            Ok(_) => PaneAction::Consumed,
            Err(e) => PaneAction::Error(e.clone()),
        };
        self.load_error = loaded.as_ref().err().cloned();
        self.rows = loaded.unwrap_or_default();
        self.selection.set_total_items(self.rows.len());
        action
    }
}

impl Pane for QueuePane {
    fn render(&self, f: &mut Frame, area: Rect) {
        if let Some(error) = &self.load_error {
            let placeholder = Paragraph::new(error.as_str())
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center);
            f.render_widget(placeholder, area);
            return;
        }

        if self.rows.is_empty() {
            let placeholder = Paragraph::new("queue is empty")
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center);
            f.render_widget(placeholder, area);
            return;
        }

        let block = Block::default().borders(Borders::NONE);
        render_track_list(f, area, &self.rows, &self.selection, block);
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
            KeyCode::Char('d') => {
                let hashes = self.resolve_selection();
                if hashes.is_empty() {
                    return PaneAction::Consumed;
                }
                // Resolved against the queue's own entries, so one whose track
                // has left the library can still be removed.
                let removed =
                    self.link
                        .playback()
                        .map_err(|e| e.to_string())
                        .and_then(|playback| {
                            let queue = playback.queue_list().map_err(|e| e.to_string())?;
                            let resolved = mdma_client::resolve_in_queue(&queue, &hashes)
                                .map_err(|e| e.to_string())?;
                            playback
                                .queue_remove(resolved.into_iter().map(ContentHash::from).collect())
                                .map_err(|e| e.to_string())
                        });
                match removed {
                    Ok(removed) => {
                        self.refresh();
                        PaneAction::Info(removed_message(removed))
                    }
                    Err(e) => PaneAction::Error(format!("Remove failed: {e}")),
                }
            }
            KeyCode::Esc => {
                if !self.selection.pop_filter() {
                    self.selection.clear_selection();
                }
                PaneAction::Consumed
            }
            KeyCode::Char(',') => {
                self.selection.clear_selection();
                PaneAction::Consumed
            }
            _ => PaneAction::Ignored,
        }
    }

    fn resolve_selection(&self) -> Vec<ContentHash> {
        selected_hashes(&self.rows, &self.selection)
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
        self.rows.len()
    }

    fn pane_kind(&self) -> PaneKind {
        PaneKind::Queue
    }

    fn playlist_name(&self) -> Option<&PlaylistName> {
        None
    }

    fn accept_tracks(&mut self, hashes: &[ContentHash]) -> PaneAction {
        let (library, playback) = match self
            .link
            .library()
            .and_then(|library| Ok((library, self.link.playback()?)))
        {
            Ok(both) => both,
            Err(e) => return PaneAction::Error(e.to_string()),
        };
        let resolved = match library.resolve_for_playback(hashes) {
            Ok(resolved) => resolved,
            Err(e) => return PaneAction::Error(e.to_string()),
        };
        // Stops at the first error: a node that stopped answering would otherwise be
        // asked, and wait for, once per track.
        let queued = resolved
            .iter()
            .try_for_each(|hash| playback.queue_append(hash.into(), SourceName::audio()));
        // Some may be queued before the error, so the rows are read again either way.
        self.refresh();
        match queued {
            Ok(()) => PaneAction::Info(format!("Queued {} track(s)", hashes.len())),
            Err(e) => PaneAction::Error(e.to_string()),
        }
    }

    fn refresh(&mut self) -> PaneAction {
        self.reload()
    }

    fn display_string(&self, data_idx: usize) -> Option<String> {
        Some(self.rows.get(data_idx)?.display_text())
    }

    fn add_playing_target(&self) -> AddPlayingTarget {
        AddPlayingTarget::Queue
    }

    fn clone_box(&self) -> Box<dyn Pane> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{make_track_info, not_found, transport_error};
    use crate::test_support::{catalog, World, ATOMIC};

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    /// A queue entry the library has no track for stays in its position as
    /// an unavailable row, and selecting it yields its queue hash so `d` can
    /// remove it.
    #[test]
    fn a_queue_entry_the_library_lacks_stays_selectable_by_its_hash() {
        let queue = vec![hash("sha256:aaa"), hash("sha256:gone"), hash("sha256:bbb")];

        let rows = rows_for_queue(Ok::<_, String>(queue.clone()), |h| {
            if h.as_str() == "sha256:gone" {
                Err(not_found(h))
            } else {
                Ok(make_track_info(h.as_str(), "A", "T", 60))
            }
        })
        .unwrap();

        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[1], ListRow::Unresolved(_)));

        let mut selection = SelectionState::new(rows.len());
        selection.list_state.select(Some(1));
        assert_eq!(
            selected_hashes(&rows, &selection),
            vec![hash("sha256:gone")]
        );
        assert_eq!(
            rows.iter().map(ListRow::hash).collect::<Vec<_>>(),
            queue,
            "rows keep the queue's order"
        );
    }

    /// A failed queue listing is an error, not an empty queue.
    #[test]
    fn a_failed_queue_listing_is_an_error_not_an_empty_queue() {
        let result = rows_for_queue(Err("playback unreachable"), |_| unreachable!());

        let message = result.err().expect("a failed listing must be an error");
        assert!(message.contains("playback unreachable"), "{message}");
    }

    /// A failed lookup other than "not found" fails the load rather than
    /// hiding the entry.
    #[test]
    fn a_failed_track_lookup_is_an_error_not_a_missing_row() {
        let result = rows_for_queue(Ok::<_, String>(vec![hash("sha256:aaa")]), |_| {
            Err(transport_error())
        });

        assert!(result.is_err());
    }

    /// Text of every cell of `pane` rendered into a wide, short terminal.
    fn rendered(pane: &QueuePane) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(240, 3)).unwrap();
        terminal.draw(|f| pane.render(f, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// With the node dead the queue cannot be listed: the pane says why and has no
    /// rows, and nothing can be selected or removed from it.
    #[test]
    fn a_dead_node_shows_the_unreachable_text_and_no_rows() {
        let world = World::new(Some(catalog()));

        let pane = QueuePane::new(world.link());

        assert_eq!(pane.item_count(), 0);
        assert!(pane.resolve_selection().is_empty());
        let shown = rendered(&pane);
        assert!(shown.contains("Could not list the queue: "), "{shown}");
        assert!(shown.contains("needs the node"), "{shown}");
        assert!(!shown.contains("queue is empty"), "{shown}");
    }

    /// The rows are the node's own: with no replica at all, and the node up, the
    /// queue loads.
    #[test]
    fn with_no_replica_and_the_node_up_the_queue_loads_its_rows() {
        use crate::test_support::{node_hash, Services};
        let world = World::new(None);
        let _services = Services::start(&world);

        let pane = QueuePane::new(world.link());

        assert!(pane.load_error.is_none(), "{:?}", pane.load_error);
        assert_eq!(pane.item_count(), 1);
        assert_eq!(pane.resolve_selection(), [node_hash()]);
        assert!(rendered(&pane).contains("Node Title"));
    }

    #[test]
    fn with_no_node_selected_the_pane_says_how_to_select_one() {
        let world = World::new(Some(catalog()));
        let link = Link::new(world.cancelled_node(), client_facts::FactsChoice::Replica);

        let pane = QueuePane::new(link);

        assert_eq!(pane.item_count(), 0);
        let shown = rendered(&pane);
        assert!(
            shown.contains("no node selected; run `mdma node select`"),
            "{shown}"
        );
    }

    #[test]
    fn queueing_on_a_dead_node_is_an_error_not_a_change() {
        let world = World::new(Some(catalog()));
        let mut pane = QueuePane::new(world.link());

        let action = pane.accept_tracks(&[hash(ATOMIC)]);

        let PaneAction::Error(text) = action else {
            panic!("a dead node cannot queue");
        };
        assert!(text.contains("needs the node"), "{text}");
        assert_eq!(pane.item_count(), 0);
    }

    #[test]
    fn removed_message_reports_what_was_removed() {
        assert_eq!(removed_message(3), "Removed 3 track(s) from queue");
    }

    #[test]
    fn removed_message_says_so_when_nothing_was_removed() {
        assert_eq!(removed_message(0), "Nothing removed from queue");
    }
}
