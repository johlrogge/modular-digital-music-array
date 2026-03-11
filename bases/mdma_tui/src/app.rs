use crate::now_playing::NowPlaying;
use crate::pane::Pane;

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
    /// Entering a command (`:` prefix).
    Command,
    /// Typing a filter string (`s` prefix).
    FilterInput,
}

/// Application state.
pub struct App {
    pub left_pane: Box<dyn Pane>,
    pub right_pane: Box<dyn Pane>,
    pub active_side: Side,
    pub mode: InputMode,
    pub now_playing: NowPlaying,
    pub status_message: Option<String>,
    pub command_input: String,
    pub filter_input: String,
    pub should_quit: bool,
}

impl App {
    pub fn new(left_pane: Box<dyn Pane>, right_pane: Box<dyn Pane>) -> Self {
        Self {
            left_pane,
            right_pane,
            active_side: Side::Left,
            mode: InputMode::Normal,
            now_playing: NowPlaying::new(),
            status_message: None,
            command_input: String::new(),
            filter_input: String::new(),
            should_quit: false,
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
}
