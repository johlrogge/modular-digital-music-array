//! Output helpers: track formatting and tables, and uniform error reporting.

use colored::Colorize;
use library_ipc_client::{ClientError, ProtocolError, TrackInfo};
use mdma_client::PlaybackClientError;
use track_formatter::{
    format_track_line as format_track_line_shared, render_for_user, ALL_COLUMNS,
};

/// Canonical playlist line format: `{short_hash}  {Artist} - {Title}  [{duration}]`
/// Used for pipe mode output and temp files (no colors, no alignment).
///
/// Delegates to `track_formatter::format_track_line` — the shared implementation
/// used by both CLI and TUI to guarantee format consistency.
pub fn format_track_line(track: &TrackInfo) -> String {
    format_track_line_shared(track)
}

/// Detect terminal width.
///
/// Priority:
/// 1. `$COLUMNS` env var — honoured first so BDD tests can override the width.
/// 2. ioctl via `terminal_size` — reads the actual PTY/terminal dimensions.
/// 3. Hard-coded fallback of 80 — used in CI / piped contexts.
pub fn terminal_width() -> usize {
    // Prefer $COLUMNS if explicitly set (e.g. in BDD tests)
    if let Some(w) = std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()) {
        return w;
    }
    // Query the actual terminal via ioctl
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80)
}

/// Virtual terminal width used when stdout is not a TTY (pipe, file redirection).
///
/// Wide enough to guarantee that corsett never shrinks the 8-char hash column,
/// and that typical artist/title content is not truncated (Joakim's uniqueness hypothesis).
pub const PIPE_TERM_WIDTH: usize = 200;

/// Render tracks as a colored, aligned table for terminal display.
/// Returns one formatted string per track row.
///
/// `reserved_prefix` is subtracted from the terminal width before column sizing.
/// Pass `0` for plain output; for queue display, pass `pos_width + 3` (digit(s) + "." + 2 spaces).
pub fn render_track_table(tracks: &[TrackInfo], reserved_prefix: usize) -> Vec<String> {
    render_track_table_inner(tracks, reserved_prefix, terminal_width())
}

/// Inner implementation of `render_track_table` that accepts an explicit `term_width`,
/// allowing tests to inject a known width without relying on `$COLUMNS`.
///
/// Delegates to `track_formatter::render_for_user` with the full default column set.
pub fn render_track_table_inner(
    tracks: &[TrackInfo],
    reserved_prefix: usize,
    term_width: usize,
) -> Vec<String> {
    render_for_user(tracks, ALL_COLUMNS, term_width, reserved_prefix)
}

/// Print tracks with a bold header in terminal mode; canonical lines in pipe mode.
pub fn print_tracks(tracks: &[TrackInfo], header: &str) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        println!("{}", header.bold());
        println!();
        for line in render_track_table(tracks, 0) {
            println!("{}", line);
        }
    } else {
        for track in tracks {
            println!("{}", format_track_line(track));
        }
    }
}

/// Handle library client errors uniformly
pub fn handle_error(err: ClientError) -> ! {
    match err {
        ClientError::Protocol(ProtocolError::TrackNotFound { hash }) => {
            eprintln!("Track not found: {}", hash);
        }
        ClientError::Protocol(ProtocolError::InboxFileNotFound { path }) => {
            eprintln!("Inbox file not found: {}", path);
        }
        ClientError::Protocol(e) => {
            eprintln!("Error: {}", e);
        }
        ClientError::Transport(nng_transport::NngClientError::Connection(e)) => {
            eprintln!("Connection failed: {}", e);
            eprintln!("Is mdma-library running?");
        }
        e => {
            eprintln!("Error: {}", e);
        }
    }
    std::process::exit(1);
}

/// Handle source errors uniformly
pub fn handle_source_error(err: &dyn std::fmt::Display) -> ! {
    eprintln!("Error: {}", err);
    std::process::exit(1);
}

pub fn handle_playback_error(err: PlaybackClientError) -> ! {
    match err {
        PlaybackClientError::Connection(e) => {
            eprintln!("Connection failed: {}", e);
            eprintln!("Is mdma-playback running?");
        }
        e => {
            eprintln!("Error: {}", e);
        }
    }
    std::process::exit(1);
}
