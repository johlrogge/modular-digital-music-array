//! What `mdma playback seek` takes and says (ADR-031).
//!
//! The rules (snapping, stepping, the missing-grid refusal, keeping a position
//! within the track) live in `mdma_client::seek` and the engine; this module
//! parses the command line and says what happened. The command itself is in
//! the `playback` group, next to stop, pause and resume.

use std::str::FromStr;

use color_eyre::Result;
use hicli::Arg;
use mdma_client::{
    Deck, LibraryBackend, NodeError, Placement, PlaybackBackend, PositionMs, TransportStatus,
};

use crate::output::display_error;

/// Where to seek: a time (`1:30`, `1:02:03`, or seconds), or beats from here
/// (`+8b`, `-8b`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(positional = "TIME|+Nb|-Nb")]
pub(crate) struct SeekArg(pub SeekTarget);

/// What a seek asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekTarget {
    /// That time into the track.
    At(PositionMs),
    /// This many beats from the nearest beat (negative goes back).
    Beats(i32),
}

const SEEK_HINT: &str = "expected a time like 1:30 or 90, or beats like +8b or -8b";

impl FromStr for SeekArg {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_suffix('b') {
            Some(beats) => parse_beats(beats).map(SeekTarget::Beats),
            None => parse_time(s).map(SeekTarget::At),
        }
        .map(Self)
    }
}

/// `+8` or `-8`: the sign is required, so that a bare number is never beats.
fn parse_beats(s: &str) -> Result<i32, &'static str> {
    let digits = s.strip_prefix(['+', '-']).ok_or(SEEK_HINT)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SEEK_HINT);
    }
    s.strip_prefix('+')
        .unwrap_or(s)
        .parse()
        .map_err(|_| "too many beats")
}

/// `S`, `M:SS` or `H:MM:SS`, in whole seconds. Every field but the first is
/// below 60.
fn parse_time(s: &str) -> Result<PositionMs, &'static str> {
    let fields = s
        .split(':')
        .map(|f| {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return Err(SEEK_HINT);
            }
            f.parse::<u64>().map_err(|_| "too long a time")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if fields.len() > 3 || fields.iter().skip(1).any(|&f| f >= 60) {
        return Err(SEEK_HINT);
    }
    let seconds = fields.iter().try_fold(0u64, |total, &f| {
        total.checked_mul(60).and_then(|t| t.checked_add(f))
    });
    seconds
        .and_then(|s| s.checked_mul(1000))
        .map(PositionMs::new)
        .ok_or("too long a time")
}

/// `1:30`, or `1:02:03` from an hour on.
fn clock(position: PositionMs) -> String {
    let seconds = position.as_millis() / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Do the seek on deck A and say where the track went. A typed time is free
/// placement (the equivalent of Alt on the console's timeline): exactly where
/// asked, without the library. Beats need the library, for the beat grid.
pub(crate) fn run(
    playback: &PlaybackBackend,
    library: impl FnOnce() -> Result<LibraryBackend, NodeError>,
    target: SeekTarget,
) -> Result<String> {
    let said = match target {
        SeekTarget::At(position) => {
            let status =
                mdma_client::seek_to(playback, library, Deck::A, position, Placement::Free, None)
                    .map_err(display_error)?;
            format!("Seeked{}", to(&status))
        }
        SeekTarget::Beats(beats) => {
            let status = mdma_client::skip_beats(playback, library, Deck::A, beats, None)
                .map_err(display_error)?;
            format!("Seeked {beats:+} beats{}", to(&status))
        }
    };
    Ok(said)
}

/// ` to 1:30`: where the node says the track is now, if it said.
fn to(status: &TransportStatus) -> String {
    status
        .position()
        .map(|position| format!(" to {}", clock(position)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{parses, recorded, usage_error, FakeServices, Setup};
    use library_ipc_client::{BeatGridInfo, Bpm};
    use mdma_client::{Command, ContentHash, Transport};
    use pretty_assertions::assert_eq;

    const A: &str = "sha256:9fb4105e0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5";

    fn target(s: &str) -> Result<SeekTarget, &'static str> {
        s.parse::<SeekArg>().map(|SeekArg(t)| t)
    }

    fn at(ms: u64) -> Result<SeekTarget, &'static str> {
        Ok(SeekTarget::At(PositionMs::new(ms)))
    }

    #[test]
    fn times_are_seconds_minutes_or_hours() {
        assert_eq!(target("90"), at(90_000));
        assert_eq!(target("1:30"), at(90_000));
        assert_eq!(target("0:05"), at(5_000));
        assert_eq!(target("1:02:03"), at(3_723_000));
        assert_eq!(target("0"), at(0));
        assert_eq!(target("125"), at(125_000), "seconds need not be below 60");
    }

    #[test]
    fn beats_need_a_sign_and_a_b() {
        assert_eq!(target("+8b"), Ok(SeekTarget::Beats(8)));
        assert_eq!(target("-8b"), Ok(SeekTarget::Beats(-8)));
        assert_eq!(target("+0b"), Ok(SeekTarget::Beats(0)));
        assert_eq!(target("+32b"), Ok(SeekTarget::Beats(32)));
    }

    #[test]
    fn anything_else_is_refused_with_the_two_spellings() {
        for bad in [
            "", "b", "8b", "+b", "+-8b", "+8", "-8", "1:60", "1:2:3:4", ":30", "1:", "1.5", "a",
            "1:3x", "+8bb", "- 8b",
        ] {
            assert_eq!(target(bad), Err(SEEK_HINT), "{bad:?}");
        }
        assert_eq!(target("+99999999999b"), Err("too many beats"));
        assert_eq!(
            target("99999999999999999999999"),
            Err("too long a time"),
            "overflow is refused, not wrapped"
        );
    }

    #[test]
    fn the_command_line_accepts_both_spellings_under_playback() {
        let path = ["playback", "seek"];
        assert_eq!(parses(&["playback", "seek", "1:30"]), path);
        assert_eq!(parses(&["playback", "seek", "+8b"]), path);
        assert_eq!(parses(&["playback", "seek", "--", "-8b"]), path);
    }

    #[test]
    fn the_command_line_needs_a_target_and_refuses_a_bad_one() {
        usage_error(&["playback", "seek"]);
        let err = usage_error(&["playback", "seek", "soon"]);
        assert!(err.contains(SEEK_HINT), "{err}");
    }

    #[test]
    fn there_is_no_top_level_seek() {
        usage_error(&["seek", "1:30"]);
    }

    #[test]
    fn clock_shows_minutes_and_hours() {
        assert_eq!(clock(PositionMs::new(90_000)), "1:30");
        assert_eq!(clock(PositionMs::new(5_900)), "0:05");
        assert_eq!(clock(PositionMs::new(3_723_000)), "1:02:03");
    }

    fn playing(position_ms: u64) -> TransportStatus {
        TransportStatus::Loaded {
            transport: Transport::Playing,
            now: ContentHash::new(A),
            position_ms: Some(PositionMs::new(position_ms)),
            duration_ms: Some(PositionMs::new(240_000)),
            preview: None,
        }
    }

    fn grid_120() -> BeatGridInfo {
        BeatGridInfo {
            first_beat_ms: 100,
            bpm: Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        }
    }

    fn seek_command(position_ms: u64) -> serde_json::Value {
        recorded(&Command::Seek {
            deck: Deck::A,
            position_ms: PositionMs::new(position_ms),
        })
    }

    #[test]
    fn a_typed_time_is_free_placement_and_never_asks_the_library() {
        let fake = FakeServices::start(Setup {
            seeked: Some(playing(90_000)),
            ..Setup::default()
        });
        let said = run(
            &fake.playback,
            || panic!("a time needs no beat grid"),
            SeekTarget::At(PositionMs::new(90_000)),
        )
        .unwrap();
        assert_eq!(said, "Seeked to 1:30");
        assert_eq!(fake.sent(), vec![seek_command(90_000)]);
    }

    /// 99:00 into a 4:00 track: the node keeps it in the track, and the
    /// command says where the track is, not what was typed.
    #[test]
    fn a_time_past_the_end_reports_where_the_track_went() {
        let fake = FakeServices::start(Setup {
            seeked: Some(playing(240_000)),
            ..Setup::default()
        });
        let said = run(
            &fake.playback,
            || panic!("a time needs no beat grid"),
            SeekTarget::At(PositionMs::new(5_940_000)),
        )
        .unwrap();
        assert_eq!(said, "Seeked to 4:00");
        assert_eq!(fake.sent(), vec![seek_command(5_940_000)]);
    }

    #[test]
    fn beats_step_from_the_nearest_beat_and_seek_there() {
        let fake = FakeServices::start(Setup {
            tracks: &[A],
            transport: Some(playing(1_230)),
            seeked: Some(playing(5_100)),
            grid: Some(grid_120()),
            ..Setup::default()
        });
        let said = run(
            &fake.playback,
            || Ok(fake.library.clone()),
            SeekTarget::Beats(8),
        )
        .unwrap();
        // 1_230 ms is nearest beat 2 (1_100 ms); +8 beats is beat 10, 5_100 ms.
        assert_eq!(said, "Seeked +8 beats to 0:05");
        assert_eq!(
            fake.sent(),
            vec![
                recorded(&Command::GetTransport { deck: Deck::A }),
                seek_command(5_100)
            ]
        );
    }

    /// The snap path, end to end: the time goes to the nearest beat of the
    /// playing track's grid.
    #[test]
    fn a_snapped_seek_goes_to_the_nearest_beat_of_the_playing_track() {
        let fake = FakeServices::start(Setup {
            tracks: &[A],
            transport: Some(playing(0)),
            seeked: Some(playing(1_100)),
            grid: Some(grid_120()),
            ..Setup::default()
        });
        let status = mdma_client::seek_to(
            &fake.playback,
            || Ok(fake.library.clone()),
            Deck::A,
            PositionMs::new(1_230),
            Placement::Snap,
            None,
        )
        .unwrap();
        assert_eq!(status.position(), Some(PositionMs::new(1_100)));
        assert_eq!(
            fake.sent(),
            vec![
                recorded(&Command::GetTransport { deck: Deck::A }),
                seek_command(1_100)
            ]
        );
    }

    #[test]
    fn a_snapped_seek_on_a_track_without_a_grid_goes_where_asked() {
        let fake = FakeServices::start(Setup {
            tracks: &[A],
            transport: Some(playing(0)),
            seeked: Some(playing(1_230)),
            ..Setup::default()
        });
        mdma_client::seek_to(
            &fake.playback,
            || Ok(fake.library.clone()),
            Deck::A,
            PositionMs::new(1_230),
            Placement::Snap,
            None,
        )
        .unwrap();
        assert_eq!(
            fake.sent(),
            vec![
                recorded(&Command::GetTransport { deck: Deck::A }),
                seek_command(1_230)
            ]
        );
    }

    #[test]
    fn beats_without_a_grid_are_refused_and_nothing_is_sent_but_the_question() {
        let fake = FakeServices::start(Setup {
            tracks: &[A],
            transport: Some(playing(1_230)),
            ..Setup::default()
        });
        let err = run(
            &fake.playback,
            || Ok(fake.library.clone()),
            SeekTarget::Beats(8),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "the track has no beat grid");
        assert_eq!(
            fake.sent(),
            vec![recorded(&Command::GetTransport { deck: Deck::A })]
        );
    }
}
