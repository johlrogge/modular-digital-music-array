use crate::args::{choice, impl_arg, parse_choice, Choice};
use crate::ctx::Ctx;
use crate::output::{library_error, print_tracks};
use crate::stdin::piped_hashes;
use bpaf::{long, Bpaf, Parser};
use color_eyre::Result;
use hicli::Arg;
use library_ipc_client::{ContentHash, TrackInfo};
use library_search::{
    parse_date_query, parse_duration_query, parse_key_query, parse_numeric_query, parse_role_query,
    parse_string_query, DateQuery, DurationQuery, KeyQuery, NumericQuery, ParseError, TrackQuery,
};
use mdma_client::LibraryBackend;
use music_facts::{EnergyLevel, TrackRole};

// The typed filters take their help from their `Arg` impls, so their field docs
// are not shown.
#[derive(Debug, Default, Bpaf, Arg)]
pub(crate) struct SearchArgs {
    /// Filter by artist name
    #[bpaf(argument("ARTIST"))]
    artist: Option<String>,

    /// Filter by track title
    #[bpaf(argument("TITLE"))]
    title: Option<String>,

    /// Filter by album name
    #[bpaf(argument("ALBUM"))]
    album: Option<String>,

    /// Filter by label name
    #[bpaf(argument("LABEL"))]
    label: Option<String>,

    /// Filter by main genre (e.g. "Electronic", "Techno")
    #[bpaf(argument("GENRE"))]
    genre: Option<String>,

    /// Filter by style descriptor — matches if any descriptor matches
    #[bpaf(argument("STYLE"))]
    style: Option<String>,

    #[bpaf(external(BpmArg::parser), optional)]
    bpm: Option<BpmArg>,

    #[bpaf(external(KeyArg::parser), optional)]
    key: Option<KeyArg>,

    #[bpaf(external(DurationArg::parser), optional)]
    duration: Option<DurationArg>,

    #[bpaf(external(YearArg::parser), optional)]
    year: Option<YearArg>,

    /// Filter by source (bandcamp, beatport, upload)
    #[bpaf(argument("SOURCE"))]
    source: Option<String>,

    /// Ignore stdin (don't read piped hashes as intersection filter)
    no_stdin: bool,

    /// Filter by last started date. Format: N/A, >2026-02, <2026, 2026-01..2026-06 (dash-leading values: --started=-7)
    #[bpaf(long("started"), argument("STARTED"))]
    started: Option<DateArg>,

    /// Filter by last stopped date. Same format as --started (dash-leading values: --stopped=-7).
    #[bpaf(long("stopped"), argument("STOPPED"))]
    stopped: Option<DateArg>,

    /// Filter by date added to library. Uses date expression syntax: ~/+1/15 (15th next month), --added=-7 (7 days ago), ^ (1st of month), $ (end of month). Ranges: --added=-7..~ (last 7 days to today).
    #[bpaf(long("added"), argument("ADDED"))]
    added: Option<DateArg>,

    #[bpaf(external(PlayedFilter::parser), optional)]
    played: Option<PlayedFilter>,

    #[bpaf(external(RoleArg::parser), optional)]
    role: Option<RoleArg>,

    #[bpaf(external(EnergyArg::parser), optional)]
    energy: Option<EnergyArg>,

    /// Invert the search results — return tracks that do NOT match the filters.
    not: bool,

    /// Free-text query applied to all text fields (title, artist, album, label, genre).
    /// Supports CamelCase initialism (CarbBased) and /regex/ syntax.
    #[bpaf(positional("QUERY"))]
    query: Option<String>,
}

/// A fact type name (e.g. MainGenre, Label, Key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactTypeArg(pub library_ipc_client::FactType);

impl std::str::FromStr for FactTypeArg {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(FactTypeArg(library_ipc_client::FactType::new(
            s.to_string(),
        )))
    }
}

/// A date filter (e.g. `-7`, `>2026-02`, `2026-01..2026-06`, `N/A`).
#[derive(Debug, Clone)]
pub(crate) struct DateArg(pub DateQuery);

impl std::str::FromStr for DateArg {
    type Err = ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_date_query(s).map(DateArg)
    }
}

/// A DJ role filter (e.g. `peak`, `cool-down`).
#[derive(Debug, Clone)]
pub(crate) struct RoleArg(pub TrackRole);

impl std::str::FromStr for RoleArg {
    type Err = ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_role_query(s).map(RoleArg)
    }
}

/// An energy filter whose every bound is a valid `EnergyLevel` (e.g. `7`, `5..8`, `7+-2`).
#[derive(Debug, Clone)]
pub(crate) struct EnergyArg(pub NumericQuery);

impl std::str::FromStr for EnergyArg {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let query = parse_numeric_query(s).map_err(|e| e.to_string())?;
        let bounds = match &query {
            NumericQuery::Exact(v) => vec![*v],
            NumericQuery::Range(lo, hi) => vec![*lo, *hi],
            NumericQuery::Tolerance { value, .. } => vec![*value],
        };
        for bound in bounds {
            let whole = bound.fract() == 0.0 && (0.0..=255.0).contains(&bound);
            let level = whole.then_some(bound as u8).ok_or_else(|| {
                format!("invalid energy '{s}': {bound} is not a whole level 1..=10")
            })?;
            EnergyLevel::new(level).map_err(|e| format!("invalid energy '{s}': {e}"))?;
        }
        Ok(EnergyArg(query))
    }
}

/// A BPM filter (e.g. `128`, `128+-4`, `124..132`).
#[derive(Debug, Clone)]
pub(crate) struct BpmArg(pub NumericQuery);

impl std::str::FromStr for BpmArg {
    type Err = ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_numeric_query(s).map(BpmArg)
    }
}

/// A release-year filter (e.g. `2022`, `2019..2022`).
#[derive(Debug, Clone)]
pub(crate) struct YearArg(pub NumericQuery);

impl std::str::FromStr for YearArg {
    type Err = ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_numeric_query(s).map(YearArg)
    }
}

/// A musical key filter (e.g. `Am`, `8B+-1`).
#[derive(Debug, Clone)]
pub(crate) struct KeyArg(pub KeyQuery);

impl std::str::FromStr for KeyArg {
    type Err = ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_key_query(s).map(KeyArg)
    }
}

/// A duration filter (e.g. `7m15s`, `>5m`, `6m..8m`).
#[derive(Debug, Clone)]
pub(crate) struct DurationArg(pub DurationQuery);

impl std::str::FromStr for DurationArg {
    type Err = ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_duration_query(s).map(DurationArg)
    }
}

// The filter options are spelled as in `SearchArgs`. `DateArg` has no `Arg`
// impl: it is `--started`, `--stopped` and `--added` (one type, three spellings),
// so those are fields of the search args struct instead.

impl_arg!(
    FactTypeArg,
    positional "FACT_TYPE",
    "Fact type to inspect (e.g. MainGenre, Label, Key, Source, BPM, StyleDescriptor)"
);
impl_arg!(
    BpmArg,
    long "bpm",
    "BPM",
    "Filter by BPM. Formats: 128  128+-4  124..132  128+2  128-2"
);
impl_arg!(
    KeyArg,
    long "key",
    "KEY",
    "Filter by musical key. Formats: Am  \"A minor\"  8B  8B+-1  8B+-1~"
);
impl_arg!(
    DurationArg,
    long "duration",
    "DURATION",
    "Filter by duration. Formats: 7m  7m15s  >5m  <8m  6m..8m"
);
impl_arg!(
    YearArg,
    long "year",
    "YEAR",
    "Filter by release year. Formats: 2022  2019..2022"
);
impl_arg!(
    RoleArg,
    long "role",
    "ROLE",
    "Filter by DJ role. Accepted values: opener, build-up, peak, banger, cool-down, closer, filler"
);
impl_arg!(
    EnergyArg,
    long "energy",
    "ENERGY",
    "Filter by energy level (1-10). Formats: 7  5..8  7+-2"
);

impl Arg for PlayedFilter {
    fn parser() -> impl Parser<Self> {
        choice(
            long("played")
                .help("Filter by playback history. `never` matches tracks with no started fact (never played). When combined with --started, --played=never takes precedence.")
                .argument::<String>("PLAYED"),
        )
    }
}

/// Search for tracks by filter or free text
#[hicli::command(default)]
pub(crate) fn query(args: SearchArgs, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let no_stdin = args.no_stdin;
    let track_query = build_track_query(args);
    search_tracks(&library, &track_query, no_stdin)
}

/// Filter by playback history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayedFilter {
    /// Tracks that have never been played (started fact is absent).
    Never,
}

impl Choice for PlayedFilter {
    const ALL: &'static [Self] = &[PlayedFilter::Never];
    const WHAT: &'static str = "played filter";

    fn name(self) -> &'static str {
        match self {
            PlayedFilter::Never => "never",
        }
    }
}

impl std::fmt::Display for PlayedFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for PlayedFilter {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        parse_choice(s)
    }
}

/// Build a TrackQuery from the search arguments (`--no-stdin` is not part of the query).
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
        no_stdin: _,
        started,
        stopped,
        added,
        played,
        role,
        energy,
        not,
        query: any_text,
    } = args;
    let started = if matches!(played, Some(PlayedFilter::Never)) {
        // --played=never is a shortcut for started=N/A and overrides any --started value.
        Some(DateQuery::NA)
    } else {
        started.map(|DateArg(q)| q)
    };
    TrackQuery {
        any_text: any_text.map(|s| parse_string_query(&s)),
        artist: artist.map(|s| parse_string_query(&s)),
        title: title.map(|s| parse_string_query(&s)),
        album: album.map(|s| parse_string_query(&s)),
        label: label.map(|s| parse_string_query(&s)),
        genre: genre.map(|s| parse_string_query(&s)),
        style: style.map(|s| parse_string_query(&s)),
        bpm: bpm.map(|BpmArg(q)| q),
        key: key.map(|KeyArg(q)| q),
        duration: duration.map(|DurationArg(q)| q),
        year: year.map(|YearArg(q)| q),
        source,
        started,
        stopped: stopped.map(|DateArg(q)| q),
        added: added.map(|DateArg(q)| q),
        role: role.map(|RoleArg(r)| r),
        energy: energy.map(|EnergyArg(q)| q),
        not,
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

fn search_tracks(library: &LibraryBackend, query: &TrackQuery, no_stdin: bool) -> Result<()> {
    use std::io::IsTerminal;

    // When stdin is piped (and --no-stdin is not set), read the hashes in it (the same
    // rule as hashes given as arguments) as an intersection filter.
    let stdin_filter: Option<Vec<ContentHash>> = if no_stdin { None } else { piped_hashes() };

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
/// Examples:
///   mdma search fact-values-for genre
///   mdma search fact-values-for label | grep -i "ost"
///   mdma search fact-values-for genre | dmenu | xargs -I{} mdma search --genre {}
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
    use crate::test_support::{parses, track, usage_error};

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
    fn invalid_filters_carry_the_from_str_message() {
        let reason = "garbage".parse::<BpmArg>().unwrap_err().to_string();
        let err = run_arg::<BpmArg>(&["--bpm", "garbage"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        let reason = "garbage".parse::<YearArg>().unwrap_err().to_string();
        let err = run_arg::<YearArg>(&["--year", "garbage"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        let reason = "garbage".parse::<KeyArg>().unwrap_err().to_string();
        let err = run_arg::<KeyArg>(&["--key", "garbage"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        let reason = "garbage".parse::<DurationArg>().unwrap_err().to_string();
        let err = run_arg::<DurationArg>(&["--duration", "garbage"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        let reason = "garbage".parse::<RoleArg>().unwrap_err().to_string();
        let err = run_arg::<RoleArg>(&["--role", "garbage"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        let err = run_arg::<EnergyArg>(&["--energy", "11"]).unwrap_err();
        assert!(err.contains("invalid energy '11'"), "{err}");
    }

    #[test]
    fn key_duration_and_role_parsers_accept_valid_values() {
        assert!(run_arg::<KeyArg>(&["--key", "Am"]).is_ok());
        assert!(run_arg::<DurationArg>(&["--duration", ">5m"]).is_ok());
        assert!(matches!(
            run_arg::<RoleArg>(&["--role", "peak"]),
            Ok(RoleArg(TrackRole::Peak))
        ));
    }

    #[test]
    fn played_filter_parser_accepts_only_never() {
        assert_eq!(
            run_arg::<PlayedFilter>(&["--played", "never"]),
            Ok(PlayedFilter::Never)
        );
        let err = run_arg::<PlayedFilter>(&["--played", "always"]).unwrap_err();
        assert!(
            err.contains("unknown played filter 'always' (expected never)"),
            "{err}"
        );
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
        assert!(err.contains("invalid date query 'garbage'"), "{err}");
    }

    #[test]
    fn search_started_and_stopped_garbage_are_parse_errors() {
        for flag in ["--started=garbage", "--stopped=garbage"] {
            let err = usage_error(&["search", flag]);
            assert!(
                err.contains("invalid date query 'garbage'"),
                "{flag}: {err}"
            );
        }
    }

    // ── role / energy parsing ────────────────────────────────────────────────

    #[test]
    fn search_role_parses_and_rejects() {
        let args = parse_search(&["--role", "peak"]).unwrap();
        assert!(args.role.is_some());
        let err = parse_search(&["--role", "dancer"]).unwrap_err();
        assert!(err.contains("invalid role 'dancer'"), "{err}");
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
        assert!(err.contains("invalid numeric query 'fast'"), "{err}");
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
        assert!(err.contains("invalid numeric query 'last-year'"), "{err}");
    }

    #[test]
    fn search_key_parses_into_key_query() {
        let args = parse_search(&["--key", "8B"]).unwrap();
        assert!(args.key.is_some(), "got {:?}", args.key);
    }

    #[test]
    fn search_key_invalid_is_parse_error() {
        let err = parse_search(&["--key", "H#"]).unwrap_err();
        assert!(err.contains("invalid key query 'H#'"), "{err}");
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
        assert!(err.contains("invalid duration query 'long'"), "{err}");
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
        for v in PlayedFilter::ALL {
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
            started: Some("-7".parse().unwrap()),
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
