use crate::ctx::Ctx;
use crate::output::{handle_error, print_tracks};
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::TrackInfo;
use library_search::{
    parse_date_query, parse_duration_query, parse_key_query, parse_numeric_query, parse_role_query,
    parse_string_query, DurationQuery, KeyQuery, NumericQuery, ParseError, TrackQuery,
};
use mdma_client::LibraryBackend;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// List all distinct values stored for a fact type.
    ///
    /// Examples:
    ///   mdma search fact-values-for genre
    ///   mdma search fact-values-for label | grep -i "ost"
    ///   mdma search fact-values-for genre | dmenu | xargs -I{} mdma search --genre {}
    FactValuesFor {
        /// Fact type to inspect (e.g. MainGenre, Label, Key, Source, BPM, StyleDescriptor)
        fact_type: String,
    },
}

// `mdma search`: a track query, or a subcommand.
#[derive(clap::Args, Debug)]
pub(crate) struct SearchCli {
    #[command(flatten)]
    pub(crate) query: SearchArgs,

    #[command(subcommand)]
    pub(crate) subcommand: Option<Command>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct SearchArgs {
    /// Filter by artist name
    #[arg(long)]
    artist: Option<String>,

    /// Filter by track title
    #[arg(long)]
    title: Option<String>,

    /// Filter by album name
    #[arg(long)]
    album: Option<String>,

    /// Filter by label name
    #[arg(long)]
    label: Option<String>,

    /// Filter by main genre (e.g. "Electronic", "Techno")
    #[arg(long)]
    genre: Option<String>,

    /// Filter by style descriptor — matches if any descriptor matches
    #[arg(long)]
    style: Option<String>,

    /// Filter by BPM. Formats: 128  128+-4  124..132  128+2  128-2
    #[arg(long)]
    bpm: Option<BpmArg>,

    /// Filter by musical key. Formats: Am  "A minor"  8B  8B+-1  8B+-1~
    #[arg(long)]
    key: Option<KeyArg>,

    /// Filter by duration. Formats: 7m  7m15s  >5m  <8m  6m..8m
    #[arg(long)]
    duration: Option<DurationArg>,

    /// Filter by release year. Formats: 2022  2019..2022
    #[arg(long)]
    year: Option<YearArg>,

    /// Filter by source (bandcamp, beatport, upload)
    #[arg(long)]
    source: Option<String>,

    /// Ignore stdin (don't read piped hashes as intersection filter)
    #[arg(long)]
    no_stdin: bool,

    /// Filter by last started date. Format: N/A, >2026-02, <2026, 2026-01..2026-06
    #[arg(long, allow_hyphen_values = true)]
    started: Option<String>,

    /// Filter by last stopped date. Same format as --started.
    #[arg(long, allow_hyphen_values = true)]
    stopped: Option<String>,

    /// Filter by date added to library. Uses date expression syntax: ~/+1/15 (15th next month), -7 (7 days ago), ^ (1st of month), $ (end of month). Ranges: -7..~ (last 7 days to today).
    #[arg(long, allow_hyphen_values = true)]
    added: Option<String>,

    /// Filter by playback history. `never` matches tracks with no started fact (never played).
    /// When combined with --started, --played=never takes precedence.
    #[arg(long)]
    played: Option<PlayedFilter>,

    /// Filter by DJ role. Accepted values: opener, build-up, peak, banger, cool-down, closer, filler
    #[arg(long)]
    role: Option<String>,

    /// Filter by energy level (1-10). Formats: 7  5..8  7+-2
    #[arg(long)]
    energy: Option<String>,

    /// Invert the search results — return tracks that do NOT match the filters.
    #[arg(long)]
    not: bool,

    /// Free-text query applied to all text fields (title, artist, album, label, genre).
    /// Supports CamelCase initialism (CarbBased) and /regex/ syntax.
    query: Option<String>,
}

/// A fact type name (e.g. MainGenre, Label, Key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactTypeArg(pub library_ipc_client::FactType);

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

pub(crate) fn dispatch(cli: SearchCli, ctx: &Ctx) -> Result<()> {
    let SearchCli {
        query: args,
        subcommand,
    } = cli;
    match subcommand {
        Some(Command::FactValuesFor { fact_type }) => fact_values_for(
            FactTypeArg(library_ipc_client::FactType::new(fact_type)),
            ctx,
        ),
        None => query(args, ctx),
    }
}

pub(crate) fn query(args: SearchArgs, ctx: &Ctx) -> Result<()> {
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
        no_stdin,
        started,
        stopped,
        added,
        played,
        role,
        energy,
        not,
        query,
    } = args;
    let library = ctx.library();
    let mut track_query = build_track_query(SearchFilters {
        any_text: query,
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
    });
    track_query.not = not;
    search_tracks(&library, &track_query, no_stdin)
}

/// Filter by playback history.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayedFilter {
    /// Tracks that have never been played (started fact is absent).
    Never,
}

impl std::fmt::Display for PlayedFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayedFilter::Never => f.write_str("never"),
        }
    }
}

impl std::str::FromStr for PlayedFilter {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "never" => Ok(PlayedFilter::Never),
            _ => Err(format!("unknown played filter '{s}'")),
        }
    }
}

/// The raw search filters taken from the command line, before parsing into a `TrackQuery`.
#[derive(Debug, Default)]
struct SearchFilters {
    any_text: Option<String>,
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    label: Option<String>,
    genre: Option<String>,
    style: Option<String>,
    bpm: Option<BpmArg>,
    key: Option<KeyArg>,
    duration: Option<DurationArg>,
    year: Option<YearArg>,
    source: Option<String>,
    started: Option<String>,
    stopped: Option<String>,
    added: Option<String>,
    played: Option<PlayedFilter>,
    role: Option<String>,
    energy: Option<String>,
}

/// Parse the value given for `flag`, if any; an invalid one is reported
/// (`Invalid <flag> value: <error>`) and exits 1.
fn parse_or_exit<T, E: std::fmt::Display>(
    value: Option<String>,
    flag: &str,
    parse: impl Fn(&str) -> std::result::Result<T, E>,
) -> Option<T> {
    value.map(|s| {
        parse(&s).unwrap_or_else(|e| {
            eprintln!("Invalid {} value: {}", flag, e);
            std::process::exit(1);
        })
    })
}

/// Build a TrackQuery from the search filters.
///
/// Precedence: `--played=never` overrides any explicit `--started` value.
fn build_track_query(filters: SearchFilters) -> TrackQuery {
    let SearchFilters {
        any_text,
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
        started: started_str,
        stopped: stopped_str,
        added: added_str,
        played,
        role: role_str,
        energy: energy_str,
    } = filters;
    let started = if matches!(played, Some(PlayedFilter::Never)) {
        // --played=never is a shortcut for started=N/A and overrides any --started value.
        Some(library_search::DateQuery::NA)
    } else {
        parse_or_exit(started_str, "--started", parse_date_query)
    };
    let stopped = parse_or_exit(stopped_str, "--stopped", parse_date_query);
    let added = parse_or_exit(added_str, "--added", parse_date_query);
    let role = parse_or_exit(role_str, "--role", parse_role_query);
    let energy = parse_or_exit(energy_str, "--energy", parse_numeric_query);
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
        stopped,
        added,
        role,
        energy,
        not: false,
    }
}

/// Returns true if a track's content hash matches any token in the set.
///
/// Both the track hash and the token are stripped of the "sha256:" prefix before
/// comparison, and a prefix match is used (like `git log <short-hash>`).
fn hash_matches(track_hash: &str, set: &std::collections::HashSet<String>) -> bool {
    let clean = track_hash.strip_prefix("sha256:").unwrap_or(track_hash);
    set.iter().any(|token| {
        let token_clean = token.strip_prefix("sha256:").unwrap_or(token.as_str());
        clean.starts_with(token_clean)
    })
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
    stdin: Option<&std::collections::HashSet<String>>,
    not: bool,
) -> Vec<TrackInfo> {
    match stdin {
        None => tracks,
        Some(set) => {
            if not {
                tracks
                    .into_iter()
                    .filter(|t| !hash_matches(t.content_hash.as_str(), set))
                    .collect()
            } else {
                tracks
                    .into_iter()
                    .filter(|t| hash_matches(t.content_hash.as_str(), set))
                    .collect()
            }
        }
    }
}

fn search_tracks(library: &LibraryBackend, query: &TrackQuery, no_stdin: bool) -> Result<()> {
    use std::collections::HashSet;
    use std::io::{BufRead, IsTerminal};

    // When stdin is piped (and --no-stdin is not set), read all hashes as an intersection filter.
    let stdin_filter: Option<HashSet<String>> = if !no_stdin && !std::io::stdin().is_terminal() {
        let tokens = std::io::stdin()
            .lock()
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| line.split_whitespace().next().map(|t| t.to_string()))
            .collect();
        Some(tokens)
    } else {
        None
    };

    // When stdin hashes are present and --not is set, we invert the intersection rather
    // than the library query: send the query without `.not` (so the library returns a
    // candidate set), then exclude any track whose hash is in the piped set.
    // Without stdin, `.not` flows through to the library as usual.
    let effective_not = query.not && stdin_filter.is_some();
    let mut effective_query = query.clone();
    if effective_not {
        effective_query.not = false;
    }

    match library.search(&effective_query) {
        Ok(tracks) => {
            let tracks = apply_stdin_filter(tracks, stdin_filter.as_ref(), effective_not);

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
        Err(e) => handle_error(e),
    }
}

pub(crate) fn fact_values_for(fact_type: FactTypeArg, ctx: &Ctx) -> Result<()> {
    use std::io::IsTerminal;
    let FactTypeArg(fact_type) = fact_type;
    let library = ctx.library();
    match library.get_fact_values(fact_type.as_str()) {
        Ok(values) => {
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
        Err(e) => handle_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::track;

    use clap::Parser;

    use library_ipc_client::TrackInfo;

    use crate::Cli;

    use pretty_assertions::assert_eq;

    // ── Date arg hyphen-value parsing ────────────────────────────────────────

    #[test]
    fn search_added_hyphen_value_parses() {
        // `mdma search --added -7` must not be rejected by clap as an unknown flag.
        let result = Cli::try_parse_from(["mdma", "search", "--added", "-7"]);
        assert!(
            result.is_ok(),
            "clap rejected --added -7: {}",
            result.unwrap_err()
        );
    }

    #[test]
    fn search_started_hyphen_value_parses() {
        let result = Cli::try_parse_from(["mdma", "search", "--started", "-7"]);
        assert!(
            result.is_ok(),
            "clap rejected --started -7: {}",
            result.unwrap_err()
        );
    }

    #[test]
    fn search_stopped_tilde_parses() {
        let result = Cli::try_parse_from(["mdma", "search", "--stopped", "~"]);
        assert!(
            result.is_ok(),
            "clap rejected --stopped ~: {}",
            result.unwrap_err()
        );
    }

    // ── bpm / key / duration / year parsing ──────────────────────────────────

    fn parse_search(args: &[&str]) -> Result<SearchArgs, clap::Error> {
        let argv = ["mdma", "search"].into_iter().chain(args.iter().copied());
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            crate::Commands::Search(search) => search.query,
            other => panic!("expected search command, got {other:?}"),
        })
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
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(err.to_string().contains("invalid numeric query 'fast'"));
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
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(err
            .to_string()
            .contains("invalid numeric query 'last-year'"));
    }

    #[test]
    fn search_key_parses_into_key_query() {
        let args = parse_search(&["--key", "8B"]).unwrap();
        assert!(args.key.is_some(), "got {:?}", args.key);
    }

    #[test]
    fn search_key_invalid_is_parse_error() {
        let err = parse_search(&["--key", "H#"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(err.to_string().contains("invalid key query 'H#'"));
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
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(err.to_string().contains("invalid duration query 'long'"));
    }

    #[test]
    fn search_key_and_duration_reach_the_built_query() {
        let args = parse_search(&["--key", "8B", "--duration", "<8m", "--bpm", "128"]).unwrap();
        let query = build_track_query(SearchFilters {
            bpm: args.bpm,
            key: args.key,
            duration: args.duration,
            ..Default::default()
        });
        assert!(query.key.is_some(), "key dropped from query");
        assert!(
            matches!(query.duration, Some(DurationQuery::AtMost(480))),
            "got {:?}",
            query.duration
        );
        assert!(query.bpm.is_some(), "bpm dropped from query");
    }

    // ── apply_stdin_filter tests ─────────────────────────────────────────────

    fn make_track_with_hash(hash: &str) -> TrackInfo {
        TrackInfo { ..track(hash) }
    }

    fn hash_set(hashes: &[&str]) -> std::collections::HashSet<String> {
        hashes.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn search_not_with_stdin_excludes_piped_hashes() {
        // stdin = {h1, h2}, library returns {h1, h2, h3, h4}, --not returns [h3, h4]
        let tracks = vec![
            make_track_with_hash("sha256:h1aaaaaa"),
            make_track_with_hash("sha256:h2aaaaaa"),
            make_track_with_hash("sha256:h3aaaaaa"),
            make_track_with_hash("sha256:h4aaaaaa"),
        ];
        let stdin = hash_set(&["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), true);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, ["sha256:h3aaaaaa", "sha256:h4aaaaaa"]);
    }

    #[test]
    fn search_not_with_stdin_prefix_match_excludes() {
        // stdin token is a short prefix (no sha256: prefix on token)
        let tracks = vec![
            make_track_with_hash("sha256:abcdef1234"),
            make_track_with_hash("sha256:deadbeef56"),
            make_track_with_hash("sha256:cafebabe78"),
        ];
        let stdin = hash_set(&["abcdef", "deadbe"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), true);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, ["sha256:cafebabe78"]);
    }

    #[test]
    fn search_stdin_without_not_intersects() {
        // Sanity: stdin + no --not still intersects
        let tracks = vec![
            make_track_with_hash("sha256:h1aaaaaa"),
            make_track_with_hash("sha256:h2aaaaaa"),
            make_track_with_hash("sha256:h3aaaaaa"),
        ];
        let stdin = hash_set(&["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), false);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, ["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
    }

    #[test]
    fn search_no_stdin_preserves_not_behavior() {
        // Without stdin, apply_stdin_filter returns tracks unchanged regardless of `not`
        let tracks = vec![
            make_track_with_hash("sha256:h1aaaaaa"),
            make_track_with_hash("sha256:h2aaaaaa"),
        ];
        let result = apply_stdin_filter(tracks.clone(), None, true);
        assert_eq!(result.len(), 2, "pass-through when no stdin, not=true");
        let result2 = apply_stdin_filter(tracks, None, false);
        assert_eq!(result2.len(), 2, "pass-through when no stdin, not=false");
    }

    // ── --played=never filter ─────────────────────────────────────────────────

    #[test]
    fn played_filter_matches_clap_value_enum() {
        use clap::ValueEnum;
        for v in PlayedFilter::value_variants() {
            let name = v.to_possible_value().unwrap().get_name().to_string();
            assert_eq!(name.parse::<PlayedFilter>(), Ok(*v));
            assert_eq!(v.to_string(), name);
        }
        assert!("nope".parse::<PlayedFilter>().is_err());
    }

    #[test]
    fn search_played_never_maps_to_started_na() {
        use library_search::DateQuery;
        let result = Cli::try_parse_from(["mdma", "search", "--played", "never"]);
        assert!(
            result.is_ok(),
            "clap rejected --played never: {}",
            result.unwrap_err()
        );
        // Build the query with played=never and no explicit --started
        let query = build_track_query(SearchFilters {
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
        use library_search::DateQuery;
        // When --played=never and --started are both given, --played=never wins.
        let query = build_track_query(SearchFilters {
            started: Some("-7".to_string()),
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
