use crate::args::{choice, choice_names, impl_arg, parse_choice, Choice, TrackArg};
use crate::ctx::Ctx;
use crate::output::{
    format_track_line, library_error, mark_outcome, marked_header, no_marked_tracks, print_tracks,
    success_unless, terminal_width, unmark_outcome, PIPE_TERM_WIDTH,
};
use crate::stdin::{
    hashes_from_pipe_or_playing, read_hashes, require_stdin_hashes, resolve_tracks_skip_errors,
};
use bpaf::{construct, long, positional, short, Parser};
use color_eyre::Result;
use hicli::Arg;
use library_ipc_client::{ClientError, ContentHash, Mark, MarkFilter, MarkNameError, TrackInfo};
use std::num::NonZeroUsize;
use track_formatter::{render_for_user, short_hash, ViewColumn};

/// A track count of at least 1 (a limit of 0 would show nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LimitArg(pub NonZeroUsize);

impl std::str::FromStr for LimitArg {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.parse::<usize>()
            .ok()
            .and_then(NonZeroUsize::new)
            .map(Self)
            .ok_or_else(|| format!("'{s}' must be a whole number of at least 1"))
    }
}

impl_arg!(LimitArg, long "limit", short 'l', "LIMIT", "Maximum number of tracks to show");

/// Maximum number of tracks to show (all when `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OptLimitArg(pub Option<LimitArg>);

impl_arg!(OptLimitArg, optional LimitArg);

/// A mark, as typed: `liked` is the default mark, any other name a named mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkArg(pub Mark);

impl std::str::FromStr for MarkArg {
    type Err = MarkNameError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

impl_arg!(
    MarkArg,
    positional "NAME",
    "Mark name (`liked` is the default mark)"
);

/// An optional [`MarkArg`] (none: the default mark, or any mark when listing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptMarkArg(pub Option<MarkArg>);

impl_arg!(OptMarkArg, optional MarkArg);

impl OptMarkArg {
    /// The mark to apply: the one named, else the default mark.
    fn mark(self) -> Mark {
        self.0.map_or(Mark::Default, |MarkArg(mark)| mark)
    }

    /// The marks to list: the one named, else any.
    fn filter(self) -> MarkFilter {
        self.0
            .map_or(MarkFilter::Any, |MarkArg(mark)| MarkFilter::Only(mark))
    }
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

impl Arg for SortField {
    fn parser() -> impl Parser<Self> {
        let field = positional::<String>("FIELD")
            .help(format!("Field to sort by: {}", choice_names::<Self>().join(", ")).as_str());
        choice(field)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl Choice for SortField {
    const ALL: &'static [Self] = &[
        SortField::Bpm,
        SortField::Title,
        SortField::Artist,
        SortField::Album,
        SortField::Duration,
        SortField::TrackNumber,
        SortField::DiscNumber,
        SortField::Added,
        SortField::Started,
        SortField::Stopped,
    ];
    const WHAT: &'static str = "sort field";

    fn name(self) -> &'static str {
        match self {
            SortField::Bpm => "bpm",
            SortField::Title => "title",
            SortField::Artist => "artist",
            SortField::Album => "album",
            SortField::Duration => "duration",
            SortField::TrackNumber => "track-number",
            SortField::DiscNumber => "disc-number",
            SortField::Added => "added",
            SortField::Started => "started",
            SortField::Stopped => "stopped",
        }
    }
}

impl std::fmt::Display for SortField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for SortField {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_choice(s)
    }
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
    println!("MDMA Library Service v{}", status.version);
    println!("{}", "=".repeat(35));
    println!("Tracks indexed:  {}", status.tracks_indexed);
    println!("Facts count:     {}", status.facts_count);
    println!("Inbox queue:     {} files", status.inbox_queue_size);
    println!("Uptime:          {} seconds", status.uptime_seconds);
    Ok(())
}

/// List tracks in library
#[hicli::command]
pub(crate) fn list(limit: OptLimitArg, ctx: &Ctx) -> Result<()> {
    let OptLimitArg(limit) = limit;
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
#[hicli::command]
pub(crate) fn get(hash: TrackArg, ctx: &Ctx) -> Result<()> {
    let TrackArg(content_hash) = hash;
    let library = ctx.library()?;
    let track = library.get_track(&content_hash).map_err(library_error)?;
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

/// Show all facts for a track
#[hicli::command]
pub(crate) fn facts(hash: TrackArg, ctx: &Ctx) -> Result<()> {
    let TrackArg(content_hash) = hash;
    let library = ctx.library()?;
    let (full_hash, facts) = library.get_facts(&content_hash).map_err(library_error)?;
    println!("Facts for: {}", full_hash.as_str());
    println!("{}", "=".repeat(65));
    for (fact_type, fact_value) in facts {
        println!("{:20} | {}", fact_type, fact_value);
    }
    Ok(())
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
/// Reads one hash per line from stdin, outputs sorted hashes.
/// Stable sort: chain multiple invocations for multi-key sort (right-to-left priority).
/// For started and stopped, a missing value counts as -inf: ascending puts never-played
/// first, descending puts never-played last.
///
/// Examples:
///   mdma search --artist=CBL | mdma sort title -a
///   mdma queue list | mdma sort bpm -d | mdma queue append
///   cat friday.plist | mdma sort title -a | mdma sort artist -a > sorted.plist
#[hicli::command]
pub(crate) fn sort(direction: SortDirection, field: SortField, ctx: &Ctx) -> Result<()> {
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

/// Run `act` on each of `hashes`, printing `{short_hash}  {line}` for each
/// success to stdout and `{short_hash}  {error}` for each failure to stderr; the
/// run fails when any track failed.
fn for_each_track(
    hashes: Vec<ContentHash>,
    act: impl Fn(&ContentHash) -> Result<String, ClientError>,
) -> hicli::Success {
    let mut failed = false;
    for hash in hashes {
        match act(&hash) {
            Ok(line) => println!("{}  {}", short_hash(&hash), line),
            Err(e) => {
                failed = true;
                eprintln!("{}  {}", short_hash(&hash), library_error(e));
            }
        }
    }
    success_unless(failed)
}

/// Mark tracks (liked unless NAME is given)
///
/// Marks the tracks whose hashes are piped on stdin; run from a terminal without
/// a pipe, marks the playing track. A track that already carries the mark is left
/// as it is. Each output line starts with the track hash, so the output can be
/// piped on.
///
/// Examples:
///   mdma mark
///   mdma search --artist CBL | mdma mark banger
#[hicli::command(alias = "bookmark")]
pub(crate) fn mark(name: OptMarkArg, ctx: &Ctx) -> Result<hicli::Success> {
    let mark = name.mark();
    let library = ctx.library()?;
    let hashes = hashes_from_pipe_or_playing(ctx)?;
    Ok(for_each_track(hashes, |hash| {
        library
            .write_mark(hash, &mark)
            .map(|outcome| mark_outcome(&mark, outcome))
    }))
}

/// Remove a mark from tracks (liked unless NAME is given)
///
/// Unmarks the tracks whose hashes are piped on stdin; run from a terminal without
/// a pipe, unmarks the playing track. A track without the mark is left as it is.
///
/// Examples:
///   mdma unmark
///   mdma marked banger | mdma unmark banger
#[hicli::command]
pub(crate) fn unmark(name: OptMarkArg, ctx: &Ctx) -> Result<hicli::Success> {
    let mark = name.mark();
    let library = ctx.library()?;
    let hashes = hashes_from_pipe_or_playing(ctx)?;
    Ok(for_each_track(hashes, |hash| {
        library
            .unmark(hash, &mark)
            .map(|outcome| unmark_outcome(&mark, outcome))
    }))
}

/// List marked tracks
///
/// Lists the tracks that carry the mark NAME, or any mark when NAME is left out;
/// `mdma marked liked` lists only the default mark.
///
/// Examples:
///   mdma marked
///   mdma marked banger | mdma playlist new bangers
#[hicli::command(alias = "bookmarks")]
pub(crate) fn marked(name: OptMarkArg, ctx: &Ctx) -> Result<()> {
    let filter = name.filter();
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
/// Reads hashes from stdin (one per line), looks up TrackInfo for each hash,
/// and renders a colored, columnized table with the selected fields.
///
/// Field order in the output matches the order the flags appear on the command line.
///
/// Examples:
///   mdma search --artist CBL | mdma view --hash --artist --title
///   mdma search --genre Techno | mdma view --title --duration
///   cat friday.plist | mdma view --hash --artist --title --duration
#[hicli::command]
pub(crate) fn view(columns: ViewColumns, ctx: &Ctx) -> Result<()> {
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
        assert_eq!(
            run_arg::<OptLimitArg>(&["--limit", "3"]),
            Ok(OptLimitArg(Some(three)))
        );
        assert_eq!(run_arg::<OptLimitArg>(&[]), Ok(OptLimitArg(None)));
        let err = run_arg::<OptLimitArg>(&["--limit", "0"]).unwrap_err();
        assert!(
            err.contains("must be a whole number of at least 1"),
            "{err}"
        );
        assert!(run_arg::<LimitArg>(&[]).is_err());
    }

    fn named(name: &str) -> Mark {
        name.parse().expect("a valid mark")
    }

    #[test]
    fn opt_mark_arg_parser_is_an_optional_positional_mark() {
        assert_eq!(run_arg::<OptMarkArg>(&[]), Ok(OptMarkArg(None)));
        assert_eq!(
            run_arg::<OptMarkArg>(&["Banger"]),
            Ok(OptMarkArg(Some(MarkArg(named("banger")))))
        );
        assert_eq!(
            run_arg::<OptMarkArg>(&["LIKED"]),
            Ok(OptMarkArg(Some(MarkArg(Mark::Default))))
        );
        let err = run_arg::<OptMarkArg>(&[""]).unwrap_err();
        assert!(err.contains("mark name is empty"), "{err}");
    }

    #[test]
    fn no_mark_name_means_the_default_mark_or_any_mark() {
        assert_eq!(OptMarkArg(None).mark(), Mark::Default);
        assert_eq!(OptMarkArg(None).filter(), MarkFilter::Any);
        let banger = OptMarkArg(Some(MarkArg(named("banger"))));
        assert_eq!(banger.clone().mark(), named("banger"));
        assert_eq!(banger.filter(), MarkFilter::Only(named("banger")));
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
            marked(OptMarkArg(None), &ctx),
            marked(OptMarkArg(Some(MarkArg(Mark::Default))), &ctx),
        ] {
            assert!(result.is_err());
        }
        assert!(mark(OptMarkArg(None), &ctx).is_err());
        assert!(unmark(OptMarkArg(None), &ctx).is_err());
        for text in [
            crate::test_support::failure_text(marked(OptMarkArg(None), &ctx)),
            crate::test_support::failure_text(mark(OptMarkArg(None), &ctx)),
            crate::test_support::failure_text(unmark(OptMarkArg(None), &ctx)),
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
        assert!(
            err.contains("unknown sort field 'nope' (expected bpm, title,"),
            "{err}"
        );
        assert!(
            err.contains("disc-number, added, started or stopped)"),
            "{err}"
        );
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
        for v in SortField::ALL {
            assert_eq!(v.name().parse::<SortField>(), Ok(*v));
            assert_eq!(v.to_string(), v.name());
        }
        assert!("nope".parse::<SortField>().is_err());
    }

    #[test]
    fn list_limit_must_be_at_least_one() {
        let e = usage_error(&["list", "--limit", "0"]);
        assert!(
            e.contains("'0' must be a whole number of at least 1"),
            "{e}"
        );
        usage_error(&["list", "--limit", "-1"]);
        usage_error(&["list", "--limit", "x"]);
        assert_eq!(parses(&["list", "-l", "3"]), ["list"]);
        assert_eq!(
            run_arg::<OptLimitArg>(&["-l", "3"]),
            Ok(OptLimitArg(Some(LimitArg(NonZeroUsize::new(3).unwrap()))))
        );
        assert_eq!(parses(&["list"]), ["list"]);
        assert_eq!(run_arg::<OptLimitArg>(&[]), Ok(OptLimitArg(None)));
    }

    #[test]
    fn sort_needs_exactly_one_direction() {
        assert_eq!(parses(&["sort", "title", "-a"]), ["sort"]);
        assert_eq!(parses(&["sort", "title", "-d"]), ["sort"]);
        usage_error(&["sort", "title", "-a", "-d"]);
        usage_error(&["sort", "title"]);
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
        assert_eq!(parses(&["view", "--title", "--hash"]), ["view"]);
        let e = usage_error(&["view"]);
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
