use crate::app::{App, InputMode};
use crate::pane::PaneAction;
use crate::playlist_pane::PlaylistPane;
use crossterm::event::{KeyCode, KeyEvent};
use std::rc::Rc;

/// Dispatch a key event to the application based on the current input mode.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => handle_normal(app, key),
        InputMode::Command => handle_command(app, key),
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
            app.mode = InputMode::Command;
            app.command_input.clear();
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

fn handle_command(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) => {
            app.command_input.push(c);
        }
        KeyCode::Backspace => {
            app.command_input.pop();
        }
        KeyCode::Enter => {
            let cmd = app.command_input.clone();
            app.command_input.clear();
            app.mode = InputMode::Normal;
            match cmd.trim() {
                "q" | "quit" => {
                    app.should_quit = true;
                }
                other => {
                    app.set_status(format!("Unknown command: {}", other));
                }
            }
        }
        KeyCode::Esc => {
            app.command_input.clear();
            app.mode = InputMode::Normal;
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
