use crate::browser_pane::BrowserPane;
use crate::commands::{matching, Command};
use crate::error::TuiError;
use crate::now_playing::NowPlaying;
use crate::pane::Pane;
use crate::playlists_pane::PlaylistsPane;
use crate::queue_pane::QueuePane;
use crate::search_pane::SearchPane;
use mdma_client::{LibraryBackend, PlaybackBackend};
use std::rc::Rc;

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
    /// Space mode picker overlay — shows available modes.
    SpaceMenu,
    /// Playback control mode; single-key bindings for transport controls.
    Playback,
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
    pub should_quit: bool,
    /// Shared library backend, used for opening new panes (e.g. PlaylistPane).
    pub library: Rc<LibraryBackend>,
    /// Shared playback backend, used for command palette execution.
    pub playback: Rc<PlaybackBackend>,
    // --- Palette state ---
    pub palette_query: String,
    pub palette_matches: Vec<&'static Command>,
    pub palette_cursor: usize,
}

impl App {
    pub fn new(
        left_pane: Box<dyn Pane>,
        right_pane: Box<dyn Pane>,
        library: Rc<LibraryBackend>,
        playback: Rc<PlaybackBackend>,
    ) -> Self {
        Self {
            left_pane,
            right_pane,
            active_side: Side::Left,
            mode: InputMode::Normal,
            now_playing: NowPlaying::new(),
            status_message: None,
            filter_input: String::new(),
            should_quit: false,
            library,
            playback,
            palette_query: String::new(),
            palette_matches: Vec::new(),
            palette_cursor: 0,
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
        self.palette_matches = matching("");
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
        self.palette_matches = matching(&query);
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
    pub fn make_playlists_pane(&self) -> Result<Box<dyn Pane>, TuiError> {
        let pane = PlaylistsPane::new(Rc::clone(&self.library))?;
        Ok(Box::new(pane))
    }
}
