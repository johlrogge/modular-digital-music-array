use crate::app::{App, InputMode};
use crate::commands::Command;
use crate::pane::PaneAction;
use crate::playlist_pane::PlaylistPane;
use crossterm::event::{KeyCode, KeyEvent};
use mdma_client::{Deck, PlaybackBackend};
use std::rc::Rc;

/// Dispatch a key event to the application based on the current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => handle_normal(app, key),
        InputMode::Palette => handle_palette(app, key),
        InputMode::FilterInput => handle_filter(app, key),
    }
}

fn handle_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') => {
            app.should_quit = true;
        }
        KeyCode::Tab => {
            app.toggle_active();
        }
        KeyCode::Char(':') => {
            app.open_palette();
        }
        KeyCode::Char('s') => {
            app.mode = InputMode::FilterInput;
            app.filter_input.clear();
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
        KeyCode::Down | KeyCode::Char('j') => {
            let len = app.palette_matches.len();
            if len > 0 {
                app.palette_cursor = (app.palette_cursor + 1) % len;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
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
            if let Some(&cmd) = app.palette_matches.get(cursor) {
                let playback = Rc::clone(&app.playback);
                execute_command(cmd, &playback, app);
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
        }
        KeyCode::Backspace => {
            app.filter_input.pop();
        }
        KeyCode::Enter => {
            let pattern = app.filter_input.clone();
            app.filter_input.clear();
            app.mode = InputMode::Normal;

            // Stub: accept everything — full text matching is wired in Task #3
            // when panes expose their display strings.
            let _ = pattern;
            app.active_pane_mut()
                .selection_state_mut()
                .push_filter(|_data_idx| true);
        }
        KeyCode::Esc => {
            app.filter_input.clear();
            app.mode = InputMode::Normal;
            app.active_pane_mut().selection_state_mut().pop_filter();
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
        "quit" => {
            app.should_quit = true;
        }
        "search" => {
            let p = app.make_search_pane();
            app.switch_active_pane(p);
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
        _ => {
            app.set_status(format!("Unknown command: {}", cmd.name));
        }
    }
}

/// Route a PaneAction returned from a pane's key handler to the App.
fn dispatch_pane_action(app: &mut App, action: PaneAction) {
    match action {
        PaneAction::Consumed => {}
        PaneAction::Ignored => {}
        PaneAction::Error(msg) => app.set_status(format!("Error: {}", msg)),
        PaneAction::Info(msg) => app.set_status(msg),
        PaneAction::OpenPlaylist(name) => {
            // Open the requested playlist in the INACTIVE pane.
            let library = Rc::clone(&app.library);
            match PlaylistPane::open(name.clone(), library) {
                Ok(playlist_pane) => {
                    *app.inactive_pane_mut() = Box::new(playlist_pane);
                    app.set_status(format!("Opened playlist: {}", name));
                }
                Err(e) => {
                    app.set_status(format!("Failed to open playlist {}: {}", name, e));
                }
            }
        }
    }
}
