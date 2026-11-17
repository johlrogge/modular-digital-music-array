//! `mdma plays`: play history, read from the local fact replica (#99, ADR-021).
//!
//! Nothing here counts or resolves anything. `client-facts` folds the plays and
//! resolves a partial hash ([`Facts::play_histories`](client_facts::Facts)), with the
//! same functions the library service uses; this module parses the command line and
//! lays the answer out.

use std::fmt::Display;
use std::num::NonZeroUsize;
use std::str::FromStr;

use bpaf::{construct, Parser};
use chrono::{DateTime, Local, TimeZone, Utc};
use client_facts::{Play, PlayHistory, RecentPlay, TrackPlays};
use color_eyre::Result;
use hicli::Arg;
use library_ipc_client::ContentHash;
use music_facts::{StartReason, StopReason};

use crate::args::{parse_count, HashInput};
use crate::ctx::Ctx;
use crate::output::each_track;
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
    ctx.read(|facts| match of {
        PlaysOf::Track(hash) => {
            let hashes = hashes_of_input(hash)?;
            // Resolved once, however many tracks are asked for; each answered or
            // refused on its own.
            let mut histories = facts.play_histories(&hashes)?.into_iter();
            let mut first = true;
            each_track(hashes, |_| {
                let plays = histories.next().expect("one answer per hash")?;
                if !std::mem::take(&mut first) {
                    println!();
                }
                println!("{}", render_track(&plays, &Local));
                Ok::<(), client_facts::FactsError>(())
            })
        }
        PlaysOf::Recent(RecentCount(n)) => {
            let recent = facts.recent_plays(n.get())?;
            println!("{}", render_recent(&recent, &Local));
            Ok(hicli::Success::OK)
        }
    })
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
fn label(hash: &ContentHash, artist: Option<&str>, title: Option<&str>) -> String {
    format!(
        "{}  {} - {}",
        track_formatter::short_hash(hash),
        artist.unwrap_or(MISSING),
        title.unwrap_or(MISSING)
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
fn render_track<Tz: TimeZone>(track: &TrackPlays, tz: &Tz) -> String
where
    Tz::Offset: Display,
{
    let history: &PlayHistory = &track.history;
    let time = |at: Option<DateTime<Utc>>| at.map_or(MISSING.to_string(), |at| when(at, tz));
    let count = history.count();
    let plural = if count == 1 { "play" } else { "plays" };
    let mut lines = vec![
        label(
            &track.content_hash,
            track.artist.as_deref(),
            track.title.as_deref(),
        ),
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
fn render_recent<Tz: TimeZone>(recent: &[RecentPlay], tz: &Tz) -> String
where
    Tz::Offset: Display,
{
    recent
        .iter()
        .map(|recent| {
            let play = &recent.play;
            let columns = Columns::of(play);
            format!(
                "{}  {}  {}/{}",
                when(play.at(), tz),
                label(
                    &recent.content_hash,
                    recent.artist.as_deref(),
                    recent.title.as_deref()
                ),
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
    use crate::test_support::{help, parses, usage_error};
    use chrono::FixedOffset;
    use client_facts::Stop;
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

    // --- the layout ----------------------------------------------------------
    //
    // What the plays are (the lineage of a merge, the pairing of starts and stops,
    // the resolution of a hash) is `client-facts`' and tested there.

    const WINNER: &str = "sha256:aabbccddeeff00112233445566778899";
    const OTHER: &str = "sha256:cc00cc00cc00cc00";

    /// A time in January 2026.
    fn t(day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, day, hour, min, 0).unwrap()
    }

    fn stop(at: DateTime<Utc>, reason: StopReason) -> Stop {
        Stop { at, reason }
    }

    /// Three plays: a stop-only one, a start another start cut off, and a complete one.
    fn blue_monday(plays: Vec<Play>) -> TrackPlays {
        TrackPlays {
            content_hash: ContentHash::new(WINNER),
            artist: Some("New Order".to_string()),
            title: Some("Blue Monday".to_string()),
            history: PlayHistory { plays },
        }
    }

    fn three_plays() -> Vec<Play> {
        vec![
            Play::StopOnly(stop(t(3, 11, 0), StopReason::OnSkip)),
            Play::Started {
                at: t(5, 12, 0),
                reason: StartReason::OnRequest,
                stop: None,
            },
            Play::Started {
                at: t(9, 13, 0),
                reason: StartReason::ByQueue,
                stop: Some(stop(t(9, 13, 30), StopReason::OnCompletion)),
            },
        ]
    }

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    #[test]
    fn a_track_shows_its_plays_newest_first() {
        let shown = render_track(&blue_monday(three_plays()), &utc());

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
        let quiet = TrackPlays {
            artist: None,
            title: Some("Quiet".to_string()),
            ..blue_monday(vec![])
        };

        let shown = render_track(&quiet, &utc());

        assert_eq!(shown, "aabbccddeeff  - - Quiet\n0 plays, last -");
    }

    #[test]
    fn one_play_is_not_plural() {
        let once = Play::Started {
            at: t(2, 8, 0),
            reason: StartReason::OnRequest,
            stop: None,
        };

        let shown = render_track(&blue_monday(vec![once]), &utc());

        assert!(shown.contains("\n1 play, last"), "{shown}");
    }

    #[test]
    fn times_are_shown_in_the_zone_given() {
        let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();

        let shown = render_track(&blue_monday(three_plays()), &plus_two);

        let newest = shown.lines().nth(2).unwrap();
        assert_eq!(
            newest,
            "2026-01-09 15:00  2026-01-09 15:30  ByQueue    OnCompletion"
        );
    }

    #[test]
    fn recent_lists_one_row_per_play_with_its_track() {
        let play_of = |hash: &str, artist: &str, title: &str, play: Play| RecentPlay {
            content_hash: ContentHash::new(hash),
            artist: Some(artist.to_string()),
            title: Some(title.to_string()),
            play,
        };
        let plays = three_plays();
        let recent = vec![
            play_of(WINNER, "New Order", "Blue Monday", plays[2]),
            play_of(
                OTHER,
                "deadmau5",
                "Strobe",
                Play::Started {
                    at: t(7, 9, 15),
                    reason: StartReason::ByQueue,
                    stop: Some(stop(t(7, 9, 20), StopReason::OnSkip)),
                },
            ),
            play_of(WINNER, "New Order", "Blue Monday", plays[1]),
            play_of(WINNER, "New Order", "Blue Monday", plays[0]),
        ];

        let shown = render_recent(&recent, &utc());

        assert_eq!(
            shown,
            "2026-01-09 13:00  aabbccddeeff  New Order - Blue Monday  ByQueue/OnCompletion\n\
             2026-01-07 09:15  cc00cc00cc00  deadmau5 - Strobe  ByQueue/OnSkip\n\
             2026-01-05 12:00  aabbccddeeff  New Order - Blue Monday  OnRequest/-\n\
             2026-01-03 11:00  aabbccddeeff  New Order - Blue Monday  -/OnSkip"
        );
    }
}
