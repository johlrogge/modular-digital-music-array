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

/// An error message as a value: its text, and whether the old `handle_*`
/// printing put an `Error: ` in front of it. The text itself never carries
/// that prefix, since hicli adds `Error: ` to every failure it prints.
struct Message {
    prefixed: bool,
    text: String,
}

impl Message {
    fn prefixed(text: String) -> Self {
        Self {
            prefixed: true,
            text,
        }
    }

    fn plain(text: String) -> Self {
        Self {
            prefixed: false,
            text,
        }
    }

    /// What the `handle_*` functions print to stderr.
    fn printed(&self) -> String {
        if self.prefixed {
            format!("Error: {}", self.text)
        } else {
            self.text.clone()
        }
    }

    // Unused outside tests until the commands return errors instead of exiting.
    #[allow(dead_code)]
    fn into_report(self) -> color_eyre::Report {
        color_eyre::eyre::eyre!("{}", self.text)
    }
}

fn library_message(err: ClientError) -> Message {
    match err {
        ClientError::Protocol(ProtocolError::TrackNotFound { hash }) => {
            Message::plain(format!("Track not found: {}", hash))
        }
        ClientError::Protocol(ProtocolError::InboxFileNotFound { path }) => {
            Message::plain(format!("Inbox file not found: {}", path))
        }
        ClientError::Protocol(e) => Message::prefixed(e.to_string()),
        ClientError::Transport(nng_transport::NngClientError::Connection(e)) => Message::plain(
            format!("Connection failed: {}\nIs mdma-library running?", e),
        ),
        e => Message::prefixed(e.to_string()),
    }
}

fn playback_message(err: PlaybackClientError) -> Message {
    match err {
        PlaybackClientError::Connection(e) => Message::plain(format!(
            "Connection failed: {}\nIs mdma-playback running?",
            e
        )),
        e => Message::prefixed(e.to_string()),
    }
}

/// The message for a library client error (without hicli's `Error: ` prefix).
// The builders are unused outside tests until the commands return errors instead of exiting.
#[allow(dead_code)]
pub fn library_error(err: ClientError) -> color_eyre::Report {
    library_message(err).into_report()
}

/// The message for a source error (without hicli's `Error: ` prefix).
#[allow(dead_code)]
pub fn source_error(err: &dyn std::fmt::Display) -> color_eyre::Report {
    Message::prefixed(err.to_string()).into_report()
}

/// The message for a playback client error (without hicli's `Error: ` prefix).
#[allow(dead_code)]
pub fn playback_error(err: PlaybackClientError) -> color_eyre::Report {
    playback_message(err).into_report()
}

/// Handle library client errors uniformly
pub fn handle_error(err: ClientError) -> ! {
    eprintln!("{}", library_message(err).printed());
    std::process::exit(1);
}

/// Handle source errors uniformly
pub fn handle_source_error(err: &dyn std::fmt::Display) -> ! {
    eprintln!("{}", Message::prefixed(err.to_string()).printed());
    std::process::exit(1);
}

pub fn handle_playback_error(err: PlaybackClientError) -> ! {
    eprintln!("{}", playback_message(err).printed());
    std::process::exit(1);
}

#[cfg(test)]
mod tests {

    use crate::test_support::*;

    use library_ipc_client::TrackInfo;

    use crate::output::terminal_width;

    use track_formatter::render_for_user;

    use crate::output::render_track_table_inner;
    use pretty_assertions::assert_eq;

    use track_formatter::ALL_COLUMNS;

    fn make_track(artist: &str, title: &str, duration_secs: u32) -> TrackInfo {
        use library_ipc_client::DurationSeconds;
        TrackInfo {
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            duration: Some(DurationSeconds::new(duration_secs)),
            ..track("sha256:aa000001")
        }
    }

    /// Build the same 8-track set used in queue_display.feature.
    fn queue_display_tracks() -> Vec<TrackInfo> {
        vec![
            make_track("Sunju Hargun", "Silverhaze (DJ MARIA. Remix)", 421),
            make_track("Carbon Based Lifeforms", "Init", 508),
            make_track("Sunju Hargun", "Right Where It Ends", 376),
            make_track("Carbon Based Lifeforms", "Marsa (2026 Remaster)", 612),
            make_track("Sunju Hargun", "Interloper", 293),
            make_track("Carbon Based Lifeforms", "Midnight Traffic Remix", 444),
            make_track("Sunju Hargun", "Polyrytmi", 367),
            make_track("Carbon Based Lifeforms", "20 Minutes", 1200),
        ]
    }

    #[test]
    fn render_track_table_with_no_prefix_fits_in_terminal_width() {
        let tracks = queue_display_tracks();
        // Use render_track_table_inner with an explicit width to avoid race conditions
        // from parallel tests sharing the $COLUMNS env var.
        let lines = render_track_table_inner(&tracks, 0, 65);
        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let width = stripped.chars().count();
            assert!(
                width <= 65,
                "Line {} is {} chars wide, exceeds 65 columns: '{}'",
                i + 1,
                width,
                stripped
            );
        }
    }

    #[test]
    fn render_track_table_with_prefix_4_fits_in_terminal_width_65() {
        let tracks = queue_display_tracks();
        // prefix=4, term_width=65. Every rendered line must be ≤ 65-4=61 chars.
        let lines = render_track_table_inner(&tracks, 4, 65);
        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let width = stripped.chars().count();
            assert!(
                width + 4 <= 65,
                "Line {} content is {} chars, total with prefix {} exceeds 65 columns: '{}'",
                i + 1,
                width,
                width + 4,
                stripped
            );
        }
    }

    #[test]
    fn render_track_table_with_prefix_4_fits_in_terminal_width_55() {
        let tracks = queue_display_tracks();
        // prefix=4, term_width=55. Every rendered line must be ≤ 55-4=51 chars.
        let lines = render_track_table_inner(&tracks, 4, 55);
        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let width = stripped.chars().count();
            assert!(
                width + 4 <= 55,
                "Line {} content is {} chars, total with prefix {} exceeds 55 columns: '{}'",
                i + 1,
                width,
                width + 4,
                stripped
            );
        }
    }

    // Mutex to serialize tests that mutate the $COLUMNS environment variable.
    // std::env::set_var / remove_var are not thread-safe across parallel tests,
    // so both tests acquire this lock before touching the env.
    static COLUMNS_ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn columns_env_lock() -> &'static std::sync::Mutex<()> {
        COLUMNS_ENV_LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn terminal_width_reads_columns_env_var() {
        // When $COLUMNS is set to a valid integer, terminal_width() must return it.
        // Hold the lock for the entire test to avoid racing with the fallback test.
        let _guard = columns_env_lock().lock().unwrap();
        std::env::set_var("COLUMNS", "75");
        let w = terminal_width();
        std::env::remove_var("COLUMNS");
        assert_eq!(
            w, 75,
            "terminal_width() should return the value of $COLUMNS"
        );
    }

    #[test]
    fn terminal_width_fallback_is_80_not_100() {
        // When $COLUMNS is absent and there is no real TTY (CI / piped),
        // terminal_width() must fall back to 80, not 100.
        let _guard = columns_env_lock().lock().unwrap();
        std::env::remove_var("COLUMNS");
        let w = terminal_width();
        // In a CI environment there is no TTY, so terminal_size() returns None
        // and we must fall back to 80.  If there happens to be a TTY (rare in
        // tests) the result will be the actual terminal width, which is fine —
        // we only care that 100 is never returned as the hard-coded default.
        assert_ne!(w, 100, "terminal_width() must not fall back to 100");
    }

    // ── error messages as values ────────────────────────────────────────────────

    use super::{library_error, playback_error, source_error};
    use super::{library_message, playback_message};
    use library_ipc_client::{ClientError, ProtocolError};
    use mdma_client::PlaybackClientError;
    use nng_transport::{ConnectionError, NngClientError};

    fn lib_text(err: ClientError) -> String {
        failure_text::<()>(Err(library_error(err)))
    }

    fn lib_printed(err: ClientError) -> String {
        library_message(err).printed()
    }

    fn connection_failed() -> ConnectionError {
        ConnectionError::ConnectionFailed("refused".to_string())
    }

    #[test]
    fn library_track_not_found_text() {
        let err = || {
            ClientError::Protocol(ProtocolError::TrackNotFound {
                hash: "sha256:aa".to_string(),
            })
        };
        assert_eq!(lib_text(err()), "Track not found: sha256:aa");
        assert_eq!(lib_printed(err()), "Track not found: sha256:aa");
    }

    #[test]
    fn library_inbox_file_not_found_text() {
        let err = || {
            ClientError::Protocol(ProtocolError::InboxFileNotFound {
                path: "a/b.mp3".to_string(),
            })
        };
        assert_eq!(lib_text(err()), "Inbox file not found: a/b.mp3");
        assert_eq!(lib_printed(err()), "Inbox file not found: a/b.mp3");
    }

    #[test]
    fn library_protocol_text_is_prefixed_when_printed() {
        let err = || {
            ClientError::Protocol(ProtocolError::PlaylistNotFound {
                name: "mix".to_string(),
            })
        };
        assert_eq!(lib_text(err()), "playlist not found: mix");
        assert_eq!(lib_printed(err()), "Error: playlist not found: mix");
    }

    #[test]
    fn library_connection_text_has_two_lines() {
        let err = || ClientError::Transport(NngClientError::Connection(connection_failed()));
        let text = "Connection failed: Connection failed: refused\nIs mdma-library running?";
        assert_eq!(lib_text(err()), text);
        assert_eq!(lib_printed(err()), text);
    }

    #[test]
    fn library_fallback_text_is_prefixed_when_printed() {
        let err = || ClientError::Transport(NngClientError::Service("boom".to_string()));
        assert_eq!(lib_text(err()), "service error: boom");
        assert_eq!(lib_printed(err()), "Error: service error: boom");
    }

    #[test]
    fn playback_connection_text_has_two_lines() {
        let err = || PlaybackClientError::Connection(connection_failed());
        let text = "Connection failed: Connection failed: refused\nIs mdma-playback running?";
        assert_eq!(failure_text::<()>(Err(playback_error(err()))), text);
        assert_eq!(playback_message(err()).printed(), text);
    }

    #[test]
    fn playback_fallback_text_is_prefixed_when_printed() {
        let err = || PlaybackClientError::Service("boom".to_string());
        assert_eq!(
            failure_text::<()>(Err(playback_error(err()))),
            "service error: boom"
        );
        assert_eq!(
            playback_message(err()).printed(),
            "Error: service error: boom"
        );
    }

    #[test]
    fn source_text_is_prefixed_when_printed() {
        assert_eq!(
            failure_text::<()>(Err(source_error(&"no such source"))),
            "no such source"
        );
        assert_eq!(
            super::Message::prefixed("no such source".to_string()).printed(),
            "Error: no such source"
        );
    }

    // ── mdma search output stability: render_track_table_inner unchanged ────────

    #[test]
    fn search_render_unchanged_from_all_columns() {
        // render_track_table_inner must produce identical output to
        // render_for_user with ALL_COLUMNS.
        // This locks the promise that mdma search output is byte-identical after the refactor.
        let tracks = queue_display_tracks();
        let term_width = 120;

        let via_inner = render_track_table_inner(&tracks, 0, term_width);
        let via_parameterized = render_for_user(&tracks, ALL_COLUMNS, term_width, 0);

        assert_eq!(
            via_inner,
            via_parameterized,
            "render_track_table_inner and render_for_user(ALL_COLUMNS) must produce identical output"
        );
    }

    // ── Non-TTY (pipe) rendering: hash is always exactly 12 chars ────────────────

    /// The PIPE_TERM_WIDTH constant in the component must be wide enough that
    /// corsett never shrinks the 12-char hash column.
    #[test]
    fn pipe_mode_hash_is_exactly_12_chars_on_every_line() {
        // Use a very long artist/title to stress-test that the virtual width is
        // wide enough to keep hash at full 12 chars even under content pressure.
        use library_ipc_client::DurationSeconds;
        let tracks: Vec<TrackInfo> = (0..5)
            .map(|i| {
                TrackInfo {
                    title: Some("A Very Long Title That Could Put Pressure On Column Sizing Algorithms And Cause Issues".to_string()),
                    artist: Some("An Equally Very Long Artist Name That Stresses The Layout Engine Significantly".to_string()),
                    duration: Some(DurationSeconds::new(300)),
                    ..track(&format!("sha256:abcdef{:02}deadbeef", i))
                }
            })
            .collect();

        // PIPE_TERM_WIDTH is a large virtual width used when rendering to non-TTY.
        let lines = render_for_user(&tracks, ALL_COLUMNS, super::PIPE_TERM_WIDTH, 0);

        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            // First whitespace-separated token is the hash
            let hash_token = stripped
                .split_whitespace()
                .next()
                .expect("line should have at least one token");
            assert_eq!(
                hash_token.len(),
                12,
                "Line {}: hash token {:?} should be exactly 12 chars (uniqueness invariant). Full line: {:?}",
                i + 1,
                hash_token,
                stripped,
            );
        }
    }

    /// Pipe mode with virtual large width must not truncate artist or title.
    #[test]
    fn pipe_mode_large_virtual_width_does_not_truncate_content() {
        use library_ipc_client::DurationSeconds;
        let long_artist = "An Equally Very Long Artist Name That Stresses The Layout Engine";
        let long_title = "A Very Long Title That Could Put Pressure On Column Sizing Algorithms";
        let track = TrackInfo {
            title: Some(long_title.to_string()),
            artist: Some(long_artist.to_string()),
            duration: Some(DurationSeconds::new(300)),
            ..track("sha256:abcdef1234567890")
        };

        let lines = render_for_user(&[track], ALL_COLUMNS, super::PIPE_TERM_WIDTH, 0);

        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains(long_artist),
            "pipe mode output should not truncate long artist; got: {:?}",
            stripped
        );
        assert!(
            stripped.contains(long_title),
            "pipe mode output should not truncate long title; got: {:?}",
            stripped
        );
    }
}
