use crate::app::{App, InputMode, PaletteEntry, Side};
use crate::commands::Command;
use crate::now_playing::PlaybackStatus;
use crate::pane::{AddPlayingTarget, PaneAction, PaneKind};
use crate::playlist_pane::PlaylistPane;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{ContentHash, Deck, PlaybackBackend, PlaylistName, SourceName};
use std::rc::Rc;

/// Dispatch a key event to the application based on the current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => handle_normal(app, key),
        InputMode::Palette => handle_palette(app, key),
        InputMode::FilterInput => handle_filter(app, key),
        InputMode::Help => app.mode = InputMode::Normal,
        InputMode::Playback => handle_playback(app, key),
        InputMode::NameInput => handle_name_input(app, key),
    }
}

fn handle_normal(app: &mut App, key: KeyEvent) {
    // If the active pane has an internal text-input field active, it owns ALL
    // keys — including characters that would normally trigger App-level bindings
    // ('a', 's', 'A', ':', '/', digits, etc.).  The pane handles Esc itself to
    // exit editing mode.
    if app.active_pane().capturing_text_input() {
        let action = app.active_pane_mut().handle_key(key);
        dispatch_pane_action(app, action);
        return;
    }

    // Pane-level preemption: some panes claim specific keys before the
    // App-level match below can handle them (e.g. PlaylistPane claims 'd'
    // and 'p' so they act as cut/paste rather than global bindings).
    if app.active_pane().preempts_normal_key(&key) {
        // 'p' (paste) requires the clipboard from App — inject it here so the
        // pane stays unaware of App.  We route through the `paste_clipboard`
        // trait method rather than PaneAction to avoid an extra round-trip.
        if key.code == KeyCode::Char('p') {
            let clipboard = app.clipboard.clone();
            if clipboard.is_empty() {
                app.set_status("Clipboard is empty");
            } else {
                let action = app.active_pane_mut().paste_clipboard(clipboard);
                dispatch_pane_action(app, action);
            }
            return;
        }

        let action = app.active_pane_mut().handle_key(key);
        dispatch_pane_action(app, action);
        return;
    }

    match key.code {
        // Tab slot keys: 1-5 = left side, 6-9,0 = right side.
        KeyCode::Char(c @ '1'..='5') => {
            let idx = (c as u8 - b'1') as usize;
            app.activate_tab(Side::Left, idx);
        }
        KeyCode::Char(c @ '6'..='9') => {
            let idx = (c as u8 - b'6') as usize;
            app.activate_tab(Side::Right, idx);
        }
        KeyCode::Char('0') => {
            app.activate_tab(Side::Right, 4);
        }
        KeyCode::Tab => {
            app.toggle_active();
        }
        KeyCode::Char(':') => {
            app.open_palette();
        }
        KeyCode::Char('?') => {
            app.mode = InputMode::Help;
        }
        KeyCode::Char('s') | KeyCode::Char('/') => {
            app.mode = InputMode::FilterInput;
            app.filter_input.clear();
            app.live_filter_active = false;
        }
        KeyCode::Char('a') => {
            // Add selection from active pane to inactive pane.
            let hashes = app.active_pane().resolve_selection();
            if hashes.is_empty() {
                app.set_status("No tracks selected");
            } else {
                let count = hashes.len();
                let action = app.inactive_pane_mut().accept_tracks(&hashes);
                match action {
                    PaneAction::Error(msg) => app.set_status(msg),
                    PaneAction::Info(msg) => app.set_status(msg),
                    _ => app.set_status(format!("Added {} track(s)", count)),
                }
            }
        }
        KeyCode::Char('q') => {
            let hashes = app.active_pane().resolve_selection();
            if hashes.is_empty() {
                app.set_status("No tracks selected");
            } else {
                let count = hashes.len();
                for hash in &hashes {
                    let _ = app.playback.queue_append(hash.clone(), SourceName::audio());
                }
                app.set_status(format!("Queued {} track(s)", count));
            }
        }
        KeyCode::Char('Q') => {
            let hashes = app.active_pane().resolve_selection();
            if hashes.is_empty() {
                app.set_status("No tracks selected");
            } else {
                let count = hashes.len();
                for hash in hashes.iter().rev() {
                    let _ = app.playback.queue_next(hash.clone(), SourceName::audio());
                }
                app.set_status(format!("Queued next {} track(s)", count));
            }
        }
        KeyCode::Char('A') => {
            // Add the currently-playing track to the focused pane (queue or playlist).
            let hash: Option<ContentHash> = match &app.now_playing.status {
                PlaybackStatus::Playing { track, .. } | PlaybackStatus::Paused { track, .. } => {
                    Some(track.clone())
                }
                PlaybackStatus::Stopped => None,
            };
            match hash {
                None => app.set_status("Nothing playing"),
                Some(h) => {
                    let target = app.active_pane().add_playing_target();
                    match target {
                        AddPlayingTarget::Queue => {
                            match app.playback.queue_append(h, SourceName::audio()) {
                                Ok(_) => {
                                    app.active_pane_mut().refresh();
                                    app.set_status("Added playing track to queue");
                                }
                                Err(e) => app.set_status(format!("Queue append failed: {e}")),
                            }
                        }
                        AddPlayingTarget::Playlist(name) => {
                            match app.library.playlist_append(&name, &[h]) {
                                Ok(()) => {
                                    app.active_pane_mut().refresh();
                                    app.set_status(format!("Added playing track to {}", name));
                                }
                                Err(e) => app.set_status(format!("Playlist append failed: {e}")),
                            }
                        }
                        AddPlayingTarget::None => {
                            app.set_status("No playlist/queue focused");
                        }
                    }
                }
            }
        }
        KeyCode::Char('P') => {
            let hashes = app.active_pane().resolve_selection();
            if hashes.is_empty() {
                app.set_status("No track selected");
            } else {
                let count = hashes.len();
                let result = (|| -> Result<(), mdma_client::PlaybackClientError> {
                    for hash in queue_next_order(hashes) {
                        app.playback.queue_next(hash, SourceName::audio())?;
                    }
                    app.playback.skip()?;
                    Ok(())
                })();
                match result {
                    Ok(()) => app.set_status(format!("Playing {} track(s)", count)),
                    Err(e) => app.set_status(format!("Play failed: {e}")),
                }
            }
        }
        KeyCode::Char('p') => {
            app.mode = InputMode::Playback;
        }
        KeyCode::Char('n') if app.active_pane().pane_kind() == PaneKind::PlaylistsList => {
            app.name_input.clear();
            app.mode = InputMode::NameInput;
        }
        KeyCode::Char('u') => {
            let action = app.active_pane_mut().undo();
            dispatch_pane_action(app, action);
        }
        _ => {
            let action = app.active_pane_mut().handle_key(key);
            dispatch_pane_action(app, action);
        }
    }
}

fn handle_palette(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.close_palette();
        }
        KeyCode::Down => {
            let len = app.palette_matches.len();
            if len > 0 {
                app.palette_cursor = (app.palette_cursor + 1) % len;
            }
        }
        KeyCode::Up => {
            let len = app.palette_matches.len();
            if len > 0 {
                app.palette_cursor = app.palette_cursor.checked_sub(1).unwrap_or(len - 1);
            }
        }
        KeyCode::Char(c) => {
            let mut query = app.palette_query.clone();
            query.push(c);
            app.palette_update_query(query);
        }
        KeyCode::Backspace => {
            let mut query = app.palette_query.clone();
            query.pop();
            app.palette_update_query(query);
        }
        KeyCode::Enter => {
            let cursor = app.palette_cursor;
            if let Some(entry) = app.palette_matches.get(cursor).cloned() {
                match entry {
                    PaletteEntry::Command(cmd) => {
                        let playback = Rc::clone(&app.playback);
                        execute_command(cmd, &playback, app);
                    }
                    PaletteEntry::OpenPlaylist(name) => {
                        let library = Rc::clone(&app.library);
                        match PlaylistPane::open(name.clone(), library) {
                            Ok(pane) => {
                                app.switch_active_pane(Box::new(pane));
                                app.set_status(format!("Opened: {}", name));
                            }
                            Err(e) => app.set_status(format!("Open failed: {e}")),
                        }
                    }
                    PaletteEntry::CreatePlaylist(name_str) => match PlaylistName::new(&name_str) {
                        Ok(name) => match app.library.playlist_new(&name, &[]) {
                            Ok(()) => {
                                let library = Rc::clone(&app.library);
                                match PlaylistPane::open(name.clone(), library) {
                                    Ok(pane) => {
                                        app.switch_active_pane(Box::new(pane));
                                        app.set_status(format!("Created: {}", name_str));
                                    }
                                    Err(e) => {
                                        app.set_status(format!("Created but open failed: {e}"))
                                    }
                                }
                            }
                            Err(e) => app.set_status(format!("Create failed: {e}")),
                        },
                        Err(e) => app.set_status(format!("Invalid name: {e}")),
                    },
                    PaletteEntry::History(arg) => match parse_history_days(&arg) {
                        Ok(days) => open_history_pane(app, days),
                        Err(_) => {
                            app.set_status("history: number of days required (e.g., :history 7)")
                        }
                    },
                }
            }
            app.close_palette();
        }
        _ => {}
    }
}

fn handle_filter(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) => {
            app.filter_input.push(c);
            apply_live_filter(app);
        }
        KeyCode::Backspace => {
            app.filter_input.pop();
            apply_live_filter(app);
        }
        KeyCode::Enter => {
            // Commit: exit FilterInput mode and stay in Normal.
            // The live filter (if any) remains applied; reset tracking flag so
            // the next FilterInput session pushes a fresh layer.
            app.mode = InputMode::Normal;
            app.live_filter_active = false;
        }
        KeyCode::Esc => {
            // Cancel: remove the live filter and return to Normal.
            app.filter_input.clear();
            app.mode = InputMode::Normal;
            if app.live_filter_active {
                app.active_pane_mut().selection_state_mut().pop_filter();
                app.live_filter_active = false;
            }
        }
        _ => {}
    }
}

fn handle_playback(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('p') | KeyCode::Char(' ') => {
            // Toggle: if playing → pause, otherwise → play
            let action = match &app.now_playing.status {
                PlaybackStatus::Playing { .. } => {
                    let _ = app.playback.pause(Deck::A);
                    "Paused"
                }
                _ => {
                    let _ = app.playback.play_queue();
                    "Playing"
                }
            };
            app.set_status(action);
        }
        KeyCode::Char('s') => {
            let _ = app.playback.stop(Deck::A);
            app.set_status("Stopped");
        }
        KeyCode::Char('n') => {
            let _ = app.playback.skip();
            app.set_status("Skipped");
        }
        KeyCode::Char('c') => {
            let _ = app.playback.queue_clear();
            app.set_status("Queue cleared");
        }
        KeyCode::Char('b') => {
            let hash: Option<ContentHash> = match &app.now_playing.status {
                PlaybackStatus::Playing { track, .. } | PlaybackStatus::Paused { track, .. } => {
                    Some(track.clone())
                }
                PlaybackStatus::Stopped => None,
            };
            match hash {
                None => app.set_status("Nothing playing"),
                Some(h) => match app.library.write_bookmark(&h, None) {
                    Ok(()) => app.set_status("Bookmarked"),
                    Err(e) => app.set_status(format!("Bookmark failed: {e}")),
                },
            }
        }
        KeyCode::Esc => {
            app.mode = InputMode::Normal;
        }
        _ => {} // any unrecognised key stays in Playback mode
    }
}

fn handle_name_input(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.name_input.clear();
            app.mode = InputMode::Normal;
        }
        KeyCode::Backspace => {
            app.name_input.pop();
        }
        KeyCode::Enter => {
            let name_str = app.name_input.trim().to_string();
            app.name_input.clear();
            app.mode = InputMode::Normal;
            if name_str.is_empty() {
                return;
            }
            let name = match PlaylistName::new(&name_str) {
                Ok(n) => n,
                Err(e) => {
                    app.set_status(format!("Invalid playlist name: {e}"));
                    return;
                }
            };
            match app.library.playlist_new(&name, &[]) {
                Ok(()) => {
                    app.active_pane_mut().refresh();
                    app.set_status(format!("Created playlist \"{}\"", name_str));
                }
                Err(e) => app.set_status(format!("Error: {e}")),
            }
        }
        KeyCode::Char(c) => {
            app.name_input.push(c);
        }
        _ => {}
    }
}

/// Execute a palette command against the playback backend, updating app status.
fn execute_command(cmd: &Command, playback: &PlaybackBackend, app: &mut App) {
    match cmd.name {
        "play" => {
            let _ = playback.play_queue();
            app.set_status("Playing");
        }
        "pause" => {
            let _ = playback.pause(Deck::A);
            app.set_status("Paused");
        }
        "stop" => {
            let _ = playback.stop(Deck::A);
            app.set_status("Stopped");
        }
        "next" => {
            let _ = playback.skip();
            app.set_status("Skipped");
        }
        "clear" => {
            let _ = playback.queue_clear();
            app.set_status("Queue cleared");
        }
        "shuffle" => {
            app.set_status("shuffle not yet implemented");
        }
        "q" | "quit" => {
            app.should_quit = true;
        }
        "search" => {
            let p = app.make_search_pane();
            app.switch_active_pane(p);
        }
        "history" => {
            // Default: 7 days (no argument)
            open_history_pane(app, 7);
        }
        "browser" => {
            let p = app.make_browser_pane();
            app.switch_active_pane(p);
        }
        "queue" => {
            let p = app.make_queue_pane();
            app.switch_active_pane(p);
        }
        "playlists" => match app.make_playlists_pane() {
            Ok(p) => app.switch_active_pane(p),
            Err(e) => app.set_status(format!("Playlists: {e}")),
        },
        "o" => {
            app.set_status(":o <name>  — open or create a playlist");
        }
        _ => {
            app.set_status(format!("Unknown command: {}", cmd.name));
        }
    }
}

/// Apply (or replace) the live filter on the active pane.
///
/// If a live filter is already active (`app.live_filter_active == true`), the
/// top of the filter stack is replaced. Otherwise a new filter is pushed and
/// `live_filter_active` is set to `true`.
///
/// If `pattern` is empty the live filter is removed (pop) and
/// `live_filter_active` is reset to `false`.
fn apply_live_filter(app: &mut App) {
    let pattern = app.filter_input.clone();

    if pattern.is_empty() {
        if app.live_filter_active {
            app.active_pane_mut().selection_state_mut().pop_filter();
            app.live_filter_active = false;
        }
        return;
    }

    let pattern_lc = pattern.to_ascii_lowercase();
    let total = app.active_pane().item_count();
    let display_strings: Vec<Option<String>> = (0..total)
        .map(|i| app.active_pane().display_string(i))
        .collect();

    let predicate = move |data_idx: usize| match display_strings.get(data_idx) {
        Some(Some(text)) => text.to_ascii_lowercase().contains(&pattern_lc),
        _ => true,
    };

    if app.live_filter_active {
        app.active_pane_mut()
            .selection_state_mut()
            .replace_top_filter(predicate);
    } else {
        app.active_pane_mut()
            .selection_state_mut()
            .push_filter(predicate);
        app.live_filter_active = true;
    }
}

// =========================================================================
// History helpers
// =========================================================================

/// Parse the optional argument to the `:history` palette command.
///
/// - `""` → `Ok(7)` (default: last 7 days)
/// - `"N"` where N is a non-negative integer → `Ok(N)`
/// - anything else → `Err(())`
pub fn parse_history_days(arg: &str) -> Result<u32, ()> {
    if arg.is_empty() {
        return Ok(7);
    }
    arg.parse::<u32>().map_err(|_| ())
}

/// Build the search query string for a history search of `days` days.
///
/// - `0` → `:started '~'`  (today only)
/// - N → `:started '-N'`
fn history_query(days: u32) -> String {
    if days == 0 {
        ":started '~'".to_string()
    } else {
        format!(":started '-{}'", days)
    }
}

/// Switch the active pane to a SearchPane pre-filled with a history query.
fn open_history_pane(app: &mut App, days: u32) {
    let query = history_query(days);
    let (pane, action) = app.make_search_pane_with_query(query);
    app.switch_active_pane(pane);
    if let PaneAction::Error(msg) = action {
        app.set_status(msg);
    }
}

// =========================================================================
// Tests
// =========================================================================
//
// `App` requires live IPC backends, so these tests exercise the filter logic
// directly via `SelectionState` + display-string slices — the same path that
// `apply_live_filter` takes internally.  The `SelectionState` tests in
// `selection.rs` cover `push_filter` / `replace_top_filter` correctness; here
// we verify the higher-level "keystroke-by-keystroke narrowing" invariant.

#[cfg(test)]
mod tests {
    use crate::selection::SelectionState;

    /// Simulate `apply_live_filter` for a given pattern against a fixed set of
    /// display strings.  Returns the visible data indices after applying the filter.
    ///
    /// `live_active` indicates whether a live filter is already pushed (i.e. this
    /// is not the first keystroke in the session).
    fn apply_filter_sim(
        state: &mut SelectionState,
        display: &[&str],
        pattern: &str,
        live_active: &mut bool,
    ) {
        if pattern.is_empty() {
            if *live_active {
                state.pop_filter();
                *live_active = false;
            }
            return;
        }
        let pattern_lc = pattern.to_ascii_lowercase();
        let display_owned: Vec<String> = display.iter().map(|s| s.to_string()).collect();
        let predicate = move |data_idx: usize| {
            display_owned
                .get(data_idx)
                .map(|t| t.to_ascii_lowercase().contains(&pattern_lc))
                .unwrap_or(true)
        };
        if *live_active {
            state.replace_top_filter(predicate);
        } else {
            state.push_filter(predicate);
            *live_active = true;
        }
    }

    const ITEMS: &[&str] = &["Destination Calabria", "Destroyed", "Daft Punk", "Moby"];

    #[test]
    fn live_filter_narrows_on_each_char() {
        let mut state = SelectionState::new(ITEMS.len());
        let mut live_active = false;

        // Type 'd' → "Destination Calabria", "Destroyed", "Daft Punk" match (case-insensitive)
        apply_filter_sim(&mut state, ITEMS, "d", &mut live_active);
        assert!(live_active, "live filter should be active after first char");
        assert_eq!(state.visible_count(), 3, "d: 3 matches");

        // Type 'e' → pattern "de" → "Destination Calabria", "Destroyed"
        apply_filter_sim(&mut state, ITEMS, "de", &mut live_active);
        assert_eq!(state.visible_count(), 2, "de: 2 matches");

        // Type 's' → pattern "des" → "Destination Calabria", "Destroyed"
        apply_filter_sim(&mut state, ITEMS, "des", &mut live_active);
        assert_eq!(state.visible_count(), 2, "des: 2 matches");

        // Type 't' → pattern "dest" → "Destination Calabria", "Destroyed"
        apply_filter_sim(&mut state, ITEMS, "dest", &mut live_active);
        assert_eq!(state.visible_count(), 2, "dest: 2 matches");

        // Only one filter layer on the stack (no stacking on each keystroke)
        assert_eq!(state.filter_depth(), 1);
    }

    #[test]
    fn live_filter_backspace_widens_results() {
        let mut state = SelectionState::new(ITEMS.len());
        let mut live_active = false;

        apply_filter_sim(&mut state, ITEMS, "dest", &mut live_active);
        assert_eq!(state.visible_count(), 2);

        // Backspace: "des" — still 2
        apply_filter_sim(&mut state, ITEMS, "des", &mut live_active);
        assert_eq!(state.visible_count(), 2);

        // Backspace to "d" — 3 matches
        apply_filter_sim(&mut state, ITEMS, "d", &mut live_active);
        assert_eq!(state.visible_count(), 3);
    }

    #[test]
    fn live_filter_cleared_on_empty_pattern() {
        let mut state = SelectionState::new(ITEMS.len());
        let mut live_active = false;

        apply_filter_sim(&mut state, ITEMS, "dest", &mut live_active);
        assert!(live_active);
        assert_eq!(state.visible_count(), 2);

        // Backspace all the way to empty
        apply_filter_sim(&mut state, ITEMS, "", &mut live_active);
        assert!(
            !live_active,
            "live filter should be inactive after empty pattern"
        );
        assert_eq!(
            state.visible_count(),
            ITEMS.len(),
            "all items visible after clearing"
        );
    }

    #[test]
    fn enter_commits_filter_leaving_it_applied() {
        // Enter key in handle_filter sets live_filter_active = false and exits mode.
        // The filter stays on the stack. Simulate: push a filter, then commit.
        let mut state = SelectionState::new(ITEMS.len());
        let mut live_active = false;

        apply_filter_sim(&mut state, ITEMS, "dest", &mut live_active);
        assert_eq!(state.visible_count(), 2);

        // Commit: reset live_filter_active (Enter behaviour)
        live_active = false;

        // Filter is still applied
        assert_eq!(state.visible_count(), 2, "filter remains after commit");

        // A subsequent filter session pushes on top (not replaces)
        apply_filter_sim(&mut state, ITEMS, "destination", &mut live_active);
        // "destination calabria" contains "destination" — 1 match
        assert_eq!(state.visible_count(), 1);
        // Two layers: committed + new live
        assert_eq!(state.filter_depth(), 2);
    }

    #[test]
    fn esc_removes_live_filter() {
        let mut state = SelectionState::new(ITEMS.len());
        let mut live_active = false;

        apply_filter_sim(&mut state, ITEMS, "dest", &mut live_active);
        assert_eq!(state.visible_count(), 2);

        // Esc behaviour: pop if live_active
        if live_active {
            state.pop_filter();
            live_active = false;
        }
        assert!(!live_active);
        assert_eq!(
            state.visible_count(),
            ITEMS.len(),
            "all items restored after Esc"
        );
    }

    // ---- quick_play_insert_order ----

    /// Verify that `queue_next_order` returns hashes in reverse input order,
    /// so that inserting each in sequence leaves the original order as "next up".
    #[test]
    fn quick_play_insert_order_is_reversed() {
        use mdma_client::ContentHash;

        let hashes = vec![
            ContentHash::new("aabbcc001122"),
            ContentHash::new("ddeeff334455"),
            ContentHash::new("112233aabbcc"),
        ];
        let order = super::queue_next_order(hashes.clone());

        // Reverse: C, B, A — so after inserting each as "next", A ends up first.
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], hashes[2]);
        assert_eq!(order[1], hashes[1]);
        assert_eq!(order[2], hashes[0]);
    }

    #[test]
    fn quick_play_insert_order_single_element() {
        use mdma_client::ContentHash;

        let hashes = vec![ContentHash::new("aabbcc001122")];
        let order = super::queue_next_order(hashes.clone());
        assert_eq!(order.len(), 1);
        assert_eq!(order[0], hashes[0]);
    }

    #[test]
    fn quick_play_insert_order_empty() {
        let order = super::queue_next_order(vec![]);
        assert!(order.is_empty());
    }

    // ---- parse_history_days ----

    #[test]
    fn parse_history_days_empty_defaults_to_7() {
        assert_eq!(super::parse_history_days(""), Ok(7));
    }

    #[test]
    fn parse_history_days_numeric_string() {
        assert_eq!(super::parse_history_days("7"), Ok(7));
        assert_eq!(super::parse_history_days("30"), Ok(30));
        assert_eq!(super::parse_history_days("0"), Ok(0));
    }

    #[test]
    fn parse_history_days_non_integer_is_err() {
        assert!(super::parse_history_days("abc").is_err());
        assert!(super::parse_history_days("7days").is_err());
        assert!(super::parse_history_days("-7").is_err());
    }

    // ---- history_query ----

    #[test]
    fn history_query_default_7_days() {
        assert_eq!(super::history_query(7), ":started '-7'");
    }

    #[test]
    fn history_query_30_days() {
        assert_eq!(super::history_query(30), ":started '-30'");
    }

    #[test]
    fn history_query_zero_is_today() {
        assert_eq!(super::history_query(0), ":started '~'");
    }
}

/// Return hashes in the order they should be passed to successive `queue_next`
/// calls so that the final "next up" sequence preserves the original selection
/// order.
///
/// `queue_next` inserts immediately after the currently-playing track, so
/// inserting [C, B, A] in that order leaves the queue as `[current, A, B, C,
/// rest…]`.  Reversing the caller's selection achieves this.
fn queue_next_order(hashes: Vec<ContentHash>) -> Vec<ContentHash> {
    hashes.into_iter().rev().collect()
}

/// Route a PaneAction returned from a pane's key handler to the App.
fn dispatch_pane_action(app: &mut App, action: PaneAction) {
    match action {
        PaneAction::Consumed => {}
        PaneAction::Ignored => {}
        PaneAction::Error(msg) => app.set_status(format!("Error: {}", msg)),
        PaneAction::Info(msg) => app.set_status(msg),
        PaneAction::Cut(hashes) => {
            let count = hashes.len();
            app.clipboard = hashes;
            app.set_status(format!("Cut {} track(s) — press p to paste", count));
        }
        PaneAction::OpenPlaylist(name) => {
            // Open the requested playlist in the ACTIVE pane (replaces it).
            let library = Rc::clone(&app.library);
            match PlaylistPane::open(name.clone(), library) {
                Ok(playlist_pane) => {
                    app.switch_active_pane(Box::new(playlist_pane));
                    app.set_status(format!("Opened playlist: {}", name));
                }
                Err(e) => {
                    app.set_status(format!("Failed to open playlist {}: {}", name, e));
                }
            }
        }
    }
}
