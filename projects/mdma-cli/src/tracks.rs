use crate::ctx::Ctx;
use crate::output::{
    format_track_line, handle_error, handle_playback_error, print_tracks, terminal_width,
    PIPE_TERM_WIDTH,
};
use crate::stdin::{hashes_arg_or_stdin, parse_hash_from_line, resolve_tracks_skip_errors};
use crate::Cli;
use clap::CommandFactory;
use color_eyre::Result;
use library_ipc_client::{ContentHash, TrackInfo};
use mdma_client::{LibraryBackend, PlaybackBackend};
use track_formatter::{render_for_user, short_hash, ViewColumn};

#[derive(clap::Args, Debug)]
pub(crate) struct ListArgs {
    /// Maximum number of tracks to show
    #[arg(short, long)]
    limit: Option<usize>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct GetArgs {
    /// Content hash (full or partial, with or without sha256: prefix)
    hash: String,
}

#[derive(clap::Args, Debug)]
pub(crate) struct FactsArgs {
    /// Content hash (full or partial, with or without sha256: prefix)
    hash: String,
}

#[derive(clap::Args, Debug)]
pub(crate) struct SortArgs {
    /// Field to sort by
    #[arg(value_enum)]
    field: SortField,

    /// Sort ascending (A→Z, low→high)
    #[arg(short = 'a')]
    ascending: bool,

    /// Sort descending (Z→A, high→low)
    #[arg(short = 'd')]
    descending: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct BookmarkArgs {
    /// Content hash of track to bookmark (defaults to now-playing if omitted)
    hash: Option<String>,
    /// Optional scope — associate bookmark with a named set
    #[arg(long)]
    scope: Option<String>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct ViewArgs {
    /// Show the 12-character content hash
    #[arg(long)]
    hash: bool,

    /// Show the artist name
    #[arg(long)]
    artist: bool,

    /// Show the track title
    #[arg(long)]
    title: bool,

    /// Show the track duration
    #[arg(long)]
    duration: bool,
}

pub(crate) fn ping(ctx: &Ctx) -> Result<()> {
    let client = ctx.library();
    handle_ping(&client)
}

pub(crate) fn status(ctx: &Ctx) -> Result<()> {
    let client = ctx.library();
    handle_status(&client)
}

pub(crate) fn list(args: ListArgs, ctx: &Ctx) -> Result<()> {
    let ListArgs { limit } = &args;
    let client = ctx.library();
    handle_list(&client, *limit)
}

pub(crate) fn get(args: GetArgs, ctx: &Ctx) -> Result<()> {
    let GetArgs { hash } = &args;
    let client = ctx.library();
    handle_get(&client, hash.clone())
}

pub(crate) fn facts(args: FactsArgs, ctx: &Ctx) -> Result<()> {
    let FactsArgs { hash } = &args;
    let client = ctx.library();
    handle_facts(&client, hash.clone())
}

pub(crate) fn sort(args: SortArgs, ctx: &Ctx) -> Result<()> {
    let SortArgs {
        field,
        ascending,
        descending,
    } = &args;
    let client = ctx.library();
    handle_sort(&client, field.clone(), *ascending, *descending)
}

pub(crate) fn bookmark(args: BookmarkArgs, ctx: &Ctx) -> Result<()> {
    let BookmarkArgs { hash, scope } = &args;
    let lib = ctx.library();
    let pb = ctx.playback();
    handle_bookmark(&lib, Some(&pb), hash.clone(), scope.clone())
}

pub(crate) fn bookmarks(ctx: &Ctx) -> Result<()> {
    let lib = ctx.library();
    handle_bookmarks(&lib)
}

pub(crate) fn view(_args: ViewArgs, ctx: &Ctx) -> Result<()> {
    // Re-parse argv via ArgMatches to recover the order the flags appeared.
    // This implements Option A from the field-order spec: use indices_of on
    // the raw matches for the `view` subcommand to sort boolean flags by
    // their argv position.
    let matches = Cli::command().get_matches();
    let view_matches = matches.subcommand_matches("view").unwrap_or_else(|| {
        eprintln!("internal error: expected 'view' subcommand matches");
        std::process::exit(1);
    });
    let columns = view_columns_from_matches(view_matches);
    let lib = ctx.library();
    handle_view(&lib, columns)
}

#[derive(clap::ValueEnum, Debug, Clone)]
pub(crate) enum SortField {
    Bpm,
    Title,
    Artist,
    Album,
    Duration,
    TrackNumber,
    DiscNumber,
    Added,
    /// Last started (played) datetime. None treated as -∞: ascending puts never-played first,
    /// descending puts never-played last.
    Started,
    /// Last stopped datetime. None treated as -∞: ascending puts never-stopped first,
    /// descending puts never-stopped last.
    Stopped,
}

// =============================================================================
// Command Handlers
// =============================================================================

fn handle_ping(client: &LibraryBackend) -> Result<()> {
    match client.ping() {
        Ok(()) => {
            println!("pong - service is alive");
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_status(client: &LibraryBackend) -> Result<()> {
    match client.status() {
        Ok(status) => {
            println!("MDMA Library Service v{}", status.version);
            println!("{}", "=".repeat(35));
            println!("Tracks indexed:  {}", status.tracks_indexed);
            println!("Facts count:     {}", status.facts_count);
            println!("Inbox queue:     {} files", status.inbox_queue_size);
            println!("Uptime:          {} seconds", status.uptime_seconds);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_list(client: &LibraryBackend, limit: Option<usize>) -> Result<()> {
    match client.list_tracks(limit) {
        Ok(tracks) => {
            if tracks.is_empty() {
                println!("No tracks in library");
                return Ok(());
            }

            println!("Tracks in library ({}):", tracks.len());
            println!("{}", "=".repeat(65));

            for track in tracks {
                println!(
                    "{} | {}",
                    short_hash(&track.content_hash),
                    format_track_line(&track)
                );
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_get(client: &LibraryBackend, hash: String) -> Result<()> {
    let content_hash = ContentHash::new(hash);
    match client.get_track(&content_hash) {
        Ok(track) => {
            println!("Track: {}", track.content_hash.as_str());
            println!("{}", "=".repeat(65));
            if let Some(title) = track.title {
                println!("Title:    {}", title);
            }
            if let Some(artist) = track.artist {
                println!("Artist:   {}", artist);
            }
            if let Some(album) = track.album {
                println!("Album:    {}", album);
            }
            if let Some(duration) = track.duration {
                println!("Duration: {}", duration);
            }
            if let Some(bpm) = track.bpm {
                println!("BPM:      {}", bpm);
            }
            if let Some(key) = track.key {
                println!("Key:      {}", key);
            }
            if let Some(role) = track.role {
                println!("Role:     {}", role);
            }
            if let Some(energy) = track.energy {
                println!("Energy:   {}", energy);
            }
            if let Some(path) = track.blob_path {
                println!("Path:     {}", path);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_facts(client: &LibraryBackend, hash: String) -> Result<()> {
    let content_hash = ContentHash::new(hash);
    match client.get_facts(&content_hash) {
        Ok((full_hash, facts)) => {
            println!("Facts for: {}", full_hash.as_str());
            println!("{}", "=".repeat(65));
            for (fact_type, fact_value) in facts {
                println!("{:20} | {}", fact_type, fact_value);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

/// Resolve the ordered set of `ViewColumn`s from the raw `clap::ArgMatches` for the `view`
/// subcommand.
///
/// Clap boolean flags (`--hash`, `--artist`, `--title`, `--duration`) don't preserve
/// insertion order in the derive API. We use `ArgMatches::indices_of` to recover the argv
/// position of each flag that was actually set, then sort by position to honour the order the
/// user typed them in. This implements `mdma view --title --hash` → title-first, hash-second.
fn view_columns_from_matches(matches: &clap::ArgMatches) -> Vec<ViewColumn> {
    // Collect (argv_index, column) pairs for every flag that was set.
    let mut indexed: Vec<(usize, ViewColumn)> = Vec::new();
    for (flag, col) in &[
        ("hash", ViewColumn::Hash),
        ("artist", ViewColumn::Artist),
        ("title", ViewColumn::Title),
        ("duration", ViewColumn::Duration),
    ] {
        if matches.get_flag(flag) {
            // indices_of returns the positions at which the flag appeared in argv.
            // Take the first (and only) position for a boolean flag.
            if let Some(mut idxs) = matches.indices_of(flag) {
                if let Some(idx) = idxs.next() {
                    indexed.push((idx, *col));
                }
            }
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    indexed.into_iter().map(|(_, col)| col).collect()
}

fn handle_view(client: &LibraryBackend, columns: Vec<ViewColumn>) -> Result<()> {
    use std::io::{BufRead, IsTerminal};

    if columns.is_empty() {
        eprintln!("No fields selected. Use --hash, --artist, --title, and/or --duration.");
        std::process::exit(1);
    }

    // Read hashes from stdin (same format as `mdma playlist append` / `mdma search` output).
    let hashes: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    if hashes.is_empty() {
        if std::io::stdout().is_terminal() {
            eprintln!("No hashes on stdin. Pipe `mdma search` output or a playlist.");
        }
        return Ok(());
    }

    // Look up TrackInfo for each hash, preserving order.
    let mut tracks: Vec<TrackInfo> = Vec::with_capacity(hashes.len());
    for hash in &hashes {
        let content_hash = ContentHash::new(hash.clone());
        match client.get_track(&content_hash) {
            Ok(track) => tracks.push(track),
            Err(e) => {
                eprintln!("Warning: could not resolve {}: {}", hash, e);
            }
        }
    }

    if tracks.is_empty() {
        if std::io::stdout().is_terminal() {
            println!("No tracks found");
        }
        return Ok(());
    }

    // Always use the parameterized corsett renderer.
    // TTY → real terminal width (corsett truncates as needed via RightEllipsis).
    // Pipe/file → virtual fixed-large width so the 8-char hash stays intact and
    //             content is not truncated (Joakim's uniqueness hypothesis).
    let term_width = if std::io::stdout().is_terminal() {
        terminal_width()
    } else {
        PIPE_TERM_WIDTH
    };
    for line in render_for_user(&tracks, &columns, term_width, 0) {
        println!("{}", line);
    }

    Ok(())
}

// =============================================================================
// Bookmark Command Handlers
// =============================================================================

fn handle_bookmark(
    library_client: &LibraryBackend,
    playback_client: Option<&PlaybackBackend>,
    hash: Option<String>,
    scope: Option<String>,
) -> Result<()> {
    let content_hash = match hash {
        Some(h) => ContentHash::new(h),
        None => {
            let pb = match playback_client {
                Some(p) => p,
                None => {
                    eprintln!("No hash provided and no playback client available");
                    std::process::exit(1);
                }
            };
            match pb.now_playing() {
                Ok(Some(h)) => h,
                Ok(None) => {
                    eprintln!("Nothing is currently playing");
                    std::process::exit(1);
                }
                Err(e) => handle_playback_error(e),
            }
        }
    };

    match library_client.write_bookmark(&content_hash, scope) {
        Ok(()) => {
            println!("Bookmarked.");
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_bookmarks(library_client: &LibraryBackend) -> Result<()> {
    let tracks = match library_client.list_tracks(None) {
        Ok(t) => t,
        Err(e) => handle_error(e),
    };

    let bookmarked: Vec<TrackInfo> = tracks
        .into_iter()
        .filter(
            |track| match library_client.get_facts(&track.content_hash) {
                Ok((_hash, facts)) => facts
                    .iter()
                    .any(|(fact_type, _value)| fact_type == "Bookmarked"),
                Err(_) => false,
            },
        )
        .collect();

    if bookmarked.is_empty() {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!("No bookmarked tracks");
        }
        return Ok(());
    }

    print_tracks(
        &bookmarked,
        &format!("Bookmarked tracks ({})", bookmarked.len()),
    );
    Ok(())
}

/// Compare two optional values treating `None` as negative-infinity (-∞).
///
/// Semantics:
/// - Ascending  (`asc=true`):  None first, then real values oldest→newest.
/// - Descending (`asc=false`): real values newest→oldest, None last.
///
/// This applies uniformly to Started, Stopped, and Added so that
/// `mdma sort started -d` yields "most recently played first, never-played last".
fn compare_optional<T: Ord>(a: Option<T>, b: Option<T>, asc: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ord = match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less, // None is -∞, smaller than any Some
        (Some(_), None) => Ordering::Greater, // Some is larger than -∞
        (Some(av), Some(bv)) => av.cmp(&bv),
    };
    if asc {
        ord
    } else {
        ord.reverse()
    }
}

fn handle_sort(
    client: &LibraryBackend,
    field: SortField,
    ascending: bool,
    descending: bool,
) -> Result<()> {
    let direction_asc = match (ascending, descending) {
        (true, false) => true,
        (false, true) => false,
        _ => {
            eprintln!("Specify exactly one of -a (ascending) or -d (descending)");
            std::process::exit(1);
        }
    };

    let hashes = hashes_arg_or_stdin(None);
    let mut tracks = resolve_tracks_skip_errors(client, hashes);

    tracks.sort_by(|a, b| match &field {
        SortField::Bpm => compare_optional(a.bpm, b.bpm, direction_asc),
        SortField::Title => compare_optional(
            a.title.as_deref().map(str::to_lowercase),
            b.title.as_deref().map(str::to_lowercase),
            direction_asc,
        ),
        SortField::Artist => compare_optional(
            a.artist.as_deref().map(str::to_lowercase),
            b.artist.as_deref().map(str::to_lowercase),
            direction_asc,
        ),
        SortField::Album => compare_optional(
            a.album.as_deref().map(str::to_lowercase),
            b.album.as_deref().map(str::to_lowercase),
            direction_asc,
        ),
        SortField::Duration => compare_optional(
            a.duration.map(|d| d.value()),
            b.duration.map(|d| d.value()),
            direction_asc,
        ),
        SortField::TrackNumber => compare_optional(a.track_number, b.track_number, direction_asc),
        SortField::DiscNumber => compare_optional(a.disc_number, b.disc_number, direction_asc),
        SortField::Added => compare_optional(
            a.added.as_deref().map(str::to_string),
            b.added.as_deref().map(str::to_string),
            direction_asc,
        ),
        SortField::Started => compare_optional(
            a.started.as_deref().map(str::to_string),
            b.started.as_deref().map(str::to_string),
            direction_asc,
        ),
        SortField::Stopped => compare_optional(
            a.stopped.as_deref().map(str::to_string),
            b.stopped.as_deref().map(str::to_string),
            direction_asc,
        ),
    });

    print_tracks(&tracks, &format!("Sorted ({} tracks)", tracks.len()));
    Ok(())
}

// Adopted by a later chunk (per-command arg types); unused until then.
#[allow(dead_code)]
/// Content hash of a track (full or partial, with or without `sha256:` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackHash(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    use clap::{CommandFactory, Parser};

    use library_ipc_client::TrackInfo;

    use track_formatter::{render_for_user, ViewColumn};

    use crate::Cli;

    use pretty_assertions::assert_eq;

    use crate::Commands;
    use track_formatter::ALL_COLUMNS;

    // ── sort by started / stopped ─────────────────────────────────────────────

    fn make_track_with_started(hash: &str, started: Option<&str>) -> TrackInfo {
        TrackInfo {
            started: started.map(str::to_string),
            ..track(hash)
        }
    }

    fn make_track_with_stopped(hash: &str, stopped: Option<&str>) -> TrackInfo {
        TrackInfo {
            stopped: stopped.map(str::to_string),
            ..track(hash)
        }
    }

    // ── sort None = -∞ semantics ──────────────────────────────────────────────

    #[test]
    fn sort_started_ascending_puts_none_first() {
        let mut tracks = [
            make_track_with_started("sha256:never", None),
            make_track_with_started("sha256:old", Some("2024-01-01T00:00:00Z")),
            make_track_with_started("sha256:recent", Some("2025-06-15T12:00:00Z")),
        ];
        // ascending: None (-∞) first, then oldest → newest
        tracks.sort_by(|a, b| {
            compare_optional(
                a.started.as_deref().map(str::to_string),
                b.started.as_deref().map(str::to_string),
                true,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            ["sha256:never", "sha256:old", "sha256:recent"],
            "ascending: never-played first (None = -∞), then oldest→newest"
        );
    }

    #[test]
    fn sort_started_descending_puts_none_last() {
        let mut tracks = [
            make_track_with_started("sha256:never", None),
            make_track_with_started("sha256:old", Some("2024-01-01T00:00:00Z")),
            make_track_with_started("sha256:recent", Some("2025-06-15T12:00:00Z")),
        ];
        // descending: newest first, None last
        tracks.sort_by(|a, b| {
            compare_optional(
                a.started.as_deref().map(str::to_string),
                b.started.as_deref().map(str::to_string),
                false,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            ["sha256:recent", "sha256:old", "sha256:never"],
            "descending: newest first, never-played last (None = -∞)"
        );
    }

    #[test]
    fn sort_by_stopped_descending_newest_first() {
        let mut tracks = [
            make_track_with_stopped("sha256:never", None),
            make_track_with_stopped("sha256:old", Some("2024-03-01T00:00:00Z")),
            make_track_with_stopped("sha256:recent", Some("2025-11-20T08:00:00Z")),
        ];
        // descending: newest first, None last (None = -∞, reversed → last)
        tracks.sort_by(|a, b| {
            compare_optional(
                a.stopped.as_deref().map(str::to_string),
                b.stopped.as_deref().map(str::to_string),
                false,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            ["sha256:recent", "sha256:old", "sha256:never"],
            "descending: newest first, never-stopped last"
        );
    }

    #[test]
    fn sort_added_respects_none_as_beginning_of_time() {
        let make = |hash: &str, added: Option<&str>| -> TrackInfo {
            TrackInfo {
                added: added.map(str::to_string),
                ..track(hash)
            }
        };
        let mut tracks = [
            make("sha256:unknown", None),
            make("sha256:new", Some("2025-12-01T00:00:00Z")),
            make("sha256:early", Some("2020-01-01T00:00:00Z")),
        ];
        // ascending: None first (beginning of time), then oldest→newest
        tracks.sort_by(|a, b| {
            compare_optional(
                a.added.as_deref().map(str::to_string),
                b.added.as_deref().map(str::to_string),
                true,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            ["sha256:unknown", "sha256:early", "sha256:new"],
            "ascending added: None first, then oldest→newest"
        );
    }

    // ── mdma view: ViewColumn / render_for_user ───────────

    fn make_view_track(
        hash: &str,
        artist: Option<&str>,
        title: Option<&str>,
        duration_secs: Option<u32>,
    ) -> TrackInfo {
        use library_ipc_client::DurationSeconds;
        TrackInfo {
            title: title.map(str::to_string),
            artist: artist.map(str::to_string),
            duration: duration_secs.map(DurationSeconds::new),
            ..track(hash)
        }
    }

    #[test]
    fn view_clap_parses_hash_artist_title_duration() {
        let result = Cli::try_parse_from([
            "mdma",
            "view",
            "--hash",
            "--artist",
            "--title",
            "--duration",
        ]);
        assert!(
            result.is_ok(),
            "clap rejected view --hash --artist --title --duration: {}",
            result.unwrap_err()
        );
        if let Commands::View(ViewArgs {
            hash,
            artist,
            title,
            duration,
        }) = result.unwrap().command
        {
            assert!(hash);
            assert!(artist);
            assert!(title);
            assert!(duration);
        } else {
            panic!("expected Commands::View");
        }
    }

    #[test]
    fn view_clap_parses_subset_of_flags() {
        let result = Cli::try_parse_from(["mdma", "view", "--title", "--hash"]);
        assert!(
            result.is_ok(),
            "clap rejected view --title --hash: {}",
            result.unwrap_err()
        );
        if let Commands::View(ViewArgs {
            hash,
            artist,
            title,
            duration,
        }) = result.unwrap().command
        {
            assert!(title);
            assert!(hash);
            assert!(!artist);
            assert!(!duration);
        } else {
            panic!("expected Commands::View");
        }
    }

    #[test]
    fn view_clap_no_flags_parses_ok() {
        // All flags are optional; no flags is valid (handle_view will reject at runtime).
        let result = Cli::try_parse_from(["mdma", "view"]);
        assert!(
            result.is_ok(),
            "clap rejected view with no flags: {}",
            result.unwrap_err()
        );
    }

    // ── render_for_user: single-column ────────────────────

    #[test]
    fn view_render_hash_only_shows_only_hash() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Title"),
            Some(180),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Hash], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.starts_with("abcdef123456"),
            "hash-only should start with 12-char hash, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Artist"),
            "hash-only should not contain artist, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Title"),
            "hash-only should not contain title, got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_artist_only_shows_only_artist() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Init"),
            Some(508),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Artist], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("Carbon Based Lifeforms"),
            "artist-only should contain artist, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("abcdef123456"),
            "artist-only should not contain hash, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Init"),
            "artist-only should not contain title, got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_title_only_shows_only_title() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("Polyrytmi"),
            "title-only should contain title, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Carbon"),
            "title-only should not contain artist, got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_duration_only_shows_only_duration() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Title"),
            Some(225), // 3:45
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Duration], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("3:45"),
            "duration-only should contain formatted duration, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Artist"),
            "duration-only should not contain artist, got: {stripped:?}"
        );
    }

    // ── render_for_user: multi-column combinations ─────────

    #[test]
    fn view_render_hash_and_title() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Init"),
            Some(508),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Hash, ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("abcdef123456"), "should contain hash");
        assert!(stripped.contains("Init"), "should contain title");
        assert!(!stripped.contains("Artist"), "should not contain artist");
    }

    #[test]
    fn view_render_all_four_columns() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        )];
        let lines = render_for_user(&tracks, ALL_COLUMNS, 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("abcdef123456"), "should contain hash");
        assert!(
            stripped.contains("Carbon Based Lifeforms"),
            "should contain artist"
        );
        assert!(stripped.contains("Polyrytmi"), "should contain title");
        assert!(stripped.contains("6:07"), "should contain duration");
    }

    // ── Field-order preservation ─────────────────────────────────────────────────

    #[test]
    fn view_render_title_before_hash_when_columns_ordered_title_hash() {
        // When columns = [Title, Hash], title must appear left of hash in the output.
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("TheTitle"),
            None,
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Title, ViewColumn::Hash], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let title_pos = stripped.find("TheTitle").expect("title not in output");
        let hash_pos = stripped.find("abcdef123456").expect("hash not in output");
        assert!(
            title_pos < hash_pos,
            "title ({title_pos}) should appear before hash ({hash_pos}) when columns=[Title, Hash]; got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_hash_before_title_when_columns_ordered_hash_title() {
        // When columns = [Hash, Title], hash must appear left of title.
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("TheTitle"),
            None,
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Hash, ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let hash_pos = stripped.find("abcdef123456").expect("hash not in output");
        let title_pos = stripped.find("TheTitle").expect("title not in output");
        assert!(
            hash_pos < title_pos,
            "hash ({hash_pos}) should appear before title ({title_pos}) when columns=[Hash, Title]; got: {stripped:?}"
        );
    }

    // ── view_columns_from_matches ────────────────────────────────────────────────

    #[test]
    fn view_columns_from_matches_hash_artist_order() {
        // Simulate `mdma view --hash --artist` → Hash before Artist.
        let matches = Cli::command().get_matches_from(["mdma", "view", "--hash", "--artist"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(cols, vec![ViewColumn::Hash, ViewColumn::Artist]);
    }

    #[test]
    fn view_columns_from_matches_title_hash_order() {
        // Simulate `mdma view --title --hash` → Title before Hash.
        let matches = Cli::command().get_matches_from(["mdma", "view", "--title", "--hash"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(cols, vec![ViewColumn::Title, ViewColumn::Hash]);
    }

    #[test]
    fn view_columns_from_matches_all_four_preserves_order() {
        // Simulate `mdma view --hash --artist --title --duration` → canonical order.
        let matches = Cli::command().get_matches_from([
            "mdma",
            "view",
            "--hash",
            "--artist",
            "--title",
            "--duration",
        ]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(
            cols,
            vec![
                ViewColumn::Hash,
                ViewColumn::Artist,
                ViewColumn::Title,
                ViewColumn::Duration
            ]
        );
    }

    #[test]
    fn view_columns_from_matches_duration_first() {
        // Simulate `mdma view --duration --title` → Duration before Title.
        let matches = Cli::command().get_matches_from(["mdma", "view", "--duration", "--title"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(cols, vec![ViewColumn::Duration, ViewColumn::Title]);
    }

    #[test]
    fn view_columns_from_matches_no_flags_empty() {
        let matches = Cli::command().get_matches_from(["mdma", "view"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert!(cols.is_empty(), "no flags → empty columns list");
    }
}
