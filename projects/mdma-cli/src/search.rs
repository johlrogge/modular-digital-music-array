use crate::args::{value_and_dash, StdinDash};
use crate::ctx::Ctx;
use crate::output::{library_error, print_tracks};
use crate::stdin::hashes_if_dash;
use bpaf::{construct, long, Parser};
use color_eyre::Result;
use hicli::{Arg, Candidate, Choice};
use library_ipc_client::{ContentHash, TrackInfo};
use library_search::{DateQuery, DurationQuery, KeyQuery, NumericQuery, StringQuery, TrackQuery};
use mdma_client::LibraryBackend;
use music_facts::{EnergyLevel, TrackRole};
use std::str::FromStr;

// Every filter is its own argument type and takes its help from its doc comment.
#[derive(Debug, Default, Arg)]
pub(crate) struct SearchArgs {
    artist: Option<ArtistFilter>,
    title: Option<TitleFilter>,
    album: Option<AlbumFilter>,
    label: Option<LabelFilter>,
    genre: Option<GenreFilter>,
    style: Option<StyleFilter>,
    bpm: Option<BpmArg>,
    key: Option<KeyArg>,
    duration: Option<DurationArg>,
    year: Option<YearArg>,
    source: Option<SourceFilter>,
    started: Option<StartedFilter>,
    stopped: Option<StoppedFilter>,
    added: Option<AddedFilter>,
    played: Option<PlayedFilter>,
    role: Option<RoleArg>,
    energy: Option<EnergyArg>,
    not: Invert,
    input: SearchInput,
}

/// The positionals of `search`: a free-text query, and `-` to read hashes from stdin.
#[derive(Debug, Default)]
pub(crate) struct SearchInput {
    query: Option<QueryText>,
    stdin: Option<StdinDash>,
}

impl Arg for SearchInput {
    /// `[QUERY] [-]`, and a hidden `--no-stdin`: accepted and ignored (stdin is read
    /// only for `-`), so scripts written when stdin was read implicitly keep working.
    fn parser() -> impl Parser<Self> {
        let no_stdin = long("no-stdin").switch().hide();
        let positionals = value_and_dash::<QueryText>(
            "QUERY",
            "[QUERY] [-]",
            "Free-text query applied to all text fields (title, artist, album, label, genre), \
             e.g. CarbBased or /regex/, and/or `-` to read hashes from stdin to search among \
             (no hashes on stdin matches nothing, or everything with --not); at most one of each",
        );
        construct!(no_stdin, positionals).map(|(_, (query, stdin))| Self { query, stdin })
    }
}

// The filters wrap the query types of `library_search`, which parse themselves:
// a filter has no `FromStr` of its own, so what a bad value says is theirs.

/// Filter by artist name
#[derive(Debug, Clone, Arg)]
#[arg(long = "artist", metavar = "ARTIST")]
pub(crate) struct ArtistFilter(pub StringQuery);

/// Filter by track title
#[derive(Debug, Clone, Arg)]
#[arg(long = "title", metavar = "TITLE")]
pub(crate) struct TitleFilter(pub StringQuery);

/// Filter by album name
#[derive(Debug, Clone, Arg)]
#[arg(long = "album", metavar = "ALBUM")]
pub(crate) struct AlbumFilter(pub StringQuery);

/// Filter by label name
#[derive(Debug, Clone, Arg)]
#[arg(long = "label", metavar = "LABEL")]
pub(crate) struct LabelFilter(pub StringQuery);

/// Filter by main genre (e.g. "Electronic", "Techno")
#[derive(Debug, Clone, Arg)]
#[arg(long = "genre", metavar = "GENRE")]
pub(crate) struct GenreFilter(pub StringQuery);

/// Filter by style descriptor (matches if any descriptor matches)
#[derive(Debug, Clone, Arg)]
#[arg(long = "style", metavar = "STYLE")]
pub(crate) struct StyleFilter(pub StringQuery);

/// Filter by source (e.g. bandcamp, beatport, upload)
// Stays a String: a source is any name a source service or the upload registers, so
// the set is open; completion offers the ones known today.
#[derive(Debug, Clone, Arg)]
#[arg(long = "source", metavar = "SOURCE", complete = complete_known_sources)]
pub(crate) struct SourceFilter(pub String);

/// The sources the library is known to hold tracks from.
fn complete_known_sources(typed: &str) -> Vec<Candidate> {
    ["bandcamp", "beatport", "upload"]
        .into_iter()
        .filter(|source| source.starts_with(typed))
        .map(|source| (source.to_string(), None))
        .collect()
}

/// Invert the search results: return tracks that do NOT match the filters
#[derive(Debug, Clone, Copy, Default, Arg)]
#[arg(long = "not")]
pub(crate) struct Invert(pub bool);

/// Free-text query applied to all text fields (title, artist, album, label, genre).
/// Supports CamelCase initialism (CarbBased) and /regex/ syntax.
#[derive(Debug, Clone, Arg)]
#[arg(positional = "QUERY")]
pub(crate) struct QueryText(pub StringQuery);

impl FromStr for QueryText {
    type Err = &'static str;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "-" {
            return Err("`-` reads hashes from stdin; it is not a query");
        }
        Ok(Self(s.parse().unwrap_or_else(|never| match never {})))
    }
}

/// Filter by last started date (format: N/A, >2026-02, <2026, 2026-01..2026-06; dash-leading values: --started=-7)
#[derive(Debug, Clone, Arg)]
#[arg(long = "started", metavar = "STARTED")]
pub(crate) struct StartedFilter(pub DateQuery);

/// Filter by last stopped date (same format as --started; dash-leading values: --stopped=-7)
#[derive(Debug, Clone, Arg)]
#[arg(long = "stopped", metavar = "STOPPED")]
pub(crate) struct StoppedFilter(pub DateQuery);

/// Filter by date added to library. Uses date expression syntax: ~/+1/15 (15th next month), --added=-7 (7 days ago), ^ (1st of month), $ (end of month). Ranges: --added=-7..~ (last 7 days to today).
#[derive(Debug, Clone, Arg)]
#[arg(long = "added", metavar = "ADDED")]
pub(crate) struct AddedFilter(pub DateQuery);

/// Fact type to inspect (e.g. MainGenre, Label, Key, Source, BPM, StyleDescriptor)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "FACT_TYPE")]
pub(crate) struct FactTypeArg(pub library_ipc_client::FactType);

impl FromStr for FactTypeArg {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(FactTypeArg(library_ipc_client::FactType::new(
            s.to_string(),
        )))
    }
}

/// Filter by DJ role
#[derive(Debug, Clone, Copy, PartialEq, Eq, Choice, Arg)]
#[arg(long = "role", metavar = "ROLE")]
pub(crate) enum RoleArg {
    /// Starts a set
    Opener,
    /// Raises the energy towards the peak
    BuildUp,
    /// The high point of a set
    Peak,
    /// A crowd-peak exclamation point before the energy drops
    Banger,
    /// Winds the energy down
    CoolDown,
    /// Ends a set
    Closer,
    /// Used outside the main arc of a set
    Filler,
}

impl From<RoleArg> for TrackRole {
    fn from(role: RoleArg) -> Self {
        match role {
            RoleArg::Opener => TrackRole::Opener,
            RoleArg::BuildUp => TrackRole::BuildUp,
            RoleArg::Peak => TrackRole::Peak,
            RoleArg::Banger => TrackRole::Banger,
            RoleArg::CoolDown => TrackRole::CoolDown,
            RoleArg::Closer => TrackRole::Closer,
            RoleArg::Filler => TrackRole::Filler,
        }
    }
}

/// Filter by energy level (1-10; formats: 7  5..8  7+-2)
///
/// Every bound must be a valid `EnergyLevel`.
#[derive(Debug, Clone, Arg)]
#[arg(long = "energy", metavar = "ENERGY")]
pub(crate) struct EnergyArg(pub NumericQuery);

impl FromStr for EnergyArg {
    type Err = &'static str;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let query: NumericQuery = s.parse()?;
        let bounds: Vec<(f32, &'static str)> = match &query {
            NumericQuery::Exact(v) => vec![(*v, "must be a whole level from 1 to 10")],
            NumericQuery::Range(lo, hi) => vec![
                (*lo, "lower bound must be a whole level from 1 to 10"),
                (*hi, "upper bound must be a whole level from 1 to 10"),
            ],
            NumericQuery::Tolerance { value, .. } => {
                vec![(*value, "value must be a whole level from 1 to 10")]
            }
        };
        for (bound, phrase) in bounds {
            let whole = bound.fract() == 0.0 && (0.0..=255.0).contains(&bound);
            whole
                .then_some(bound as u8)
                .and_then(|level| EnergyLevel::new(level).ok())
                .ok_or(phrase)?;
        }
        Ok(EnergyArg(query))
    }
}

/// Filter by BPM (formats: 128  128+-4  124..132  128+2  128-2)
#[derive(Debug, Clone, Arg)]
#[arg(long = "bpm", metavar = "BPM")]
pub(crate) struct BpmArg(pub NumericQuery);

/// Filter by release year (formats: 2022  2019..2022)
#[derive(Debug, Clone, Arg)]
#[arg(long = "year", metavar = "YEAR")]
pub(crate) struct YearArg(pub NumericQuery);

/// Filter by musical key (formats: Am  "A minor"  8B  8B+-1  8B+-1~)
#[derive(Debug, Clone, Arg)]
#[arg(long = "key", metavar = "KEY")]
pub(crate) struct KeyArg(pub KeyQuery);

/// Filter by duration (formats: 7m  7m15s  >5m  <8m  6m..8m)
#[derive(Debug, Clone, Arg)]
#[arg(long = "duration", metavar = "DURATION")]
pub(crate) struct DurationArg(pub DurationQuery);

/// Search for tracks by filter or free text
#[hicli::command(default)]
pub(crate) fn query(args: SearchArgs, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let stdin = args.input.stdin;
    let track_query = build_track_query(args);
    search_tracks(&library, &track_query, hashes_if_dash(stdin))
}

/// Filter by playback history (takes precedence over --started); `never` matches
/// tracks with no started fact
#[derive(Debug, Clone, Copy, PartialEq, Eq, Choice, Arg)]
#[arg(long = "played", metavar = "PLAYED")]
pub(crate) enum PlayedFilter {
    /// Tracks that have never been played (no started fact)
    Never,
}

/// Build a TrackQuery from the search arguments (`-` is not part of the query).
///
/// Precedence: `--played=never` overrides any explicit `--started` value.
fn build_track_query(args: SearchArgs) -> TrackQuery {
    let SearchArgs {
        artist,
        title,
        album,
        label,
        genre,
        style,
        bpm,
        key,
        duration,
        year,
        source,
        started,
        stopped,
        added,
        played,
        role,
        energy,
        not,
        input: SearchInput {
            query: any_text, ..
        },
    } = args;
    let started = if matches!(played, Some(PlayedFilter::Never)) {
        // --played=never is a shortcut for started=N/A and overrides any --started value.
        Some(DateQuery::NA)
    } else {
        started.map(|StartedFilter(q)| q)
    };
    TrackQuery {
        any_text: any_text.map(|QueryText(q)| q),
        artist: artist.map(|ArtistFilter(q)| q),
        title: title.map(|TitleFilter(q)| q),
        album: album.map(|AlbumFilter(q)| q),
        label: label.map(|LabelFilter(q)| q),
        genre: genre.map(|GenreFilter(q)| q),
        style: style.map(|StyleFilter(q)| q),
        bpm: bpm.map(|BpmArg(q)| q),
        key: key.map(|KeyArg(q)| q),
        duration: duration.map(|DurationArg(q)| q),
        year: year.map(|YearArg(q)| q),
        source: source.map(|SourceFilter(s)| s),
        started,
        stopped: stopped.map(|StoppedFilter(q)| q),
        added: added.map(|AddedFilter(q)| q),
        role: role.map(TrackRole::from),
        energy: energy.map(|EnergyArg(q)| q),
        not: not.0,
    }
}

/// Returns true if a track's content hash matches any hash in `set`.
///
/// Both the track hash and the piped hash are stripped of the "sha256:" prefix
/// before comparison, and a prefix match is used (like `git log <short-hash>`).
fn hash_matches(track_hash: &ContentHash, set: &[ContentHash]) -> bool {
    fn strip(h: &ContentHash) -> &str {
        h.as_str().strip_prefix("sha256:").unwrap_or(h.as_str())
    }
    let clean = strip(track_hash);
    set.iter().any(|piped| clean.starts_with(strip(piped)))
}

/// Apply a stdin hash filter to a list of tracks.
///
/// - `stdin`: `None` means no filter was piped — return `tracks` unchanged.
/// - `stdin`: `Some(set)` with `not == false` — keep only tracks whose hash is in `set`
///   (intersection).
/// - `stdin`: `Some(set)` with `not == true` — keep only tracks whose hash is NOT in `set`
///   (exclusion). In this case the caller must ensure the query was sent to the library
///   with `.not = false` so that the library returns a candidate set (not nothing).
fn apply_stdin_filter(
    tracks: Vec<TrackInfo>,
    stdin: Option<&[ContentHash]>,
    not: bool,
) -> Vec<TrackInfo> {
    match stdin {
        None => tracks,
        Some(set) => tracks
            .into_iter()
            .filter(|t| hash_matches(&t.content_hash, set) != not)
            .collect(),
    }
}

/// Search the library; `stdin_filter` holds the hashes read from stdin (asked for
/// with `-`), if any, as an intersection filter.
fn search_tracks(
    library: &LibraryBackend,
    query: &TrackQuery,
    stdin_filter: Option<Vec<ContentHash>>,
) -> Result<()> {
    use std::io::IsTerminal;

    // When stdin hashes are present and --not is set, we invert the intersection rather
    // than the library query: send the query without `.not` (so the library returns a
    // candidate set), then exclude any track whose hash is in the piped set.
    // Without stdin, `.not` flows through to the library as usual.
    let effective_not = query.not && stdin_filter.is_some();
    let mut effective_query = query.clone();
    if effective_not {
        effective_query.not = false;
    }

    let tracks = library.search(&effective_query).map_err(library_error)?;
    let tracks = apply_stdin_filter(tracks, stdin_filter.as_deref(), effective_not);

    if tracks.is_empty() && std::io::stdout().is_terminal() {
        println!("No tracks found");
        return Ok(());
    }
    print_tracks(
        &tracks,
        &format!("Search results ({} matches)", tracks.len()),
    );
    Ok(())
}

/// List all distinct values stored for a fact type
///
/// # Examples
///
/// ```sh
/// mdma search fact-values-for genre
/// mdma search fact-values-for label | grep -i "ost"
/// mdma search fact-values-for genre | dmenu | xargs -I{} mdma search --genre {}
/// ```
#[hicli::command]
pub(crate) fn fact_values_for(fact_type: FactTypeArg, ctx: &Ctx) -> Result<()> {
    use std::io::IsTerminal;
    let FactTypeArg(fact_type) = fact_type;
    let library = ctx.library()?;
    let values = library
        .get_fact_values(fact_type.as_str())
        .map_err(library_error)?;
    if std::io::stdout().is_terminal() {
        if values.is_empty() {
            println!("No values found for fact type '{}'", fact_type);
            return Ok(());
        }
        println!("Values for '{}' ({} distinct):", fact_type, values.len());
        println!("{}", "=".repeat(65));
        for v in values {
            println!("  {}", v);
        }
    } else {
        // Pipe mode: one value per line — composable with dmenu, grep, etc.
        for v in values {
            println!("{}", v);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{help, parses, track, usage_error};

    use pretty_assertions::assert_eq;

    // ── hicli Arg parsers ────────────────────────────────────────────────────

    use crate::test_support::run_arg;

    #[test]
    fn fact_type_arg_parser_reads_a_positional() {
        assert_eq!(
            run_arg::<FactTypeArg>(&["MainGenre"]),
            Ok(FactTypeArg(library_ipc_client::FactType::new(
                "MainGenre".to_string()
            )))
        );
        assert!(run_arg::<FactTypeArg>(&[]).is_err());
    }

    #[test]
    fn search_reads_stdin_only_when_given_a_dash() {
        assert_eq!(parses(&["search"]), ["search"]);
        assert_eq!(parses(&["search", "-"]), ["search"]);
        assert_eq!(parses(&["search", "jazz", "-"]), ["search"]);
        assert_eq!(parses(&["search", "--bpm", "120..130", "-"]), ["search"]);
        assert_eq!(parses(&["search", "-", "--bpm", "120..130"]), ["search"]);
        // One dash, and nothing after it.
        usage_error(&["search", "-", "-"]);
    }

    #[test]
    fn usage_shows_one_optional_value_and_one_optional_dash() {
        let flat = |t: String| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let search = flat(help(&["search", "--help"]));
        assert!(search.contains("[--not] [QUERY] [-])"), "{search}");
        assert!(
            search.contains("no hashes on stdin matches nothing"),
            "{search}"
        );
        for cmd in ["mark", "unmark"] {
            let text = flat(help(&[cmd, "--help"]));
            assert!(
                text.contains(&format!("Usage: mdma {cmd} [NAME] [-]")),
                "{text}"
            );
        }
    }

    #[test]
    fn a_dash_with_no_hashes_matches_nothing_or_everything_with_not() {
        let tracks = vec![track("a1aaaaaa"), track("a2aaaaaa")];
        assert_eq!(
            apply_stdin_filter(tracks.clone(), Some(&[]), false).len(),
            0
        );
        assert_eq!(apply_stdin_filter(tracks, Some(&[]), true).len(), 2);
    }

    #[test]
    fn a_dash_is_not_a_search_query() {
        assert!(run_arg::<QueryText>(&["-"]).is_err());
        assert!(run_arg::<QueryText>(&["jazz"]).is_ok());
    }

    #[test]
    fn no_stdin_is_still_accepted_but_hidden() {
        assert_eq!(
            parses(&["search", "--no-stdin", "--bpm", "128"]),
            ["search"]
        );
        let text = help(&["search", "--help"]);
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(!text.contains("no-stdin"), "{text}");
        assert!(
            text.contains("Give `-` to read hashes from stdin"),
            "{text}"
        );
        assert!(
            text.contains("mdma search --bpm 120..130 | mdma mark -"),
            "{text}"
        );
    }

    #[test]
    fn numeric_filter_parsers_are_named_options() {
        assert!(matches!(
            run_arg::<BpmArg>(&["--bpm", "128"]),
            Ok(BpmArg(NumericQuery::Exact(v))) if v == 128.0
        ));
        assert!(matches!(
            run_arg::<YearArg>(&["--year=2019..2022"]),
            Ok(YearArg(NumericQuery::Range(..)))
        ));
        assert!(matches!(
            run_arg::<EnergyArg>(&["--energy", "7"]),
            Ok(EnergyArg(NumericQuery::Exact(v))) if v == 7.0
        ));
        assert!(run_arg::<BpmArg>(&[]).is_err());
    }

    #[test]
    fn invalid_filters_carry_the_query_types_reason_without_the_value() {
        fn reason<T: std::str::FromStr<Err = &'static str> + std::fmt::Debug>() -> String {
            "garbage".parse::<T>().unwrap_err().to_string()
        }
        let cases = [
            (
                run_arg::<BpmArg>(&["--bpm", "garbage"]).map(|_| ()),
                reason::<NumericQuery>(),
            ),
            (
                run_arg::<YearArg>(&["--year", "garbage"]).map(|_| ()),
                reason::<NumericQuery>(),
            ),
            (
                run_arg::<KeyArg>(&["--key", "garbage"]).map(|_| ()),
                reason::<KeyQuery>(),
            ),
            (
                run_arg::<DurationArg>(&["--duration", "garbage"]).map(|_| ()),
                reason::<DurationQuery>(),
            ),
        ];
        for (parsed, reason) in cases {
            let err = parsed.unwrap_err();
            assert!(err.ends_with(&reason), "{err}");
            assert_eq!(
                err.matches("garbage").count(),
                1,
                "echoed once, by hicli: {err}"
            );
        }
        let err = run_arg::<RoleArg>(&["--role", "garbage"]).unwrap_err();
        assert!(err.contains("expected `opener`, `build-up`"), "{err}");
        let err = run_arg::<EnergyArg>(&["--energy", "11"]).unwrap_err();
        assert!(
            err.ends_with("`--energy`: must be a whole level from 1 to 10"),
            "{err}"
        );
        let err = run_arg::<EnergyArg>(&["--energy", "5..11"]).unwrap_err();
        assert!(
            err.ends_with("`--energy`: upper bound must be a whole level from 1 to 10"),
            "{err}"
        );
        let err = run_arg::<EnergyArg>(&["--energy", "0..5"]).unwrap_err();
        assert!(err.contains("lower bound must be"), "{err}");
    }

    #[test]
    fn a_bad_filter_value_names_the_flag() {
        for (flag, value) in [
            ("--bpm", "fast"),
            ("--key", "H#"),
            ("--duration", "long"),
            ("--year", "last-year"),
            ("--role", "dancer"),
            ("--energy", "11"),
        ] {
            let e = usage_error(&["search", flag, value]);
            assert!(e.contains(&format!("`{flag}`")), "{flag}: {e}");
        }
        let e = usage_error(&["search", "--played", "always"]);
        assert!(e.contains("`--played`"), "{e}");
        assert!(e.contains("expected `never`"), "{e}");
    }

    #[test]
    fn key_duration_and_role_parsers_accept_valid_values() {
        assert!(run_arg::<KeyArg>(&["--key", "Am"]).is_ok());
        assert!(run_arg::<DurationArg>(&["--duration", ">5m"]).is_ok());
        assert_eq!(run_arg::<RoleArg>(&["--role", "peak"]), Ok(RoleArg::Peak));
    }

    #[test]
    fn role_names_are_the_ones_track_role_parses() {
        for role in RoleArg::VARIANTS {
            assert_eq!(
                role.name().parse::<TrackRole>().ok(),
                Some(TrackRole::from(*role)),
                "{}",
                role.name()
            );
        }
        assert_eq!(
            run_arg::<RoleArg>(&["--role", "cool-down"]),
            Ok(RoleArg::CoolDown)
        );
    }

    #[test]
    fn source_is_open_but_completes_the_known_sources() {
        assert_eq!(
            run_arg::<SourceFilter>(&["--source", "anything"])
                .unwrap()
                .0,
            "anything"
        );
        let offered: Vec<String> = complete_known_sources("b")
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(offered, ["bandcamp", "beatport"]);
    }

    #[test]
    fn played_help_says_it_once() {
        let help = help(&["search", "--help"]);
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(
                "(takes precedence over --started); `never` matches tracks with no started fact [never]"
            ),
            "{flat}"
        );
        assert_eq!(flat.matches("[never]").count(), 1, "{flat}");
    }

    #[test]
    fn played_filter_parser_accepts_only_never() {
        assert_eq!(
            run_arg::<PlayedFilter>(&["--played", "never"]),
            Ok(PlayedFilter::Never)
        );
        let err = run_arg::<PlayedFilter>(&["--played", "always"]).unwrap_err();
        assert!(err.contains("expected `never`"), "{err}");
    }

    // ── Date arg hyphen-value parsing ────────────────────────────────────────

    #[test]
    fn search_added_equals_hyphen_value_parses() {
        parses(&["search", "--added=-7"]);
    }

    #[test]
    fn search_started_equals_hyphen_value_parses() {
        parses(&["search", "--started=-7"]);
    }

    #[test]
    fn search_added_separate_hyphen_value_is_usage_error() {
        usage_error(&["search", "--added", "-7"]);
    }

    #[test]
    fn search_added_range_parses() {
        parses(&["search", "--added=-7..~"]);
    }

    #[test]
    fn search_added_garbage_is_parse_error() {
        let err = usage_error(&["search", "--added=garbage"]);
        assert!(
            err.contains("`garbage`: `--added`: expected a date"),
            "{err}"
        );
    }

    #[test]
    fn search_started_and_stopped_garbage_are_parse_errors() {
        for flag in ["started", "stopped"] {
            let err = usage_error(&["search", &format!("--{flag}=garbage")]);
            assert!(
                err.contains(&format!("`garbage`: `--{flag}`: expected a date")),
                "{flag}: {err}"
            );
        }
    }

    // ── role / energy parsing ────────────────────────────────────────────────

    #[test]
    fn search_role_parses_and_rejects() {
        let args = parse_search(&["--role", "peak"]).unwrap();
        assert!(args.role.is_some(), "{args:?}");
        let err = parse_search(&["--role", "dancer"]).unwrap_err();
        assert!(err.contains("`--role`: expected `opener`"), "{err}");
    }

    #[test]
    fn search_energy_accepts_values_in_range() {
        for v in ["1", "7", "10", "5..8", "1..10", "7+-2"] {
            assert!(parse_search(&["--energy", v]).is_ok(), "rejected {v}");
        }
    }

    #[test]
    fn search_energy_rejects_out_of_range_and_garbage() {
        for v in ["0", "11", "0..5", "5..11", "11+-1", "7.5", "loud"] {
            let err = parse_search(&["--energy", v]);
            assert!(err.is_err(), "accepted {v}");
        }
    }

    #[test]
    fn fact_type_arg_parses_any_name() {
        let arg: FactTypeArg = "MainGenre".parse().unwrap();
        assert_eq!(arg.0.as_str(), "MainGenre");
    }

    #[test]
    fn search_stopped_tilde_parses() {
        parses(&["search", "--stopped", "~"]);
    }

    // ── bpm / key / duration / year parsing ──────────────────────────────────

    fn parse_search(args: &[&str]) -> Result<SearchArgs, String> {
        run_arg::<SearchArgs>(args)
    }

    #[test]
    fn search_bpm_parses_into_numeric_query() {
        let args = parse_search(&["--bpm", "124..132"]).unwrap();
        assert!(
            matches!(args.bpm, Some(BpmArg(NumericQuery::Range(lo, hi))) if lo == 124.0 && hi == 132.0),
            "got {:?}",
            args.bpm
        );
    }

    #[test]
    fn search_bpm_invalid_is_parse_error() {
        let err = parse_search(&["--bpm", "fast"]).unwrap_err();
        assert!(err.contains("`--bpm`: expected a number"), "{err}");
    }

    #[test]
    fn search_year_parses_into_numeric_query() {
        let args = parse_search(&["--year", "2022"]).unwrap();
        assert!(
            matches!(args.year, Some(YearArg(NumericQuery::Exact(y))) if y == 2022.0),
            "got {:?}",
            args.year
        );
    }

    #[test]
    fn search_year_invalid_is_parse_error() {
        let err = parse_search(&["--year", "last-year"]).unwrap_err();
        assert!(err.contains("`--year`: expected a number"), "{err}");
    }

    #[test]
    fn search_key_parses_into_key_query() {
        let args = parse_search(&["--key", "8B"]).unwrap();
        assert!(args.key.is_some(), "got {:?}", args.key);
    }

    #[test]
    fn search_key_invalid_is_parse_error() {
        let err = parse_search(&["--key", "H#"]).unwrap_err();
        assert!(err.contains("`--key`: expected a Camelot key"), "{err}");
    }

    #[test]
    fn search_duration_parses_into_duration_query() {
        let args = parse_search(&["--duration", ">5m"]).unwrap();
        assert!(
            matches!(
                args.duration,
                Some(DurationArg(DurationQuery::AtLeast(300)))
            ),
            "got {:?}",
            args.duration
        );
    }

    #[test]
    fn search_duration_invalid_is_parse_error() {
        let err = parse_search(&["--duration", "long"]).unwrap_err();
        assert!(err.contains("`--duration`: expected a duration"), "{err}");
    }

    #[test]
    fn search_key_and_duration_reach_the_built_query() {
        let args = parse_search(&["--key", "8B", "--duration", "<8m", "--bpm", "128"]).unwrap();
        let query = build_track_query(args);
        assert!(query.key.is_some(), "key dropped from query");
        assert!(
            matches!(query.duration, Some(DurationQuery::AtMost(480))),
            "got {:?}",
            query.duration
        );
        assert!(query.bpm.is_some(), "bpm dropped from query");
    }

    // ── apply_stdin_filter tests ─────────────────────────────────────────────

    fn hash_set(hashes: &[&str]) -> Vec<ContentHash> {
        hashes.iter().map(|s| ContentHash::new(*s)).collect()
    }

    #[test]
    fn search_not_with_stdin_excludes_piped_hashes() {
        // stdin = {a1, a2}, library returns {a1, a2, a3, a4}, --not returns [a3, a4]
        let tracks = vec![
            track("sha256:a1aaaaaa"),
            track("sha256:a2aaaaaa"),
            track("sha256:a3aaaaaa"),
            track("sha256:a4aaaaaa"),
        ];
        let stdin = hash_set(&["sha256:a1aaaaaa", "sha256:a2aaaaaa"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), true);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, ["sha256:a3aaaaaa", "sha256:a4aaaaaa"]);
    }

    #[test]
    fn search_not_with_stdin_prefix_match_excludes() {
        // stdin token is a short prefix (no sha256: prefix on token)
        let tracks = vec![
            track("sha256:abcdef1234"),
            track("sha256:deadbeef56"),
            track("sha256:cafebabe78"),
        ];
        let stdin = hash_set(&["abcdef12", "deadbeef"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), true);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, ["sha256:cafebabe78"]);
    }

    #[test]
    fn search_stdin_without_not_intersects() {
        // Sanity: stdin + no --not still intersects
        let tracks = vec![
            track("sha256:a1aaaaaa"),
            track("sha256:a2aaaaaa"),
            track("sha256:a3aaaaaa"),
        ];
        let stdin = hash_set(&["sha256:a1aaaaaa", "sha256:a2aaaaaa"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), false);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, ["sha256:a1aaaaaa", "sha256:a2aaaaaa"]);
    }

    #[test]
    fn search_no_stdin_preserves_not_behavior() {
        // Without stdin, apply_stdin_filter returns tracks unchanged regardless of `not`
        let tracks = vec![track("sha256:a1aaaaaa"), track("sha256:a2aaaaaa")];
        let result = apply_stdin_filter(tracks.clone(), None, true);
        assert_eq!(result.len(), 2, "pass-through when no stdin, not=true");
        let result2 = apply_stdin_filter(tracks, None, false);
        assert_eq!(result2.len(), 2, "pass-through when no stdin, not=false");
    }

    // ── --played=never filter ─────────────────────────────────────────────────

    #[test]
    fn played_filter_names_round_trip() {
        for v in PlayedFilter::VARIANTS {
            assert_eq!(v.name().parse::<PlayedFilter>(), Ok(*v));
            assert_eq!(v.to_string(), v.name());
        }
        assert!("nope".parse::<PlayedFilter>().is_err());
    }

    #[test]
    fn search_played_never_maps_to_started_na() {
        parses(&["search", "--played", "never"]);
        // Build the query with played=never and no explicit --started
        let query = build_track_query(SearchArgs {
            played: Some(PlayedFilter::Never),
            ..Default::default()
        });
        assert!(
            matches!(query.started, Some(DateQuery::NA)),
            "expected started == Some(DateQuery::NA), got {:?}",
            query.started
        );
    }

    #[test]
    fn search_played_never_overrides_started() {
        // When --played=never and --started are both given, --played=never wins.
        let query = build_track_query(SearchArgs {
            started: Some(StartedFilter("-7".parse().unwrap())),
            played: Some(PlayedFilter::Never),
            ..Default::default()
        });
        assert!(
            matches!(query.started, Some(DateQuery::NA)),
            "expected --played=never to override --started, got {:?}",
            query.started
        );
    }
}
