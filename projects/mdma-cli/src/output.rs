//! Output helpers: track formatting and tables, and uniform error reporting.
//! It also interprets service replies into the error texts hicli prints.

use color_eyre::eyre::eyre;
use colored::Colorize;
use gateway_client::AdminResponse;
use library_ipc_client::{ClientError, LibraryResponse, ProtocolError, TrackInfo};
use mdma_client::PlaybackClientError;
use nng_transport::ConnectionError;
use source_protocol::{SourceError, SourceResponse};
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

/// Why connecting to `what` failed, as one layer: `Failed to connect to
/// <what> at <address>: <OS error>` for a failed dial, else `Failed to connect
/// to <what>: <error>`.
pub fn connect_failure(what: &str, err: &ConnectionError) -> String {
    match err {
        ConnectionError::Dial { address, cause } => {
            format!("Failed to connect to {what} at {address}: {cause}")
        }
        other => connect_failure_text(what, other),
    }
}

/// `Failed to connect to <what>: <err>`, for a failure without a dialed address.
pub fn connect_failure_text(what: &str, err: &dyn std::fmt::Display) -> String {
    format!("Failed to connect to {what}: {err}")
}

/// How a source is named in messages: `source 'bandcamp'`.
pub fn source_label(name: &str) -> String {
    format!("source '{name}'")
}

/// The failure for a reply `what` sent that the command did not expect.
pub fn unexpected_response(what: &str) -> color_eyre::Report {
    eyre!("Unexpected response from {what}")
}

/// The failure for a request to `what` that got no reply.
pub fn request_failed(what: &str, err: &dyn std::fmt::Display) -> color_eyre::Report {
    eyre!("Request to {what} failed: {err}")
}

/// The failure for a library client error (without hicli's `Error: ` prefix).
pub fn library_error(err: ClientError) -> color_eyre::Report {
    let text = match err {
        ClientError::Protocol(ProtocolError::TrackNotFound { hash }) => {
            format!("Track not found: {}", hash)
        }
        ClientError::Protocol(ProtocolError::InboxFileNotFound { path }) => {
            format!("Inbox file not found: {}", path)
        }
        ClientError::Protocol(e) => e.to_string(),
        e => e.to_string(),
    };
    eyre!("{}", text)
}

/// The failure for a source error (without hicli's `Error: ` prefix).
pub fn source_error(err: &dyn std::fmt::Display) -> color_eyre::Report {
    eyre!("{}", err)
}

/// The failure for a playback client error (without hicli's `Error: ` prefix).
pub fn playback_error(err: PlaybackClientError) -> color_eyre::Report {
    eyre!("{}", err)
}

/// Interpret an admin reply: `accept` takes the response it wants (`None`
/// declines). An error response, a declined response, or a failed request is
/// the failure.
pub fn interpret_admin<T>(
    reply: Result<AdminResponse, impl std::fmt::Display>,
    accept: impl FnOnce(AdminResponse) -> Option<T>,
) -> color_eyre::Result<T> {
    match reply {
        Ok(AdminResponse::Error { message }) => Err(eyre!("{}", message)),
        Ok(response) => accept(response).ok_or_else(|| unexpected_response("admin")),
        Err(e) => Err(request_failed("admin", &e)),
    }
}

/// Interpret a reply of the source `name`: `accept` takes the response it wants
/// (`None` declines). An error response is turned into the failure by
/// `on_error`; a declined response or a failed request is a failure too.
pub fn interpret_source<T>(
    name: &str,
    reply: Result<SourceResponse, impl std::fmt::Display>,
    on_error: impl FnOnce(SourceError) -> color_eyre::Report,
    accept: impl FnOnce(SourceResponse) -> Option<T>,
) -> color_eyre::Result<T> {
    let what = source_label(name);
    match reply {
        Ok(SourceResponse::Error(e)) => Err(on_error(e)),
        Ok(response) => accept(response).ok_or_else(|| unexpected_response(&what)),
        Err(e) => Err(request_failed(&what, &e)),
    }
}

/// Interpret a library reply: `accept` takes the response it wants (`None`
/// declines). An error response, a declined response, or a failed request is
/// the failure.
pub fn interpret_library<T>(
    reply: Result<LibraryResponse, impl std::fmt::Display>,
    accept: impl FnOnce(LibraryResponse) -> Option<T>,
) -> color_eyre::Result<T> {
    match reply {
        Ok(LibraryResponse::Error(e)) => Err(eyre!("{}", e)),
        Ok(response) => accept(response).ok_or_else(|| unexpected_response("library")),
        Err(e) => Err(request_failed("library", &e)),
    }
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

    use super::{
        connect_failure, interpret_admin, interpret_library, interpret_source, library_error,
        playback_error, request_failed, source_error, source_label, unexpected_response,
    };
    use gateway_client::AdminResponse;
    use library_ipc_client::{ClientError, LibraryResponse, ProtocolError};
    use mdma_client::PlaybackClientError;
    use nng_transport::{ConnectionError, NngClientError};
    use source_protocol::{SourceError, SourceResponse};

    fn lib_text(err: ClientError) -> String {
        failure_text::<()>(Err(library_error(err)))
    }

    fn refused_at(address: &str) -> ConnectionError {
        ConnectionError::Dial {
            address: address.to_string(),
            cause: nng::Error::ConnectionRefused,
        }
    }

    #[test]
    fn library_track_not_found_text() {
        let err = ClientError::Protocol(ProtocolError::TrackNotFound {
            hash: "sha256:aa".to_string(),
        });
        assert_eq!(lib_text(err), "Track not found: sha256:aa");
    }

    #[test]
    fn library_inbox_file_not_found_text() {
        let err = ClientError::Protocol(ProtocolError::InboxFileNotFound {
            path: "a/b.mp3".to_string(),
        });
        assert_eq!(lib_text(err), "Inbox file not found: a/b.mp3");
    }

    #[test]
    fn library_protocol_text_is_the_protocol_message() {
        let err = ClientError::Protocol(ProtocolError::PlaylistNotFound {
            name: "mix".to_string(),
        });
        assert_eq!(lib_text(err), "playlist not found: mix");
    }

    #[test]
    fn library_fallback_text_is_the_error() {
        let err = ClientError::Transport(NngClientError::Service("boom".to_string()));
        assert_eq!(lib_text(err), "service error: boom");
    }

    #[test]
    fn playback_fallback_text_is_the_error() {
        let err = PlaybackClientError::Service("boom".to_string());
        assert_eq!(
            failure_text::<()>(Err(playback_error(err))),
            "service error: boom"
        );
    }

    #[test]
    fn source_text_is_the_error() {
        assert_eq!(
            failure_text::<()>(Err(source_error(&"no such source"))),
            "no such source"
        );
    }

    #[test]
    fn connect_failure_names_what_and_where_once() {
        assert_eq!(
            connect_failure("library", &refused_at("tcp://10.0.0.2:5555")),
            "Failed to connect to library at tcp://10.0.0.2:5555: Connection refused"
        );
    }

    #[test]
    fn connect_failure_without_an_address_keeps_the_cause() {
        assert_eq!(
            connect_failure(
                "library",
                &ConnectionError::DnsResolution("no host".to_string())
            ),
            "Failed to connect to library: DNS resolution failed: no host"
        );
    }

    #[test]
    fn unexpected_response_names_who_sent_it() {
        assert_eq!(
            failure_text::<()>(Err(unexpected_response("library"))),
            "Unexpected response from library"
        );
        assert_eq!(
            failure_text::<()>(Err(unexpected_response(&source_label("bandcamp")))),
            "Unexpected response from source 'bandcamp'"
        );
    }

    #[test]
    fn request_failed_names_who_was_asked_and_why() {
        assert_eq!(
            failure_text::<()>(Err(request_failed("admin", &"timed out"))),
            "Request to admin failed: timed out"
        );
    }

    // ── interpreting a reply ────────────────────────────────────────────────────

    fn accept_ok(response: AdminResponse) -> Option<()> {
        matches!(response, AdminResponse::Ok).then_some(())
    }

    #[test]
    fn an_accepted_admin_response_is_the_result() {
        let reply = Ok::<_, &str>(AdminResponse::Ok);
        assert!(interpret_admin(reply, accept_ok).is_ok());
    }

    #[test]
    fn an_admin_error_response_gives_its_message() {
        let reply = Ok::<_, &str>(AdminResponse::Error {
            message: "boot partition unhealthy".to_string(),
        });
        assert_eq!(
            failure_text(interpret_admin(reply, accept_ok)),
            "boot partition unhealthy"
        );
    }

    #[test]
    fn a_declined_admin_response_is_unexpected() {
        let reply = Ok::<_, &str>(AdminResponse::Pong);
        assert_eq!(
            failure_text(interpret_admin(reply, accept_ok)),
            "Unexpected response from admin"
        );
    }

    #[test]
    fn a_failed_admin_request_says_so() {
        let reply = Err::<AdminResponse, _>("timed out");
        assert_eq!(
            failure_text(interpret_admin(reply, accept_ok)),
            "Request to admin failed: timed out"
        );
    }

    fn paused(response: SourceResponse) -> Option<()> {
        matches!(response, SourceResponse::Paused).then_some(())
    }

    fn interpret(reply: Result<SourceResponse, &str>) -> color_eyre::Result<()> {
        interpret_source("bandcamp", reply, |e| source_error(&e), paused)
    }

    #[test]
    fn an_accepted_source_response_is_the_result() {
        assert!(interpret(Ok(SourceResponse::Paused)).is_ok());
    }

    #[test]
    fn a_source_error_response_gives_its_message() {
        let reply = SourceResponse::Error(SourceError::NotAuthenticated {
            message: "login first".to_string(),
        });
        assert_eq!(
            failure_text(interpret(Ok(reply))),
            "not authenticated: login first"
        );
    }

    #[test]
    fn a_declined_source_response_names_the_source() {
        assert_eq!(
            failure_text(interpret(Ok(SourceResponse::Resumed))),
            "Unexpected response from source 'bandcamp'"
        );
    }

    #[test]
    fn a_failed_source_request_names_the_source() {
        assert_eq!(
            failure_text(interpret(Err("socket closed"))),
            "Request to source 'bandcamp' failed: socket closed"
        );
    }

    #[test]
    fn on_error_decides_how_a_source_error_response_is_reported() {
        let reply = SourceResponse::Error(SourceError::Internal {
            message: "boom".to_string(),
        });
        let result = interpret_source(
            "bandcamp",
            Ok::<_, &str>(reply),
            |_| color_eyre::eyre::eyre!("custom"),
            paused,
        );
        assert_eq!(failure_text(result), "custom");
    }

    fn playlist_content(response: LibraryResponse) -> Option<String> {
        match response {
            LibraryResponse::PlaylistContent(c) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn interpret_library_gives_the_accepted_response() {
        let reply = Ok::<_, &str>(LibraryResponse::PlaylistContent("a\nb\n".to_string()));
        assert_eq!(
            interpret_library(reply, playlist_content).unwrap(),
            "a\nb\n"
        );
    }

    #[test]
    fn interpret_library_reports_an_error_response_as_its_message() {
        let reply = Ok::<_, &str>(LibraryResponse::Error(ProtocolError::PlaylistNotFound {
            name: "mix".to_string(),
        }));
        assert_eq!(
            failure_text(interpret_library(reply, playlist_content)),
            "playlist not found: mix"
        );
    }

    #[test]
    fn interpret_library_reports_a_declined_response() {
        let reply = Ok::<_, &str>(LibraryResponse::Pong);
        assert_eq!(
            failure_text(interpret_library(reply, playlist_content)),
            "Unexpected response from library"
        );
    }

    #[test]
    fn interpret_library_reports_a_failed_request() {
        let reply = Err::<LibraryResponse, _>("socket closed");
        assert_eq!(
            failure_text(interpret_library(reply, playlist_content)),
            "Request to library failed: socket closed"
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
