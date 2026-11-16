use crate::pane::{AddPlayingTarget, Pane, PaneAction, PaneKind};
use crate::rows::{rows_for_hashes, ListRow};
use crate::selection::SelectionState;
use crate::theme::MUTED;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{
    ContentHash, LibraryBackend, LibraryClientError, PlaybackBackend, PlaylistName, SourceName,
    TrackInfo,
};
use ratatui::{
    layout::{Alignment, Rect},
    style::Style,
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::rc::Rc;

/// What to tell the user after asking playback to remove tracks: what playback
/// reports it removed, which can be fewer than were asked for (or none).
fn removed_message(removed: usize) -> String {
    match removed {
        0 => "Nothing removed from queue".to_string(),
        n => format!("Removed {n} track(s) from queue"),
    }
}

/// Build the queue's rows from the answer to listing it: one row per queue
/// entry, in order. An entry the library has no track for stays as an
/// unavailable row, so it can still be removed. A failed listing or any
/// other failed lookup fails the load with its message.
fn rows_for_queue<E: std::fmt::Display>(
    queue: Result<Vec<ContentHash>, E>,
    get_track: impl FnMut(&ContentHash) -> Result<TrackInfo, LibraryClientError>,
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

/// Minimal proof-of-life pane that shows the current playback queue.
#[allow(dead_code)]
#[derive(Clone)]
pub struct QueuePane {
    rows: Vec<ListRow>,
    /// Why the queue could not be loaded; shown instead of the (then unknown)
    /// queue contents.
    load_error: Option<String>,
    selection: SelectionState,
    title: String,
    playback: Rc<PlaybackBackend>,
    library: Rc<LibraryBackend>,
}

impl QueuePane {
    /// Create a new QueuePane, loading the queue from the playback backend
    /// and resolving each hash against the library.
    ///
    /// If the queue cannot be loaded the pane shows why instead of a queue.
    pub fn new(playback: Rc<PlaybackBackend>, library: Rc<LibraryBackend>) -> Self {
        let mut pane = QueuePane {
            rows: Vec::new(),
            load_error: None,
            selection: SelectionState::new(0),
            title: "Queue".to_string(),
            playback,
            library,
        };
        pane.reload();
        pane
    }

    /// Load queue hashes from playback then resolve each via library.
    fn load_queue(
        playback: &PlaybackBackend,
        library: &LibraryBackend,
    ) -> Result<Vec<ListRow>, String> {
        rows_for_queue(playback.queue_list(), |h| library.get_track(h))
    }

    /// Replace the rows with the loaded queue. A failed load leaves no rows,
    /// so nothing stale can be acted on, and is returned as an error.
    fn reload(&mut self) -> PaneAction {
        let loaded = Self::load_queue(&self.playback, &self.library);
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
                let resolved = match self
                    .playback
                    .queue_list()
                    .map_err(|e| e.to_string())
                    .and_then(|queue| {
                        mdma_client::resolve_in_queue(&queue, &hashes).map_err(|e| e.to_string())
                    }) {
                    Ok(resolved) => resolved,
                    Err(e) => return PaneAction::Error(format!("Remove failed: {e}")),
                };
                match self
                    .playback
                    .queue_remove(resolved.into_iter().map(ContentHash::from).collect())
                {
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
        let resolved = match self.library.resolve_for_playback(hashes) {
            Ok(resolved) => resolved,
            Err(e) => return PaneAction::Error(e.to_string()),
        };
        let mut errors = Vec::new();
        for hash in &resolved {
            if let Err(e) = self.playback.queue_append(hash.into(), SourceName::audio()) {
                errors.push(format!("{e}"));
            }
        }
        if errors.is_empty() {
            self.refresh();
            PaneAction::Info(format!("Queued {} track(s)", hashes.len()))
        } else {
            PaneAction::Error(errors.join(", "))
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

    #[test]
    fn removed_message_reports_what_was_removed() {
        assert_eq!(removed_message(3), "Removed 3 track(s) from queue");
    }

    #[test]
    fn removed_message_says_so_when_nothing_was_removed() {
        assert_eq!(removed_message(0), "Nothing removed from queue");
    }
}
