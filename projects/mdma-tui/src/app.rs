use crate::browser_pane::BrowserPane;
use crate::commands::{matching, Command};
use crate::events::AppEvent;
use crate::now_playing::NowPlaying;
use crate::pane::{Pane, PaneKind};
use crate::playlists_pane::PlaylistsPane;
use crate::queue_pane::QueuePane;
use crate::search_pane::SearchPane;
use event_protocol::PlaybackEvent;
use mdma_client::{LibraryBackend, PlaybackBackend, PlaylistName};
use std::rc::Rc;
use std::sync::mpsc::Receiver;

/// An entry in the command palette — either a built-in command or a playlist open/create action.
#[derive(Clone)]
pub enum PaletteEntry {
    Command(&'static Command),
    OpenPlaylist(PlaylistName),
    CreatePlaylist(String),
    /// Open a history search. The string is the raw argument from `:history [arg]`.
    History(String),
}

/// Which side of the split layout is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Current input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Normal navigation and pane interaction.
    Normal,
    /// Command palette open (`:` prefix).
    Palette,
    /// Typing a filter string (`s` prefix).
    FilterInput,
    /// Help overlay visible; any key returns to Normal.
    Help,
    /// Playback control mode; single-key bindings for transport controls.
    Playback,
    /// Typing a new playlist name.
    NameInput,
}

/// Application state.
pub struct App {
    pub left_pane: Box<dyn Pane>,
    pub right_pane: Box<dyn Pane>,
    pub active_side: Side,
    pub mode: InputMode,
    pub now_playing: NowPlaying,
    pub status_message: Option<String>,
    pub filter_input: String,
    /// Tracks whether a live (in-progress) filter has been pushed onto the active
    /// pane's filter stack. Used by `apply_live_filter` to decide between
    /// `push_filter` (first keystroke) and `replace_top_filter` (subsequent).
    pub live_filter_active: bool,
    pub name_input: String,
    pub should_quit: bool,
    /// Shared library backend, used for opening new panes (e.g. PlaylistPane).
    pub library: Rc<LibraryBackend>,
    /// Shared playback backend, used for command palette execution.
    pub playback: Rc<PlaybackBackend>,
    // --- Palette state ---
    pub palette_query: String,
    pub palette_matches: Vec<PaletteEntry>,
    pub palette_cursor: usize,
    /// Background event receiver (NNG playback events from the subscriber thread).
    /// `None` when no event subscription was established (e.g. no --node flag).
    pub event_rx: Option<Receiver<AppEvent>>,
}

impl App {
    pub fn new(
        left_pane: Box<dyn Pane>,
        right_pane: Box<dyn Pane>,
        library: Rc<LibraryBackend>,
        playback: Rc<PlaybackBackend>,
        event_rx: Option<Receiver<AppEvent>>,
    ) -> Self {
        Self {
            left_pane,
            right_pane,
            active_side: Side::Left,
            mode: InputMode::Normal,
            now_playing: NowPlaying::new(),
            status_message: None,
            filter_input: String::new(),
            live_filter_active: false,
            name_input: String::new(),
            should_quit: false,
            library,
            playback,
            palette_query: String::new(),
            palette_matches: Vec::new(),
            palette_cursor: 0,
            event_rx,
        }
    }

    /// Drain the background event channel and apply any pending events to app state.
    ///
    /// Called once per tick from the `TuiApp::on_tick` implementation.
    ///
    /// Events are collected into a `Vec` first so the borrow on `self.event_rx` is
    /// released before we mutate other fields (panes, now_playing, status).
    pub fn drain_events(&mut self) {
        let events: Vec<AppEvent> = match self.event_rx.as_ref() {
            Some(rx) => std::iter::from_fn(|| rx.try_recv().ok()).collect(),
            None => return,
        };

        let library = Rc::clone(&self.library);
        for ev in events {
            match ev {
                AppEvent::Playback(pe) => {
                    // Refresh queue panes when the queue changes on the node.
                    if matches!(pe, PlaybackEvent::QueueChanged { .. }) {
                        if self.left_pane.pane_kind() == PaneKind::Queue {
                            self.left_pane.refresh();
                        }
                        if self.right_pane.pane_kind() == PaneKind::Queue {
                            self.right_pane.refresh();
                        }
                    }
                    // Resolve track metadata when a new track starts.
                    if let PlaybackEvent::TrackStarted { hash } = &pe {
                        let meta = library.get_track(hash);
                        let (title, artist) = match meta {
                            Ok(t) => (t.title, t.artist),
                            Err(_) => (None, None),
                        };
                        self.now_playing.set_track_metadata(title, artist);
                    }
                    self.now_playing.apply(&pe);
                }
                AppEvent::SubscriberError(msg) => {
                    self.set_status(format!("Event error: {msg}"));
                }
            }
        }
    }

    /// Immutable reference to the currently active pane.
    #[allow(dead_code)]
    pub fn active_pane(&self) -> &dyn Pane {
        match self.active_side {
            Side::Left => self.left_pane.as_ref(),
            Side::Right => self.right_pane.as_ref(),
        }
    }

    /// Mutable reference to the currently active pane box.
    pub fn active_pane_mut(&mut self) -> &mut Box<dyn Pane> {
        match self.active_side {
            Side::Left => &mut self.left_pane,
            Side::Right => &mut self.right_pane,
        }
    }

    /// Immutable reference to the inactive pane.
    #[allow(dead_code)]
    pub fn inactive_pane(&self) -> &dyn Pane {
        match self.active_side {
            Side::Left => self.right_pane.as_ref(),
            Side::Right => self.left_pane.as_ref(),
        }
    }

    /// Mutable reference to the inactive pane box.
    #[allow(dead_code)]
    pub fn inactive_pane_mut(&mut self) -> &mut Box<dyn Pane> {
        match self.active_side {
            Side::Left => &mut self.right_pane,
            Side::Right => &mut self.left_pane,
        }
    }

    /// Toggle focus between left and right panes.
    pub fn toggle_active(&mut self) {
        self.active_side = match self.active_side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
    }

    /// Set a status bar message.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    /// Clear the status bar message.
    #[allow(dead_code)]
    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    /// Open the command palette, resetting query and computing initial matches.
    pub fn open_palette(&mut self) {
        self.palette_query.clear();
        self.palette_matches = matching("")
            .into_iter()
            .map(PaletteEntry::Command)
            .collect();
        self.palette_cursor = 0;
        self.mode = InputMode::Palette;
    }

    /// Close the command palette and return to normal mode.
    pub fn close_palette(&mut self) {
        self.palette_query.clear();
        self.palette_matches.clear();
        self.palette_cursor = 0;
        self.mode = InputMode::Normal;
    }

    /// Update the palette query and recompute matches, clamping the cursor.
    pub fn palette_update_query(&mut self, query: String) {
        if let Some(arg) = query.strip_prefix("o ") {
            // Open/create mode: fetch playlists and filter by arg prefix
            let playlists = self.library.playlist_list().unwrap_or_default();
            let lower = arg.to_lowercase();
            let mut entries: Vec<PaletteEntry> = playlists
                .into_iter()
                .filter(|p| p.as_str().to_lowercase().starts_with(lower.as_str()))
                .map(PaletteEntry::OpenPlaylist)
                .collect();
            let has_exact = entries.iter().any(|e| {
                matches!(e, PaletteEntry::OpenPlaylist(p) if p.as_str().eq_ignore_ascii_case(arg))
            });
            if !arg.is_empty() && !has_exact {
                entries.push(PaletteEntry::CreatePlaylist(arg.to_string()));
            }
            self.palette_matches = entries;
        } else if let Some(arg) = query.strip_prefix("history ") {
            // History mode: show a single entry so the user can confirm with Enter.
            // Argument validation (and error display) happens on Enter.
            self.palette_matches = vec![PaletteEntry::History(arg.trim().to_string())];
        } else {
            self.palette_matches = matching(&query)
                .into_iter()
                .map(PaletteEntry::Command)
                .collect();
        }
        self.palette_cursor = self
            .palette_cursor
            .min(self.palette_matches.len().saturating_sub(1));
        self.palette_query = query;
    }

    /// Replace the currently active pane with `new_pane`.
    pub fn switch_active_pane(&mut self, new_pane: Box<dyn Pane>) {
        if self.active_side == Side::Left {
            self.left_pane = new_pane;
        } else {
            self.right_pane = new_pane;
        }
    }

    /// Construct a fresh SearchPane backed by this app's library.
    pub fn make_search_pane(&self) -> Box<dyn Pane> {
        Box::new(SearchPane::new(Rc::clone(&self.library)))
    }

    /// Construct a SearchPane with a pre-filled query and immediately run the search.
    ///
    /// Returns the pane and any `PaneAction` from the initial search (e.g. an error
    /// message if the library backend could not be reached).
    pub fn make_search_pane_with_query(
        &self,
        query: String,
    ) -> (Box<dyn Pane>, crate::pane::PaneAction) {
        let (pane, action) = SearchPane::with_query(Rc::clone(&self.library), query);
        (Box::new(pane), action)
    }

    /// Construct a fresh BrowserPane backed by this app's library.
    pub fn make_browser_pane(&self) -> Box<dyn Pane> {
        Box::new(BrowserPane::new(Rc::clone(&self.library)))
    }

    /// Construct a fresh QueuePane backed by this app's playback and library backends.
    pub fn make_queue_pane(&self) -> Box<dyn Pane> {
        Box::new(QueuePane::new(
            Rc::clone(&self.playback),
            Rc::clone(&self.library),
        ))
    }

    /// Construct a fresh PlaylistsPane backed by this app's library.
    ///
    /// Returns `Err` if the library backend cannot be reached.
    pub fn make_playlists_pane(&self) -> color_eyre::Result<Box<dyn Pane>> {
        let pane = PlaylistsPane::new(Rc::clone(&self.library))?;
        Ok(Box::new(pane))
    }
}

impl tui_base::TuiApp for App {
    type Error = color_eyre::Report;

    fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        crate::input::handle_key(self, key);
    }

    fn on_tick(&mut self) {
        self.drain_events();
    }

    fn render(&self, frame: &mut ratatui::Frame) {
        crate::ui::render(frame, self);
    }

    fn should_quit(&self) -> bool {
        self.should_quit
    }
}
