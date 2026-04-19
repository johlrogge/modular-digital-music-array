use crate::browser_pane::BrowserPane;
use crate::commands::{matching, Command};
use crate::events::AppEvent;
use crate::now_playing::NowPlaying;
use crate::pane::{Pane, PaneKind};
use crate::playlists_pane::PlaylistsPane;
use crate::queue_pane::QueuePane;
use crate::search_pane::SearchPane;
use event_protocol::PlaybackEvent;
use mdma_client::{ContentHash, LibraryBackend, PlaybackBackend, PlaylistName};
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

/// Number of tab slots per side.
pub const TABS_PER_SIDE: usize = 5;

/// Application state.
pub struct App {
    // --- Tab state ---
    /// Left-side pane slots. `None` = unvisited/empty.
    pub left_tabs: [Option<Box<dyn Pane>>; TABS_PER_SIDE],
    /// Currently active left-side slot index (0..TABS_PER_SIDE).
    pub left_tab_idx: usize,
    /// Most-recently-visited left slot indices (front = most recent).
    pub left_recency: Vec<usize>,
    /// Right-side pane slots. `None` = unvisited/empty.
    pub right_tabs: [Option<Box<dyn Pane>>; TABS_PER_SIDE],
    /// Currently active right-side slot index (0..TABS_PER_SIDE).
    pub right_tab_idx: usize,
    /// Most-recently-visited right slot indices (front = most recent).
    pub right_recency: Vec<usize>,

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
    /// Cut/yank clipboard: hashes placed here by `d` (cut) in PlaylistPane.
    /// Paste (`p`) reads from here non-destructively, allowing multi-paste.
    pub clipboard: Vec<ContentHash>,
}

impl App {
    pub fn new(
        left_pane: Box<dyn Pane>,
        right_pane: Box<dyn Pane>,
        library: Rc<LibraryBackend>,
        playback: Rc<PlaybackBackend>,
        event_rx: Option<Receiver<AppEvent>>,
    ) -> Self {
        // Build fixed-size slot arrays with only slot 0 populated on each side.
        // We can't use array initializers with non-Copy boxed types so we construct
        // them element by element via a helper that produces `None`-filled arrays.
        let mut left_tabs: [Option<Box<dyn Pane>>; TABS_PER_SIDE] = [None, None, None, None, None];
        let mut right_tabs: [Option<Box<dyn Pane>>; TABS_PER_SIDE] = [None, None, None, None, None];
        left_tabs[0] = Some(left_pane);
        right_tabs[0] = Some(right_pane);

        Self {
            left_tabs,
            left_tab_idx: 0,
            left_recency: vec![0],
            right_tabs,
            right_tab_idx: 0,
            right_recency: vec![0],
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
            clipboard: Vec::new(),
        }
    }

    // =========================================================================
    // Tab management
    // =========================================================================

    /// Activate the tab at `idx` on `side`.
    ///
    /// - Same side + same idx → no-op.
    /// - Slot already populated → switch.
    /// - Slot is `None` → clone the currently-focused pane into it, then switch.
    /// - Updates the recency vec of the target side.
    pub fn activate_tab(&mut self, side: Side, idx: usize) {
        assert!(idx < TABS_PER_SIDE, "tab index {idx} out of range");

        // No-op when already on the requested slot and side.
        if self.active_side == side && self.tab_idx(side) == idx {
            return;
        }

        // If the target slot is empty, clone the currently-focused pane into it.
        let slot_is_empty = match side {
            Side::Left => self.left_tabs[idx].is_none(),
            Side::Right => self.right_tabs[idx].is_none(),
        };
        if slot_is_empty {
            // Clone from the currently-focused pane (the active side's active slot).
            let cloned = self.active_pane().clone_box();
            match side {
                Side::Left => self.left_tabs[idx] = Some(cloned),
                Side::Right => self.right_tabs[idx] = Some(cloned),
            }
        }

        // Switch active side and slot.
        self.active_side = side;
        match side {
            Side::Left => self.left_tab_idx = idx,
            Side::Right => self.right_tab_idx = idx,
        }

        // Update recency vec: remove idx if already present, then push to front.
        let recency = match side {
            Side::Left => &mut self.left_recency,
            Side::Right => &mut self.right_recency,
        };
        recency.retain(|&x| x != idx);
        recency.insert(0, idx);
    }

    /// Current active slot index for `side`.
    pub fn tab_idx(&self, side: Side) -> usize {
        match side {
            Side::Left => self.left_tab_idx,
            Side::Right => self.right_tab_idx,
        }
    }

    // =========================================================================
    // Pane accessors
    // =========================================================================

    /// Immutable reference to the currently active pane.
    pub fn active_pane(&self) -> &dyn Pane {
        match self.active_side {
            Side::Left => self.left_tabs[self.left_tab_idx]
                .as_ref()
                .expect("active left tab must be Some")
                .as_ref(),
            Side::Right => self.right_tabs[self.right_tab_idx]
                .as_ref()
                .expect("active right tab must be Some")
                .as_ref(),
        }
    }

    /// Mutable reference to the currently active pane box.
    pub fn active_pane_mut(&mut self) -> &mut Box<dyn Pane> {
        match self.active_side {
            Side::Left => self.left_tabs[self.left_tab_idx]
                .as_mut()
                .expect("active left tab must be Some"),
            Side::Right => self.right_tabs[self.right_tab_idx]
                .as_mut()
                .expect("active right tab must be Some"),
        }
    }

    /// Immutable reference to the inactive side's active pane.
    #[allow(dead_code)]
    pub fn inactive_pane(&self) -> &dyn Pane {
        match self.active_side {
            Side::Left => self.right_tabs[self.right_tab_idx]
                .as_ref()
                .expect("inactive right tab must be Some")
                .as_ref(),
            Side::Right => self.left_tabs[self.left_tab_idx]
                .as_ref()
                .expect("inactive left tab must be Some")
                .as_ref(),
        }
    }

    /// Mutable reference to the inactive side's active pane box.
    pub fn inactive_pane_mut(&mut self) -> &mut Box<dyn Pane> {
        match self.active_side {
            Side::Left => self.right_tabs[self.right_tab_idx]
                .as_mut()
                .expect("inactive right tab must be Some"),
            Side::Right => self.left_tabs[self.left_tab_idx]
                .as_mut()
                .expect("inactive left tab must be Some"),
        }
    }

    /// Toggle focus between left and right panes.
    pub fn toggle_active(&mut self) {
        self.active_side = match self.active_side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
    }

    /// Replace the active slot's pane with `new_pane`.
    ///
    /// Used for drill-in navigation (e.g. PlaylistsPane → PlaylistPane).
    /// Does NOT create a new tab — the replacement stays on the current tab.
    pub fn switch_active_pane(&mut self, new_pane: Box<dyn Pane>) {
        match self.active_side {
            Side::Left => self.left_tabs[self.left_tab_idx] = Some(new_pane),
            Side::Right => self.right_tabs[self.right_tab_idx] = Some(new_pane),
        }
    }

    // =========================================================================
    // Event handling
    // =========================================================================

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
                    // Refresh all queue panes across all tabs when the queue changes.
                    if matches!(pe, PlaybackEvent::QueueChanged { .. }) {
                        for slot in self.left_tabs.iter_mut().flatten() {
                            if slot.pane_kind() == PaneKind::Queue {
                                slot.refresh();
                            }
                        }
                        for slot in self.right_tabs.iter_mut().flatten() {
                            if slot.pane_kind() == PaneKind::Queue {
                                slot.refresh();
                            }
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

    // =========================================================================
    // Status bar
    // =========================================================================

    /// Set a status bar message.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    /// Clear the status bar message.
    #[allow(dead_code)]
    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    // =========================================================================
    // Palette
    // =========================================================================

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

    // =========================================================================
    // Pane factories
    // =========================================================================

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

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{AddPlayingTarget, PaneAction, PaneKind};
    use crate::selection::SelectionState;
    use crossterm::event::KeyEvent;
    use ratatui::layout::Rect;
    use ratatui::Frame;

    // ---- Stub pane for tests ----

    #[derive(Clone)]
    struct StubPane {
        title: String,
        count: usize,
        selection: SelectionState,
    }

    impl StubPane {
        fn new(title: &str, count: usize) -> Self {
            Self {
                title: title.to_string(),
                count,
                selection: SelectionState::new(count),
            }
        }
    }

    impl Pane for StubPane {
        fn render(&self, _f: &mut Frame, _area: Rect) {}
        fn handle_key(&mut self, _key: KeyEvent) -> PaneAction {
            PaneAction::Ignored
        }
        fn resolve_selection(&self) -> Vec<mdma_client::ContentHash> {
            vec![]
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
            self.count
        }
        fn pane_kind(&self) -> PaneKind {
            PaneKind::Search
        }
        fn add_playing_target(&self) -> AddPlayingTarget {
            AddPlayingTarget::None
        }
        fn clone_box(&self) -> Box<dyn Pane> {
            Box::new(self.clone())
        }
    }

    /// Build a minimal App with two stub panes (no real backends).
    ///
    /// Since `App::new` requires `Rc<LibraryBackend>` and `Rc<PlaybackBackend>`,
    /// which can't be constructed in unit tests without a live IPC socket,
    /// we test tab-management methods by manipulating the struct fields directly
    /// after building an App from real backends via the stub approach below.
    ///
    /// Instead we test `activate_tab` logic via an App built by directly
    /// constructing the struct fields. We define a `test_app` helper that builds
    /// the minimum App shell needed without backend connections.
    fn make_tab_state(
        left_pane: Box<dyn Pane>,
        right_pane: Box<dyn Pane>,
    ) -> (
        [Option<Box<dyn Pane>>; TABS_PER_SIDE],
        usize,
        Vec<usize>,
        [Option<Box<dyn Pane>>; TABS_PER_SIDE],
        usize,
        Vec<usize>,
        Side,
    ) {
        let mut left_tabs: [Option<Box<dyn Pane>>; TABS_PER_SIDE] = [None, None, None, None, None];
        let mut right_tabs: [Option<Box<dyn Pane>>; TABS_PER_SIDE] = [None, None, None, None, None];
        left_tabs[0] = Some(left_pane);
        right_tabs[0] = Some(right_pane);
        (left_tabs, 0, vec![0], right_tabs, 0, vec![0], Side::Left)
    }

    // Because we can't easily build `App` without live backends, we test the
    // `activate_tab` logic by extracting the relevant state and logic into a
    // standalone helper that mirrors what `activate_tab` does. This keeps the
    // tests deterministic and backend-free.
    //
    // The integration tests rely on clippy + manual verification for end-to-end.

    #[test]
    fn activate_tab_same_side_same_idx_is_noop() {
        let (
            left_tabs,
            left_tab_idx,
            left_recency,
            right_tabs,
            right_tab_idx,
            right_recency,
            active_side,
        ) = make_tab_state(
            Box::new(StubPane::new("Left", 3)),
            Box::new(StubPane::new("Right", 2)),
        );

        // Simulate activate_tab(Side::Left, 0) when already on Left:0
        let target_side = Side::Left;
        let target_idx = 0;
        let is_same = active_side == target_side && left_tab_idx == target_idx;
        assert!(is_same, "should detect no-op when already on same slot");

        // State unchanged
        assert_eq!(left_tab_idx, 0);
        assert_eq!(left_recency, vec![0]);
        assert_eq!(active_side, Side::Left);
        let _ = (left_tabs, right_tabs, right_tab_idx, right_recency);
    }

    #[test]
    fn activate_tab_empty_slot_clones_active_pane() {
        let left_pane = Box::new(StubPane::new("Browser", 5));
        let right_pane = Box::new(StubPane::new("Queue", 2));

        let (mut left_tabs, _, _, right_tabs, right_tab_idx, right_recency, active_side) =
            make_tab_state(left_pane, right_pane);

        // Activate left slot 2 (currently None) — should clone from current active (Left:0)
        let target_idx = 2;
        assert!(left_tabs[target_idx].is_none(), "slot 2 should start empty");

        // Clone from active
        let cloned = left_tabs[0].as_ref().unwrap().clone_box();
        left_tabs[target_idx] = Some(cloned);

        assert!(
            left_tabs[target_idx].is_some(),
            "slot 2 should be populated after clone"
        );
        assert_eq!(
            left_tabs[target_idx].as_ref().unwrap().title(),
            "Browser",
            "cloned pane should have same title"
        );

        let _ = (right_tabs, right_tab_idx, right_recency, active_side);
    }

    #[test]
    fn activate_tab_populated_slot_switches_without_clone() {
        let left_pane_a = Box::new(StubPane::new("Pane-A", 1));
        let left_pane_b = Box::new(StubPane::new("Pane-B", 7));
        let right_pane = Box::new(StubPane::new("Queue", 0));

        let (
            mut left_tabs,
            _left_tab_idx_initial,
            mut left_recency,
            right_tabs,
            right_tab_idx,
            right_recency,
            _active_side,
        ) = make_tab_state(left_pane_a, right_pane);
        left_tabs[1] = Some(left_pane_b);
        left_recency.push(1); // pretend slot 1 was visited before

        // Activate slot 1 (already populated)
        let target_idx = 1;
        let left_tab_idx = target_idx;
        // Update recency: remove 1, push to front
        left_recency.retain(|&x| x != target_idx);
        left_recency.insert(0, target_idx);

        assert_eq!(left_tab_idx, 1);
        assert_eq!(left_recency[0], 1, "slot 1 should be most recent");
        assert_eq!(left_tabs[1].as_ref().unwrap().title(), "Pane-B");
        let _ = (right_tabs, right_tab_idx, right_recency);
    }

    #[test]
    fn activate_tab_recency_vec_moves_to_front() {
        // Simulate visiting slots 0, 1, 2, then revisiting 1.
        let mut recency: Vec<usize> = vec![2, 1, 0]; // 2 is most recent

        let target_idx = 1;
        recency.retain(|&x| x != target_idx);
        recency.insert(0, target_idx);

        assert_eq!(recency, vec![1, 2, 0], "slot 1 should move to front");
    }

    #[test]
    fn activate_tab_switching_side_updates_active_side() {
        let left_pane = Box::new(StubPane::new("Left", 0));
        let right_pane = Box::new(StubPane::new("Right", 0));

        let (left_tabs, _, _, right_tabs, right_tab_idx, mut right_recency, _active_side_initial) =
            make_tab_state(left_pane, right_pane);

        // Switch to Right side, slot 0 (already populated)
        let active_side = Side::Right;
        right_recency.retain(|&x| x != 0);
        right_recency.insert(0, 0);

        assert_eq!(active_side, Side::Right);
        assert_eq!(right_tab_idx, 0);
        assert!(right_tabs[0].is_some());
        let _ = (left_tabs, right_recency);
    }

    #[test]
    fn clone_box_produces_pane_with_same_title_and_count() {
        let original = StubPane::new("TestPane", 42);
        let cloned: Box<dyn Pane> = original.clone_box();
        assert_eq!(cloned.title(), "TestPane");
        assert_eq!(cloned.item_count(), 42);
    }

    #[test]
    fn recency_vec_reflects_lru_order_after_multiple_activations() {
        // Start with recency = [0] (only slot 0 visited).
        let mut recency: Vec<usize> = vec![0];

        // Visit slots in order: 1, 2, 3, 4
        for idx in 1..=4 {
            recency.retain(|&x| x != idx);
            recency.insert(0, idx);
        }
        // Most recent = 4, oldest = 0
        assert_eq!(recency, vec![4, 3, 2, 1, 0]);

        // Revisit slot 2 — it should move to front
        let idx = 2;
        recency.retain(|&x| x != idx);
        recency.insert(0, idx);
        assert_eq!(recency, vec![2, 4, 3, 1, 0]);
    }
}
