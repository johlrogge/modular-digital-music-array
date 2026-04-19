use crate::pane::{AddPlayingTarget, Pane, PaneAction, PaneKind};
use crate::selection::SelectionState;
use crate::theme::TEXT_TERTIARY;
use crate::track_list::render_track_list;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{
    ContentHash, LibraryBackend, PlaybackBackend, PlaylistName, SourceName, TrackInfo,
};
use ratatui::{
    layout::{Alignment, Rect},
    style::Style,
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::rc::Rc;

/// Minimal proof-of-life pane that shows the current playback queue.
#[allow(dead_code)]
#[derive(Clone)]
pub struct QueuePane {
    tracks: Vec<TrackInfo>,
    selection: SelectionState,
    title: String,
    playback: Rc<PlaybackBackend>,
    library: Rc<LibraryBackend>,
}

impl QueuePane {
    /// Create a new QueuePane, loading the queue from the playback backend
    /// and resolving each hash against the library.
    ///
    /// If either backend is unavailable or returns an error, the pane starts
    /// empty with a status placeholder.
    pub fn new(playback: Rc<PlaybackBackend>, library: Rc<LibraryBackend>) -> Self {
        let tracks = Self::load_queue(&playback, &library);
        let total = tracks.len();
        QueuePane {
            tracks,
            selection: SelectionState::new(total),
            title: "Queue".to_string(),
            playback,
            library,
        }
    }

    /// Load queue hashes from playback then resolve each via library.
    fn load_queue(playback: &PlaybackBackend, library: &LibraryBackend) -> Vec<TrackInfo> {
        let hashes = match playback.queue_list() {
            Ok(h) => h,
            Err(_) => return Vec::new(),
        };
        hashes
            .iter()
            .filter_map(|hash| library.get_track(hash).ok())
            .collect()
    }
}

impl Pane for QueuePane {
    fn render(&self, f: &mut Frame, area: Rect) {
        if self.tracks.is_empty() {
            let placeholder = Paragraph::new("queue is empty")
                .style(Style::default().fg(TEXT_TERTIARY))
                .alignment(Alignment::Center);
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
            KeyCode::Char('d') => {
                let hashes = self.resolve_selection();
                if hashes.is_empty() {
                    return PaneAction::Consumed;
                }
                let count = hashes.len();
                match self.playback.queue_remove(hashes) {
                    Ok(_) => {
                        self.refresh();
                        PaneAction::Info(format!("Removed {} track(s) from queue", count))
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

    fn title(&self) -> &str {
        &self.title
    }

    fn item_count(&self) -> usize {
        self.tracks.len()
    }

    fn pane_kind(&self) -> PaneKind {
        PaneKind::Queue
    }

    fn playlist_name(&self) -> Option<&PlaylistName> {
        None
    }

    fn accept_tracks(&mut self, hashes: &[ContentHash]) -> PaneAction {
        let mut errors = Vec::new();
        for hash in hashes {
            if let Err(e) = self
                .playback
                .queue_append(hash.clone(), SourceName::audio())
            {
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
        self.tracks = Self::load_queue(&self.playback, &self.library);
        self.selection.set_total_items(self.tracks.len());
        PaneAction::Consumed
    }

    fn display_string(&self, data_idx: usize) -> Option<String> {
        let track = self.tracks.get(data_idx)?;
        let artist = track.artist.as_deref().unwrap_or("");
        let title = track.title.as_deref().unwrap_or("");
        let album = track.album.as_deref().unwrap_or("");
        Some(format!("{} {} {}", artist, title, album))
    }

    fn add_playing_target(&self) -> AddPlayingTarget {
        AddPlayingTarget::Queue
    }

    fn clone_box(&self) -> Box<dyn Pane> {
        Box::new(self.clone())
    }
}
