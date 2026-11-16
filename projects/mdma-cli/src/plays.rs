//! `mdma plays`: play history, read from the local fact replica (#99, ADR-021).
//!
//! Nothing here counts or resolves anything. `library_projection` folds the plays
//! ([`PlayLog`](library_projection::PlayLog)) and resolves a partial hash
//! ([`resolve_prefix`]), with the same functions the library service uses; this module
//! parses the command line and lays the answer out.

use std::fmt::Display;
use std::num::NonZeroUsize;
use std::str::FromStr;

use bpaf::{construct, Parser};
use chrono::{DateTime, Local, TimeZone, Utc};
use color_eyre::Result;
use fact_replica::UnixSecs;
use hicli::Arg;
use library_ipc_client::ContentHash;
use library_projection::{
    ambiguity_message, resolve_prefix, IndexedTrackInfo, Play, PlayHistory, Projection,
    ResolvedPlays, Unresolved,
};
use music_facts::{StartReason, StopReason};

use crate::args::{parse_count, HashInput};
use crate::ctx::Ctx;
use crate::output::{each_track, TrackFailure};
use crate::replica::{ago, fold_current, ReplicaFold};
use crate::stdin::hashes_of_input;

/// List the N latest plays of any track, instead of one track's history
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "recent", metavar = "N")]
pub(crate) struct RecentCount(pub NonZeroUsize);

impl FromStr for RecentCount {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_count(s).map(Self)
    }
}

/// What `plays` shows: one track's history, or the latest plays of every track.
/// Exactly one of the two, decided at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaysOf {
    /// `HASH`, or `-` for the hashes on stdin.
    Track(HashInput),
    /// `--recent N`.
    Recent(RecentCount),
}

impl Arg for PlaysOf {
    /// `HASH|-` or `--recent N`.
    fn parser() -> impl Parser<Self> {
        let track = HashInput::alternative(HashInput::HELP).map(Self::Track);
        let recent = <RecentCount as Arg>::parser().map(Self::Recent);
        construct!([track, recent])
    }
}

/// Show when a track was played
///
/// Reads the local replica of the node's facts (see `mdma replica status`), so it
/// works offline and shows what the last sync brought. A track's plays include those
/// of the duplicates merged into it. Give `-` to read hashes from stdin; a hash the
/// replica does not have is reported on stderr, the rest are shown, and the exit code
/// is 1. With `--recent N`, list the N latest plays of any track instead.
///
/// # Examples
///
/// ```sh
/// mdma plays abcd1234
/// mdma search --artist CBL | mdma plays -
/// mdma plays --recent 20
/// ```
#[hicli::command]
pub(crate) fn plays(of: PlaysOf, ctx: &Ctx) -> Result<hicli::Success> {
    let fold = fold_current(ctx)?;
    for note in replica_notes(&fold, UnixSecs::now()) {
        eprintln!("{note}");
    }
    match of {
        PlaysOf::Track(hash) => {
            let hashes = hashes_of_input(hash)?;
            // Resolved once, however many tracks are asked for.
            let resolved = fold.projection.plays.resolved(&fold.projection.tracks);
            let mut first = true;
            each_track(hashes, |hash| {
                let text = show_track(&fold.projection, &resolved, hash, &Local)?;
                if !std::mem::take(&mut first) {
                    println!();
                }
                println!("{text}");
                Ok::<(), TrackFailure>(())
            })
        }
        PlaysOf::Recent(RecentCount(n)) => {
            println!("{}", show_recent(&fold.projection, n, &Local));
            Ok(hicli::Success::OK)
        }
    }
}

/// What to say on stderr about the replica the answer comes from: which node, how old,
/// and any lines it could not read.
fn replica_notes(fold: &ReplicaFold, now: UnixSecs) -> Vec<String> {
    let synced = ago(now, fold.synced_at);
    let mut notes = vec![format!("replica of {}, synced {synced}", fold.id)];
    let unreadable = fold.projection.unparseable_lines;
    if unreadable > 0 {
        let unread =
            format!("{unreadable} lines this mdma cannot read; plays may be missing; upgrade mdma");
        notes.push(unread);
    }
    notes
}

/// The history of the track `hash` names, laid out; or why it cannot be shown.
/// `resolved` is the projection's plays, resolved once for all the tracks asked for.
fn show_track<Tz: TimeZone>(
    projection: &Projection,
    resolved: &ResolvedPlays,
    hash: &ContentHash,
    tz: &Tz,
) -> Result<String, TrackFailure>
where
    Tz::Offset: Display,
{
    let found = resolve_prefix(&projection.tracks, hash.as_str());
    let track = found.map_err(|unresolved| match unresolved {
        Unresolved::NotFound => TrackFailure::Said("not in the replica".to_string()),
        Unresolved::Ambiguous(tracks) => {
            TrackFailure::Said(ambiguity_message(hash.as_str(), &tracks))
        }
    })?;
    Ok(render_track(track, &resolved.history(track), tz))
}

/// The `n` latest plays of any track, laid out.
fn show_recent<Tz: TimeZone>(projection: &Projection, n: NonZeroUsize, tz: &Tz) -> String
where
    Tz::Offset: Display,
{
    let tracks = &projection.tracks;
    let recent = projection.plays.recent(tracks, n.get());
    render_recent(tracks, &recent, tz)
}

/// How a missing value is written.
const MISSING: &str = "-";

/// A time as `2026-01-31 14:05` in `tz`.
fn when<Tz: TimeZone>(at: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: Display,
{
    at.with_timezone(tz).format("%Y-%m-%d %H:%M").to_string()
}

/// `{short}  {artist} - {title}`.
fn label(track: &IndexedTrackInfo) -> String {
    format!(
        "{}  {} - {}",
        track_formatter::short_hash(&track.content_hash),
        track.artist.as_deref().unwrap_or(MISSING),
        track.title.as_deref().unwrap_or(MISSING)
    )
}

/// One play, as the values of its columns; `None` is a missing value.
struct Columns {
    start: Option<DateTime<Utc>>,
    stop: Option<DateTime<Utc>>,
    start_reason: Option<StartReason>,
    stop_reason: Option<StopReason>,
}

impl Columns {
    fn of(play: &Play) -> Self {
        match play {
            Play::Started { at, reason, stop } => Columns {
                start: Some(*at),
                stop: stop.map(|s| s.at),
                start_reason: Some(*reason),
                stop_reason: stop.map(|s| s.reason),
            },
            Play::StopOnly(stop) => Columns {
                start: None,
                stop: Some(stop.at),
                start_reason: None,
                stop_reason: Some(stop.reason),
            },
        }
    }

    fn start_reason(&self) -> String {
        self.start_reason
            .map_or(MISSING.to_string(), |r| r.to_string())
    }

    fn stop_reason(&self) -> String {
        self.stop_reason
            .map_or(MISSING.to_string(), |r| r.to_string())
    }
}

/// One track's history: its label, how many plays and the latest, then a row per play,
/// newest first: `start stop start-reason stop-reason`.
fn render_track<Tz: TimeZone>(track: &IndexedTrackInfo, history: &PlayHistory, tz: &Tz) -> String
where
    Tz::Offset: Display,
{
    let time = |at: Option<DateTime<Utc>>| at.map_or(MISSING.to_string(), |at| when(at, tz));
    let count = history.count();
    let plural = if count == 1 { "play" } else { "plays" };
    let mut lines = vec![
        label(track),
        format!("{count} {plural}, last {}", time(history.last_played())),
    ];
    for play in history.plays.iter().rev() {
        let columns = Columns::of(play);
        // 16 is the width of a time, 9 the width of the longest start reason.
        lines.push(format!(
            "{:<16}  {:<16}  {:<9}  {}",
            time(columns.start),
            time(columns.stop),
            columns.start_reason(),
            columns.stop_reason()
        ));
    }
    lines.join("\n")
}

/// The latest plays, one row each: `time short artist - title start-reason/stop-reason`.
/// `recent` pairs a play with the index of its track in `tracks`.
fn render_recent<Tz: TimeZone>(
    tracks: &[IndexedTrackInfo],
    recent: &[(usize, Play)],
    tz: &Tz,
) -> String
where
    Tz::Offset: Display,
{
    recent
        .iter()
        .map(|(i, play)| {
            let columns = Columns::of(play);
            format!(
                "{}  {}  {}/{}",
                when(play.at(), tz),
                label(&tracks[*i]),
                columns.start_reason(),
                columns.stop_reason()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{asserted, help, parses, usage_error};
    use chrono::FixedOffset;
    use fact_replica::NodeId;
    use library_projection::project_lines;
    use pretty_assertions::assert_eq;

    // --- the command line ----------------------------------------------------

    #[test]
    fn plays_takes_a_hash_a_dash_or_recent() {
        assert_eq!(parses(&["plays", "abcd1234"]), ["plays"]);
        assert_eq!(parses(&["plays", "sha256:abcd1234"]), ["plays"]);
        assert_eq!(parses(&["plays", "-"]), ["plays"]);
        assert_eq!(parses(&["plays", "--recent", "5"]), ["plays"]);
        assert_eq!(parses(&["plays", "--recent=20"]), ["plays"]);
    }

    #[test]
    fn plays_needs_exactly_one_of_a_track_and_recent() {
        let neither = usage_error(&["plays"]);
        assert!(neither.contains("HASH|-"), "{neither}");
        usage_error(&["plays", "-", "-"]);
        usage_error(&["plays", "abcd1234", "abcd5678"]);
        usage_error(&["plays", "--recent", "3", "-"]);
        usage_error(&["plays", "-", "--recent", "3"]);
        usage_error(&["plays", "abcd1234", "--recent", "3"]);
    }

    #[test]
    fn recent_is_a_whole_number_of_at_least_one() {
        for bad in ["0", "-1", "x", "1.5"] {
            let err = usage_error(&["plays", &format!("--recent={bad}")]);
            assert!(err.contains("`--recent`"), "{bad}: {err}");
            assert!(err.contains("at least 1"), "{bad}: {err}");
        }
        let missing = usage_error(&["plays", "--recent"]);
        assert!(missing.contains("--recent"), "{missing}");
    }

    #[test]
    fn a_hash_is_validated_at_parse_time() {
        let err = usage_error(&["plays", "xyz"]);
        assert!(err.contains("not a content hash"), "{err}");
    }

    #[test]
    fn help_shows_both_forms_and_examples() {
        let text = help(&["plays", "--help"]);
        assert!(text.contains("HASH|-"), "{text}");
        assert!(text.contains("--recent=N"), "{text}");
        assert!(text.contains("Examples:"), "{text}");
        assert!(text.contains("mdma plays --recent 20"), "{text}");
        assert!(!text.contains("```"), "{text}");
    }

    // --- a replica to read ---------------------------------------------------

    const WINNER: &str = "sha256:aabbccddeeff00112233445566778899";
    const LOSER: &str = "sha256:99887766554433221100ffeeddccbbaa";
    const OTHER: &str = "sha256:cc00cc00cc00cc00";

    /// A time in January 2026, as the log writes it.
    fn t(day: u32, hour: u32, min: u32) -> String {
        format!("2026-01-{day:02}T{hour:02}:{min:02}:00Z")
    }

    fn field(entity: &str, name: &str, value: &str, time: &str) -> String {
        let json = format!(r#"{{"t":"{name}","v":"{value}"}}"#);
        asserted(entity, &json, time)
    }

    fn started(entity: &str, reason: &str, time: &str) -> String {
        field(entity, "TrackStarted", reason, time)
    }

    fn stopped(entity: &str, reason: &str, time: &str) -> String {
        field(entity, "TrackStopped", reason, time)
    }

    /// Two tracks. The winner replaced the loser, which is gone, and carries its play:
    /// three plays in all (a stop-only one, a start another start cut off, and a
    /// complete one). The other track was played once.
    fn log() -> Vec<String> {
        let replaces = format!(r#"{{"t":"Replaces","v":"{LOSER}"}}"#);
        vec![
            field(WINNER, "Artist", "New Order", &t(1, 10, 0)),
            field(WINNER, "Title", "Blue Monday", &t(1, 10, 0)),
            field(OTHER, "Artist", "deadmau5", &t(1, 10, 0)),
            field(OTHER, "Title", "Strobe", &t(1, 10, 0)),
            stopped(LOSER, "OnSkip", &t(3, 11, 0)),
            asserted(WINNER, &replaces, &t(2, 10, 0)),
            started(WINNER, "OnRequest", &t(5, 12, 0)),
            started(WINNER, "ByQueue", &t(9, 13, 0)),
            stopped(WINNER, "OnCompletion", &t(9, 13, 30)),
            started(OTHER, "ByQueue", &t(7, 9, 15)),
            stopped(OTHER, "OnSkip", &t(7, 9, 20)),
        ]
    }

    fn project(lines: &[String]) -> Projection {
        project_lines(lines.iter().map(String::as_str))
    }

    fn projection() -> Projection {
        let projection = project(&log());
        assert_eq!(projection.unparseable_lines, 0, "the fixture lines parse");
        projection
    }

    /// `show_track`, with the plays resolved for this one track.
    fn track_text(
        projection: &Projection,
        hash: &ContentHash,
        tz: &FixedOffset,
    ) -> Result<String, TrackFailure> {
        let resolved = projection.plays.resolved(&projection.tracks);
        show_track(projection, &resolved, hash, tz)
    }

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn said(result: Result<String, TrackFailure>) -> String {
        match result {
            Err(TrackFailure::Said(text)) => text,
            other => panic!("expected a failure with a text, got {other:?}"),
        }
    }

    // --- one track -----------------------------------------------------------

    #[test]
    fn a_track_shows_its_plays_newest_first_with_those_inherited_from_a_merge() {
        let hash = ContentHash::new("aabbccdd");

        let shown = track_text(&projection(), &hash, &utc()).unwrap();

        assert_eq!(
            shown,
            "aabbccddeeff  New Order - Blue Monday\n\
             3 plays, last 2026-01-09 13:00\n\
             2026-01-09 13:00  2026-01-09 13:30  ByQueue    OnCompletion\n\
             2026-01-05 12:00  -                 OnRequest  -\n\
             -                 2026-01-03 11:00  -          OnSkip"
        );
    }

    #[test]
    fn a_track_with_no_plays_says_so() {
        let projection = project(&[field(WINNER, "Title", "Quiet", &t(1, 10, 0))]);
        let hash = ContentHash::new(WINNER);

        let shown = track_text(&projection, &hash, &utc()).unwrap();

        assert_eq!(shown, "aabbccddeeff  - - Quiet\n0 plays, last -");
    }

    #[test]
    fn one_play_is_not_plural() {
        let projection = project(&[
            field(WINNER, "Title", "Once", &t(1, 10, 0)),
            started(WINNER, "OnRequest", &t(2, 8, 0)),
        ]);
        let hash = ContentHash::new(WINNER);

        let shown = track_text(&projection, &hash, &utc()).unwrap();

        assert!(shown.contains("\n1 play, last"), "{shown}");
    }

    #[test]
    fn times_are_shown_in_the_zone_given() {
        let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
        let hash = ContentHash::new("aabbccdd");

        let shown = track_text(&projection(), &hash, &plus_two).unwrap();

        let newest = shown.lines().nth(2).unwrap();
        assert_eq!(
            newest,
            "2026-01-09 15:00  2026-01-09 15:30  ByQueue    OnCompletion"
        );
    }

    #[test]
    fn a_hash_the_replica_lacks_is_not_in_the_replica() {
        let hash = ContentHash::new("12345678");

        let missing = track_text(&projection(), &hash, &utc());

        assert_eq!(said(missing), "not in the replica");
    }

    #[test]
    fn a_hash_two_tracks_share_is_ambiguous_and_says_how_many() {
        let projection = project(&[
            field("sha256:abcdef0011223344", "Title", "A", &t(1, 10, 0)),
            field("sha256:abcdef0099887766", "Title", "B", &t(1, 10, 0)),
        ]);
        let hash = ContentHash::new("abcdef00");

        let ambiguous = track_text(&projection, &hash, &utc());

        let text = said(ambiguous);
        assert!(text.contains("matches 2 tracks"), "{text}");
    }

    // --- recent --------------------------------------------------------------

    #[test]
    fn recent_lists_the_latest_plays_of_every_track_newest_first() {
        let n = NonZeroUsize::new(10).unwrap();

        let shown = show_recent(&projection(), n, &utc());

        assert_eq!(
            shown,
            "2026-01-09 13:00  aabbccddeeff  New Order - Blue Monday  ByQueue/OnCompletion\n\
             2026-01-07 09:15  cc00cc00cc00  deadmau5 - Strobe  ByQueue/OnSkip\n\
             2026-01-05 12:00  aabbccddeeff  New Order - Blue Monday  OnRequest/-\n\
             2026-01-03 11:00  aabbccddeeff  New Order - Blue Monday  -/OnSkip"
        );
    }

    #[test]
    fn recent_stops_after_n_plays() {
        let n = NonZeroUsize::new(2).unwrap();

        let shown = show_recent(&projection(), n, &utc());

        assert_eq!(shown.lines().count(), 2, "{shown}");
        assert!(shown.starts_with("2026-01-09 13:00"), "{shown}");
    }

    // --- the replica note ----------------------------------------------------

    fn fold_of(lines: &[String]) -> ReplicaFold {
        ReplicaFold {
            id: NodeId::new("mdma-stage").unwrap(),
            synced_at: Some(UnixSecs::new(1_000)),
            projection: project(lines),
        }
    }

    #[test]
    fn the_replica_is_named_and_its_age_given_on_stderr() {
        let fold = fold_of(&log());

        let notes = replica_notes(&fold, UnixSecs::new(1_000 + 7_200));

        assert_eq!(notes, ["replica of mdma-stage, synced 2 h ago"]);
    }

    #[test]
    fn lines_this_mdma_cannot_read_are_reported_too() {
        let mut lines = log();
        lines.push(r#"["from-a-newer-node"]"#.to_string());
        lines.push("garbage".to_string());

        let notes = replica_notes(&fold_of(&lines), UnixSecs::new(1_005));

        assert_eq!(
            notes,
            [
                "replica of mdma-stage, synced 5 s ago",
                "2 lines this mdma cannot read; plays may be missing; upgrade mdma"
            ]
        );
    }

    #[test]
    fn a_replica_that_never_synced_says_never() {
        let mut fold = fold_of(&[]);
        fold.synced_at = None;

        let notes = replica_notes(&fold, UnixSecs::new(5));

        assert_eq!(notes, ["replica of mdma-stage, synced never"]);
    }
}
