use crate::args::{parse_count, value_and_dash, HashInput, StdinDash};
use crate::ctx::Ctx;
use crate::output::{
    each_track, for_each_track, format_track_line, library_error, mark_outcome, marked_header,
    no_marked_tracks, playing_note, print_tracks, terminal_width, unmark_outcome, PIPE_TERM_WIDTH,
};
use crate::stdin::{
    hashes_from_dash_or_playing, hashes_of_input, read_hashes, require_stdin_hashes,
    resolve_tracks_skip_errors,
};
use bpaf::{construct, long, short, Parser};
use color_eyre::Result;
use hicli::{Arg, Choice};
use library_ipc_client::{ContentHash, TrackInfo};
use mdma_client::{Mark, MarkFilter, MarkNameError};
use std::num::NonZeroUsize;
use track_formatter::{render_for_user, short_hash, ViewColumn};

/// Maximum number of tracks to show
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "limit", short = 'l', metavar = "LIMIT")]
pub(crate) struct LimitArg(pub NonZeroUsize);

impl std::str::FromStr for LimitArg {
    type Err = &'static str;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_count(s).map(Self)
    }
}

/// Mark name (`liked` is the default mark)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "NAME")]
pub(crate) struct MarkArg(pub Mark);

impl std::str::FromStr for MarkArg {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "-" {
            return Err("`-` reads hashes from stdin; it is not a mark name".to_string());
        }
        s.parse()
            .map(Self)
            .map_err(|e: MarkNameError| e.to_string())
    }
}

/// The mark `mark` and `unmark` act with, and whether to read the tracks from stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkInput {
    pub(crate) name: Option<MarkArg>,
    pub(crate) stdin: Option<StdinDash>,
}

impl Arg for MarkInput {
    /// `[NAME] [-]`: the mark (the default mark when left out), and `-` to read the
    /// tracks' hashes from stdin instead of acting on the playing track.
    fn parser() -> impl Parser<Self> {
        value_and_dash::<MarkArg>(
            "NAME",
            "[NAME] [-]",
            "Mark name (`liked` is the default mark), and/or `-` to read track hashes from stdin; \
             at most one of each",
        )
        .map(|(name, stdin)| Self { name, stdin })
    }
}

/// The mark to apply: the one named, else the default mark.
fn mark_or_default(name: Option<MarkArg>) -> Mark {
    name.map_or(Mark::Default, |MarkArg(mark)| mark)
}

/// The marks to list: the one named, else any.
fn mark_filter(name: Option<MarkArg>) -> MarkFilter {
    name.map_or(MarkFilter::Any, |MarkArg(mark)| MarkFilter::Only(mark))
}

/// The columns `mdma view` shows, in the order their flags were typed; at least one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewColumns(pub Vec<ViewColumn>);

/// One `view` column flag.
fn column_flag(
    name: &'static str,
    help: &'static str,
    column: ViewColumn,
) -> impl Parser<ViewColumn> {
    long(name).help(help).req_flag(column)
}

impl Arg for ViewColumns {
    /// One or more column flags; the columns come in the order the flags were
    /// typed, and a flag given twice is an error.
    fn parser() -> impl Parser<Self> {
        let hash = column_flag(
            "hash",
            "Show the 12-character content hash",
            ViewColumn::Hash,
        );
        let artist = column_flag("artist", "Show the artist name", ViewColumn::Artist);
        let title = column_flag("title", "Show the track title", ViewColumn::Title);
        let duration = column_flag("duration", "Show the track duration", ViewColumn::Duration);
        construct!([hash, artist, title, duration])
            .some("No fields selected. Use --hash, --artist, --title, and/or --duration.")
            .parse(|columns| {
                match columns
                    .iter()
                    .enumerate()
                    .find(|(i, column)| columns[..*i].contains(column))
                {
                    Some((_, column)) => Err(format!("{column:?} column given more than once")),
                    None => Ok(Self(columns)),
                }
            })
    }
}

impl Arg for SortDirection {
    /// Exactly one of `-a` / `-d`.
    fn parser() -> impl Parser<Self> {
        let ascending = short('a')
            .help("Sort ascending (A→Z, low→high)")
            .req_flag(Self::Ascending);
        let descending = short('d')
            .help("Sort descending (Z→A, high→low)")
            .req_flag(Self::Descending);
        construct!([ascending, descending])
    }
}

/// Field to sort by
#[derive(Debug, Clone, Copy, PartialEq, Eq, Choice, Arg)]
#[arg(positional = "FIELD")]
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

/// Direction of a sort, built from the mutually exclusive `-a` / `-d` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    fn is_ascending(self) -> bool {
        self == SortDirection::Ascending
    }
}

/// Check if the service is running
#[hicli::command]
pub(crate) fn ping(ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    library.ping().map_err(library_error)?;
    println!("pong - service is alive");
    Ok(())
}

/// Get service status
#[hicli::command]
pub(crate) fn status(ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let status = library.status().map_err(library_error)?;
    print!("{}", format_status(&status));
    Ok(())
}

/// Render the library status block. The unparseable-lines row only appears
/// when non-zero, so a healthy library keeps the familiar layout.
fn format_status(status: &library_ipc_client::ServiceStatus) -> String {
    let mut text = format!(
        "MDMA Library Service v{}\n{}\n\
         Tracks indexed:  {}\n\
         Facts count:     {}\n\
         Inbox queue:     {} files\n\
         Uptime:          {} seconds\n",
        status.version,
        "=".repeat(35),
        status.tracks_indexed,
        status.facts_count,
        status.inbox_queue_size,
        status.uptime_seconds,
    );
    if status.unparseable_lines > 0 {
        text.push_str(&format!(
            "Unparseable:     {} fact-log lines skipped\n",
            status.unparseable_lines
        ));
    }
    text
}

/// List tracks in library
#[hicli::command]
pub(crate) fn list(limit: Option<LimitArg>, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let tracks = library
        .list_tracks(limit.map(|LimitArg(n)| n.get()))
        .map_err(library_error)?;
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

/// Get a specific track by hash (supports partial hashes like git)
///
/// Give `-` to read hashes from stdin and show each track in turn. A hash that can't
/// be found is reported on stderr, the rest are still shown, and the exit code is 1.
///
/// # Examples
///
/// ```sh
/// mdma get abcd1234
/// mdma search --artist CBL | mdma get -
/// ```
#[hicli::command]
pub(crate) fn get(hash: HashInput, ctx: &Ctx) -> Result<hicli::Success> {
    let library = ctx.library()?;
    let hashes = hashes_of_input(hash)?;
    // A track that can't be found is reported and the rest are still shown.
    each_track(hashes, |content_hash| {
        library.get_track(content_hash).map(print_track_details)
    })
}

/// Print every known detail of `track`.
fn print_track_details(track: TrackInfo) {
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
}

/// Show all facts for a track
///
/// Give `-` to read hashes from stdin and show the facts of each track in turn. A hash
/// that can't be found is reported on stderr, the rest are still shown, and the exit
/// code is 1.
///
/// # Examples
///
/// ```sh
/// mdma facts abcd1234
/// mdma search --artist CBL | mdma facts -
/// ```
#[hicli::command]
pub(crate) fn facts(hash: HashInput, ctx: &Ctx) -> Result<hicli::Success> {
    let library = ctx.library()?;
    let hashes = hashes_of_input(hash)?;
    each_track(hashes, |content_hash| {
        library.get_facts(content_hash).map(|(full_hash, facts)| {
            println!("Facts for: {}", full_hash.as_str());
            println!("{}", "=".repeat(65));
            for (fact_type, fact_value) in facts {
                println!("{:20} | {}", fact_type, fact_value);
            }
        })
    })
}

/// Compare two optional values treating `None` as negative-infinity (-∞).
///
/// Semantics:
/// - Ascending:  None first, then real values oldest→newest.
/// - Descending: real values newest→oldest, None last.
///
/// This applies uniformly to Started, Stopped, and Added so that
/// `mdma sort started -d` yields "most recently played first, never-played last".
fn compare_optional<T: Ord>(
    a: Option<T>,
    b: Option<T>,
    direction: SortDirection,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ord = match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less, // None is -∞, smaller than any Some
        (Some(_), None) => Ordering::Greater, // Some is larger than -∞
        (Some(av), Some(bv)) => av.cmp(&bv),
    };
    if direction.is_ascending() {
        ord
    } else {
        ord.reverse()
    }
}

/// Sort hashes from stdin by a track metadata field
///
/// Give `-` to read one hash per line from stdin; outputs sorted hashes.
/// Stable sort: chain multiple invocations for multi-key sort (right-to-left priority).
/// For started and stopped, a missing value counts as -inf: ascending puts never-played
/// first, descending puts never-played last.
///
/// # Examples
///
/// ```sh
/// mdma search --artist=CBL | mdma sort title -a -
/// mdma queue list | mdma sort bpm -d - | mdma queue append -
/// cat friday.plist | mdma sort title -a - | mdma sort artist -a - > sorted.plist
/// ```
#[hicli::command]
pub(crate) fn sort(
    direction: SortDirection,
    field: SortField,
    _stdin: StdinDash,
    ctx: &Ctx,
) -> Result<()> {
    let library = ctx.library()?;

    let hashes = require_stdin_hashes()?;
    let mut tracks = resolve_tracks_skip_errors(&library, hashes);

    tracks.sort_by(|a, b| match field {
        SortField::Bpm => compare_optional(a.bpm, b.bpm, direction),
        SortField::Title => compare_optional(
            a.title.as_deref().map(str::to_lowercase),
            b.title.as_deref().map(str::to_lowercase),
            direction,
        ),
        SortField::Artist => compare_optional(
            a.artist.as_deref().map(str::to_lowercase),
            b.artist.as_deref().map(str::to_lowercase),
            direction,
        ),
        SortField::Album => compare_optional(
            a.album.as_deref().map(str::to_lowercase),
            b.album.as_deref().map(str::to_lowercase),
            direction,
        ),
        SortField::Duration => compare_optional(
            a.duration.map(|d| d.value()),
            b.duration.map(|d| d.value()),
            direction,
        ),
        SortField::TrackNumber => compare_optional(a.track_number, b.track_number, direction),
        SortField::DiscNumber => compare_optional(a.disc_number, b.disc_number, direction),
        SortField::Added => compare_optional(
            a.added.as_deref().map(str::to_string),
            b.added.as_deref().map(str::to_string),
            direction,
        ),
        SortField::Started => compare_optional(
            a.started.as_deref().map(str::to_string),
            b.started.as_deref().map(str::to_string),
            direction,
        ),
        SortField::Stopped => compare_optional(
            a.stopped.as_deref().map(str::to_string),
            b.stopped.as_deref().map(str::to_string),
            direction,
        ),
    });

    print_tracks(&tracks, &format!("Sorted ({} tracks)", tracks.len()));
    Ok(())
}

/// Mark tracks (liked unless NAME is given)
///
/// Marks the playing track; with `-`, marks the tracks whose hashes are piped on
/// stdin instead (stdin is read only for `-`; none on it is an error). A track that
/// already carries the mark is left as it is. Each output line starts with the
/// track hash, so the output can be piped on. Without `-` the line ends with
/// "(playing track: Artist – Title)", so a forgotten `-` in a pipeline shows.
///
/// # Examples
///
/// ```sh
/// mdma mark
/// mdma search --bpm 120..130 | mdma mark -
/// mdma search --artist CBL | mdma mark banger -
/// ```
#[hicli::command(alias = "bookmark")]
pub(crate) fn mark(input: MarkInput, ctx: &Ctx) -> Result<hicli::Success> {
    let mark = mark_or_default(input.name);
    let library = ctx.library()?;
    let playing = input.stdin.is_none();
    let hashes = hashes_from_dash_or_playing(ctx, input.stdin)?;
    for_each_track(hashes, |hash| {
        library
            .write_mark(hash, &mark)
            .map(|outcome| mark_outcome(&mark, outcome) + &note_if_playing(&library, hash, playing))
    })
}

/// Remove a mark from tracks (liked unless NAME is given)
///
/// Unmarks the playing track; with `-`, unmarks the tracks whose hashes are piped
/// on stdin instead (stdin is read only for `-`; none on it is an error). A track
/// without the mark is left as it is. Without `-` the output line ends with
/// "(playing track: Artist – Title)", so a forgotten `-` in a pipeline shows.
///
/// # Examples
///
/// ```sh
/// mdma unmark
/// mdma marked banger | mdma unmark banger -
/// ```
#[hicli::command]
pub(crate) fn unmark(input: MarkInput, ctx: &Ctx) -> Result<hicli::Success> {
    let mark = mark_or_default(input.name);
    let library = ctx.library()?;
    let playing = input.stdin.is_none();
    let hashes = hashes_from_dash_or_playing(ctx, input.stdin)?;
    for_each_track(hashes, |hash| {
        library.unmark(hash, &mark).map(|outcome| {
            unmark_outcome(&mark, outcome) + &note_if_playing(&library, hash, playing)
        })
    })
}

/// [`playing_note`] for `hash` when the command acted on the playing track, else nothing.
fn note_if_playing(
    library: &mdma_client::LibraryBackend,
    hash: &ContentHash,
    playing: bool,
) -> String {
    if !playing {
        return String::new();
    }
    playing_note(library.get_track(hash).ok().as_ref())
}

/// List marked tracks
///
/// Lists the tracks that carry the mark NAME, or any mark when NAME is left out;
/// `mdma marked liked` lists only the default mark.
///
/// # Arguments
///
/// * `name` - Only list tracks with this mark (any mark when left out; `liked` is the default mark)
///
/// # Examples
///
/// ```sh
/// mdma marked
/// mdma marked banger | mdma playlist new bangers
/// ```
#[hicli::command(alias = "bookmarks")]
pub(crate) fn marked(name: Option<MarkArg>, ctx: &Ctx) -> Result<()> {
    let filter = mark_filter(name);
    let library = ctx.library()?;
    let tracks = library.list_marked(&filter).map_err(library_error)?;

    if tracks.is_empty() {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!("{}", no_marked_tracks(&filter));
        }
        return Ok(());
    }

    print_tracks(&tracks, &marked_header(&filter, tracks.len()));
    Ok(())
}

/// Display selected fields for tracks read from stdin
///
/// Give `-` to read hashes from stdin (one per line), looks up TrackInfo for each hash,
/// and renders a colored, columnized table with the selected fields.
///
/// Field order in the output matches the order the flags appear on the command line.
///
/// # Examples
///
/// ```sh
/// mdma search --artist CBL | mdma view --hash --artist --title -
/// mdma search --genre Techno | mdma view --title --duration -
/// cat friday.plist | mdma view --hash --artist --title --duration -
/// ```
#[hicli::command]
pub(crate) fn view(columns: ViewColumns, _stdin: StdinDash, ctx: &Ctx) -> Result<()> {
    use std::io::IsTerminal;

    let ViewColumns(columns) = columns;
    let library = ctx.library()?;

    // Read hashes from stdin (same format as `mdma playlist append` / `mdma search` output).
    let hashes = read_hashes();

    if hashes.is_empty() {
        if std::io::stdout().is_terminal() {
            eprintln!("No hashes on stdin. Pipe `mdma search` output or a playlist.");
        }
        return Ok(());
    }

    // Look up TrackInfo for each hash, preserving order.
    let mut tracks: Vec<TrackInfo> = Vec::with_capacity(hashes.len());
    for hash in hashes {
        match library.get_track(&hash) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{parses, strip_ansi, track, usage_error};

    fn service_status(unparseable_lines: usize) -> library_ipc_client::ServiceStatus {
        library_ipc_client::ServiceStatus {
            version: "1.0".into(),
            tracks_indexed: 1,
            facts_count: 2,
            inbox_queue_size: 3,
            uptime_seconds: 4,
            unparseable_lines,
        }
    }

    #[test]
    fn status_text_omits_unparseable_lines_when_zero() {
        assert!(!format_status(&service_status(0)).contains("nparseable"));
    }

    #[test]
    fn status_text_shows_unparseable_lines_when_nonzero() {
        let text = format_status(&service_status(7));
        assert!(
            text.contains("Unparseable:     7 fact-log lines skipped"),
            "{text}"
        );
    }

    use track_formatter::{render_for_user, ViewColumn};

    use pretty_assertions::assert_eq;

    use track_formatter::ALL_COLUMNS;

    // ── hicli Arg parsers ─────────────────────────────────────────────────────

    use crate::test_support::run_arg;

    #[test]
    fn limit_arg_parsers_read_a_non_zero_count() {
        let three = LimitArg(NonZeroUsize::new(3).unwrap());
        assert_eq!(run_arg::<LimitArg>(&["--limit", "3"]), Ok(three));
        assert_eq!(run_arg::<LimitArg>(&["-l", "3"]), Ok(three));
        let err = run_arg::<LimitArg>(&["--limit", "0"]).unwrap_err();
        assert!(
            err.ends_with("`--limit`: must be a whole number of at least 1"),
            "{err}"
        );
        assert!(run_arg::<LimitArg>(&[]).is_err());
    }

    #[test]
    fn an_unsupported_request_stops_the_run_with_the_upgrade_text() {
        use library_ipc_client::{ClientError, ProtocolError};
        let calls = std::cell::Cell::new(0);
        let result = for_each_track(
            vec![ContentHash::new("aaaa"), ContentHash::new("bbbb")],
            |_| {
                calls.set(calls.get() + 1);
                Err(ClientError::Protocol(ProtocolError::InvalidRequest {
                    message: "unknown variant".to_string(),
                }))
            },
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            crate::test_support::failure_text(result),
            mdma_client::upgrade_node_text("CLI")
        );
    }

    fn named(name: &str) -> Mark {
        name.parse().expect("a valid mark")
    }

    #[test]
    fn mark_arg_parser_is_a_positional_mark() {
        assert_eq!(
            run_arg::<MarkArg>(&["Banger"]),
            Ok(MarkArg(named("banger")))
        );
        assert_eq!(run_arg::<MarkArg>(&["LIKED"]), Ok(MarkArg(Mark::Default)));
        let err = run_arg::<MarkArg>(&[""]).unwrap_err();
        assert!(err.contains("mark name is empty"), "{err}");
    }

    #[test]
    fn no_mark_name_means_the_default_mark_or_any_mark() {
        assert_eq!(mark_or_default(None), Mark::Default);
        assert_eq!(mark_filter(None), MarkFilter::Any);
        let banger = Some(MarkArg(named("banger")));
        assert_eq!(mark_or_default(banger.clone()), named("banger"));
        assert_eq!(mark_filter(banger), MarkFilter::Only(named("banger")));
    }

    #[test]
    fn mark_commands_reject_a_bad_name_as_a_usage_error() {
        for cmd in ["mark", "unmark", "marked", "bookmark", "bookmarks"] {
            assert!(
                usage_error(&[cmd, ""]).contains("mark name is empty"),
                "{cmd}"
            );
            let long = "x".repeat(65);
            let e = usage_error(&[cmd, &long]);
            assert!(e.contains("longer than 64 characters"), "{cmd}: {e}");
        }
    }

    #[test]
    fn mark_commands_without_a_connection_fail_cleanly() {
        let ctx = crate::test_support::local_ctx();
        for result in [
            marked(None, &ctx),
            marked(Some(MarkArg(Mark::Default)), &ctx),
        ] {
            assert!(result.is_err());
        }
        let nothing = || MarkInput {
            name: None,
            stdin: None,
        };
        assert!(mark(nothing(), &ctx).is_err());
        assert!(unmark(nothing(), &ctx).is_err());
        for text in [
            crate::test_support::failure_text(marked(None, &ctx)),
            crate::test_support::failure_text(mark(nothing(), &ctx)),
            crate::test_support::failure_text(unmark(nothing(), &ctx)),
        ] {
            assert!(text.starts_with("Failed to connect to library"), "{text}");
        }
    }

    #[test]
    fn sort_field_parser_reads_a_positional_field() {
        assert_eq!(run_arg::<SortField>(&["bpm"]), Ok(SortField::Bpm));
        assert_eq!(
            run_arg::<SortField>(&["track-number"]),
            Ok(SortField::TrackNumber)
        );
        let err = run_arg::<SortField>(&["nope"]).unwrap_err();
        assert!(err.contains("`FIELD`"), "{err}");
        assert!(err.contains("expected `bpm`, `title`,"), "{err}");
        assert!(
            err.contains("`disc-number`, `added`, `started` or `stopped`"),
            "{err}"
        );
    }

    #[test]
    fn multi_word_sort_fields_are_kebab_case() {
        assert_eq!(
            run_arg::<SortField>(&["disc-number"]),
            Ok(SortField::DiscNumber)
        );
        // Only the kebab-case spelling parses; the error lists the names.
        let err = run_arg::<SortField>(&["DiscNumber"]).unwrap_err();
        assert!(err.contains("`track-number`, `disc-number`"), "{err}");
    }

    #[test]
    fn sort_direction_parser_wants_exactly_one_flag() {
        assert_eq!(
            run_arg::<SortDirection>(&["-a"]),
            Ok(SortDirection::Ascending)
        );
        assert_eq!(
            run_arg::<SortDirection>(&["-d"]),
            Ok(SortDirection::Descending)
        );
        assert!(run_arg::<SortDirection>(&[]).is_err());
        assert!(run_arg::<SortDirection>(&["-a", "-d"]).is_err());
        assert!(run_arg::<SortDirection>(&["-d", "-a"]).is_err());
    }

    #[test]
    fn view_columns_parser_keeps_argv_order() {
        assert_eq!(
            run_arg::<ViewColumns>(&["--hash", "--title"]),
            Ok(ViewColumns(vec![ViewColumn::Hash, ViewColumn::Title]))
        );
        assert_eq!(
            run_arg::<ViewColumns>(&["--duration", "--artist", "--hash", "--title"]),
            Ok(ViewColumns(vec![
                ViewColumn::Duration,
                ViewColumn::Artist,
                ViewColumn::Hash,
                ViewColumn::Title,
            ]))
        );
    }

    #[test]
    fn view_columns_parser_rejects_none_and_duplicates() {
        let err = run_arg::<ViewColumns>(&[]).unwrap_err();
        assert!(err.contains("No fields selected"), "{err}");
        let err = run_arg::<ViewColumns>(&["--hash", "--hash"]).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
        let err = run_arg::<ViewColumns>(&["--hash", "--title", "--hash"]).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    // ── SortField / SortDirection ─────────────────────────────────────────────

    #[test]
    fn sort_field_names_round_trip() {
        for v in SortField::VARIANTS {
            assert_eq!(v.name().parse::<SortField>(), Ok(*v));
            assert_eq!(v.to_string(), v.name());
        }
        assert!("nope".parse::<SortField>().is_err());
    }

    #[test]
    fn list_limit_must_be_at_least_one() {
        let e = usage_error(&["list", "--limit", "0"]);
        assert!(
            e.contains("`0`: `--limit`: must be a whole number of at least 1"),
            "{e}"
        );
        assert!(
            !e.contains("'0'"),
            "the value is echoed once, by hicli: {e}"
        );
        usage_error(&["list", "--limit", "-1"]);
        usage_error(&["list", "--limit", "x"]);
        assert_eq!(parses(&["list", "-l", "3"]), ["list"]);
        assert_eq!(parses(&["list"]), ["list"]);
    }

    #[test]
    fn a_bad_limit_names_the_flag() {
        let e = usage_error(&["list", "--limit", "0"]);
        assert!(e.contains("`--limit`"), "{e}");
        let err = run_arg::<LimitArg>(&["--limit", "x"]).unwrap_err();
        assert!(err.contains("`--limit`"), "{err}");
    }

    #[test]
    fn a_bad_sort_field_names_the_argument_and_the_choices() {
        let e = usage_error(&["sort", "bmp", "-a"]);
        assert!(e.contains("`FIELD`"), "{e}");
        assert!(e.contains("did you mean `bpm`?"), "{e}");
    }

    #[test]
    fn a_bad_hash_names_the_argument() {
        let e = usage_error(&["get", "xyz"]);
        assert!(e.contains("`HASH|-`"), "{e}");
        // Hash-only commands still name plain `HASH`.
        let e = usage_error(&["library", "track", "replace", "xyz", "file.mp3"]);
        assert!(e.contains("`OLD_HASH`"), "{e}");
    }

    #[test]
    fn sort_needs_exactly_one_direction() {
        assert_eq!(parses(&["sort", "title", "-a", "-"]), ["sort"]);
        assert_eq!(parses(&["sort", "title", "-d", "-"]), ["sort"]);
        usage_error(&["sort", "title", "-a", "-d", "-"]);
        usage_error(&["sort", "title", "-"]);
    }

    #[test]
    fn mark_and_unmark_read_stdin_only_when_given_a_dash() {
        for cmd in ["mark", "unmark"] {
            assert_eq!(parses(&[cmd]), [cmd]);
            assert_eq!(parses(&[cmd, "banger"]), [cmd]);
            assert_eq!(parses(&[cmd, "-"]), [cmd], "{cmd} -");
            assert_eq!(parses(&[cmd, "banger", "-"]), [cmd], "{cmd} banger -");
            // One dash, and nothing after it.
            usage_error(&[cmd, "-", "-"]);
            usage_error(&[cmd, "banger", "-", "extra"]);
        }
    }

    #[test]
    fn a_mark_cannot_be_named_dash() {
        // `-` is the stdin marker, never a mark name (the default mark has no name).
        assert_eq!(super::mark_or_default(None), Mark::Default);
        assert!(run_arg::<MarkArg>(&["-"]).is_err());
    }

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
                SortDirection::Ascending,
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
                SortDirection::Descending,
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
                SortDirection::Descending,
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
                SortDirection::Ascending,
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
    fn view_needs_at_least_one_column() {
        assert_eq!(parses(&["view", "--title", "--hash", "-"]), ["view"]);
        let e = usage_error(&["view", "-"]);
        assert!(e.contains("No fields selected"), "{e}");
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
}
