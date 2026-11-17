//! Seeking by beats and snapping, in one place (ADR-031).
//!
//! The rules are here, not in each client: the CLI, the TUI, the console and a
//! hardware remote all call these. Playback itself only seeks to a time and
//! never learns about beat grids; the grid comes from the library.
//!
//! Without a grid a beat step is refused ([`SeekError::NoBeatGrid`]); there is
//! no fallback to a guessed tempo. Keeping a position within the track is the
//! engine's rule, not repeated here; the answer says where the track went.

use library_ipc_client::{BeatGrid, ClientError as LibraryClientError, TrackInfo};
use media_client::{
    ClientError as PlaybackClientError, ContentHash, Deck, PositionMs, TransportStatus,
};

use crate::{LibraryBackend, NodeError, PlaybackBackend};

/// Why a seek by beats or a snapped seek could not be made.
#[derive(Debug, thiserror::Error)]
pub enum SeekError {
    #[error("nothing is loaded")]
    NothingLoaded,
    #[error("the node did not say where the track is")]
    NoPosition,
    #[error("the track has no beat grid")]
    NoBeatGrid,
    #[error(transparent)]
    Playback(#[from] PlaybackClientError),
    #[error(transparent)]
    Library(#[from] LibraryClientError),
    #[error(transparent)]
    Node(#[from] NodeError),
}

/// How a requested time is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// To the nearest beat, when the track has a beat grid.
    Snap,
    /// Exactly where asked (what holding Alt does on the timeline, and what a
    /// typed time does).
    Free,
}

/// The audible track of a deck with its beat grid.
#[derive(Debug, Clone, PartialEq)]
struct Audible {
    hash: ContentHash,
    position: Option<PositionMs>,
    duration: Option<PositionMs>,
    grid: Option<BeatGrid>,
}

impl Audible {
    fn of(
        transport: &TransportStatus,
        track: impl FnOnce(&ContentHash) -> Result<TrackInfo, SeekError>,
    ) -> Result<Self, SeekError> {
        match transport {
            TransportStatus::Idle => Err(SeekError::NothingLoaded),
            TransportStatus::Loaded {
                now,
                position_ms,
                duration_ms,
                ..
            } => Ok(Self {
                hash: now.clone(),
                position: *position_ms,
                duration: *duration_ms,
                grid: grid_of(&track(now)?),
            }),
        }
    }
}

/// The track's grid, if it has a usable one.
fn grid_of(track: &TrackInfo) -> Option<BeatGrid> {
    track.beat_grid.as_ref().and_then(|info| info.grid())
}

/// What is playing on `deck`, with its grid: the opening of every operation
/// that needs the grid. The library is connected to only here, so an
/// operation that does not need the grid never needs the library.
fn audible_with_grid(
    playback: &PlaybackBackend,
    library: impl FnOnce() -> Result<LibraryBackend, NodeError>,
    deck: Deck,
) -> Result<Audible, SeekError> {
    Audible::of(&playback.get_transport(deck)?, |hash| {
        Ok(library()?.get_track(hash)?)
    })
}

/// Where `beats` beats from `audible` is: snap to the nearest beat, step, clamp.
fn skip_target(audible: &Audible, beats: i32) -> Result<PositionMs, SeekError> {
    let grid = audible.grid.ok_or(SeekError::NoBeatGrid)?;
    let (Some(position), Some(duration)) = (audible.position, audible.duration) else {
        return Err(SeekError::NoPosition);
    };
    Ok(PositionMs::new(grid.step(
        position.as_millis(),
        beats,
        duration.as_millis(),
    )))
}

/// `ms`, snapped to the nearest beat when `placement` is `Snap` and there is a
/// grid.
fn placed(ms: PositionMs, placement: Placement, grid: Option<BeatGrid>) -> PositionMs {
    match (placement, grid) {
        (Placement::Snap, Some(grid)) => PositionMs::new(grid.nearest_beat(ms.as_millis())),
        _ => ms,
    }
}

/// Move the audible track of `deck` by `beats` beats (negative goes back):
/// read where it is, read its grid, snap, step, clamp, seek. Answers where it
/// went. Playing stays playing and paused stays paused.
pub fn skip_beats(
    playback: &PlaybackBackend,
    library: impl FnOnce() -> Result<LibraryBackend, NodeError>,
    deck: Deck,
    beats: i32,
) -> Result<TransportStatus, SeekError> {
    let target = skip_target(&audible_with_grid(playback, library, deck)?, beats)?;
    Ok(playback.seek(deck, target)?)
}

/// Seek the audible track of `deck` to `ms`. With `Placement::Snap` the time
/// goes to the nearest beat when the track has a grid (without one, where
/// asked); with `Placement::Free` it goes where asked and the library is never
/// connected to. Answers where the track went.
pub fn seek_to(
    playback: &PlaybackBackend,
    library: impl FnOnce() -> Result<LibraryBackend, NodeError>,
    deck: Deck,
    ms: PositionMs,
    placement: Placement,
) -> Result<TransportStatus, SeekError> {
    let target = match placement {
        Placement::Free => ms,
        Placement::Snap => placed(
            ms,
            placement,
            audible_with_grid(playback, library, deck)?.grid,
        ),
    };
    Ok(playback.seek(deck, target)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::BeatGridInfo;
    use media_client::Transport;
    use pretty_assertions::assert_eq;

    /// 120 BPM, a beat every 500 ms from 100 ms.
    fn grid() -> BeatGrid {
        BeatGridInfo {
            first_beat_ms: 100,
            bpm: library_ipc_client::Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        }
        .grid()
        .unwrap()
    }

    fn loaded(position_ms: Option<u64>, duration_ms: Option<u64>) -> TransportStatus {
        TransportStatus::Loaded {
            transport: Transport::Playing,
            now: ContentHash::new("a"),
            position_ms: position_ms.map(PositionMs::new),
            duration_ms: duration_ms.map(PositionMs::new),
            preview: None,
        }
    }

    fn audible(position_ms: u64, duration_ms: u64, grid: Option<BeatGrid>) -> Audible {
        Audible {
            hash: ContentHash::new("a"),
            position: Some(PositionMs::new(position_ms)),
            duration: Some(PositionMs::new(duration_ms)),
            grid,
        }
    }

    fn track_with(grid: Option<BeatGridInfo>) -> TrackInfo {
        let mut track: TrackInfo =
            serde_json::from_str(r#"{"content_hash":"a"}"#).expect("a minimal track");
        track.beat_grid = grid;
        track
    }

    #[test]
    fn nothing_loaded_when_the_deck_is_idle_and_the_library_is_not_asked() {
        let err =
            Audible::of(&TransportStatus::Idle, |_| panic!("no track to ask for")).unwrap_err();
        assert!(matches!(err, SeekError::NothingLoaded), "{err}");
    }

    #[test]
    fn audible_carries_hash_position_duration_and_the_tracks_grid() {
        let info = BeatGridInfo {
            first_beat_ms: 100,
            bpm: library_ipc_client::Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        };
        let a = Audible::of(&loaded(Some(90_000), Some(240_000)), |hash| {
            assert_eq!(hash, &ContentHash::new("a"));
            Ok(track_with(Some(info)))
        })
        .unwrap();
        assert_eq!(a, audible(90_000, 240_000, Some(grid())));
    }

    #[test]
    fn a_track_without_a_grid_or_with_a_zero_bar_has_no_grid() {
        assert_eq!(grid_of(&track_with(None)), None);
        let zero = BeatGridInfo {
            first_beat_ms: 0,
            bpm: library_ipc_client::Bpm::from_u32(120).unwrap(),
            beats_per_bar: 0,
        };
        assert_eq!(grid_of(&track_with(Some(zero))), None);
    }

    #[test]
    fn a_beat_step_snaps_the_position_first() {
        // 1_230 ms snaps to beat 2 (1_100); +8 beats is beat 10 (5_100).
        assert_eq!(
            skip_target(&audible(1_230, 240_000, Some(grid())), 8).unwrap(),
            PositionMs::new(5_100)
        );
        assert_eq!(
            skip_target(&audible(1_230, 240_000, Some(grid())), -8).unwrap(),
            PositionMs::START,
            "beat -6 is before the start of the track"
        );
    }

    #[test]
    fn a_beat_step_is_clamped_to_the_track() {
        assert_eq!(
            skip_target(&audible(1_100, 3_000, Some(grid())), 32).unwrap(),
            PositionMs::new(3_000)
        );
    }

    #[test]
    fn a_beat_step_without_a_grid_is_refused_not_guessed() {
        let err = skip_target(&audible(1_000, 240_000, None), 8).unwrap_err();
        assert!(matches!(err, SeekError::NoBeatGrid), "{err}");
        assert_eq!(err.to_string(), "the track has no beat grid");
    }

    #[test]
    fn a_beat_step_needs_to_know_where_the_track_is() {
        let mut a = audible(0, 1, Some(grid()));
        a.position = None;
        let err = skip_target(&a, 8).unwrap_err();
        assert!(matches!(err, SeekError::NoPosition), "{err}");
    }

    #[test]
    fn snap_goes_to_the_nearest_beat_and_free_goes_where_asked() {
        let ms = PositionMs::new(1_230);
        assert_eq!(
            placed(ms, Placement::Snap, Some(grid())),
            PositionMs::new(1_100)
        );
        assert_eq!(placed(ms, Placement::Free, Some(grid())), ms);
    }

    #[test]
    fn snap_without_a_grid_goes_where_asked() {
        let ms = PositionMs::new(1_230);
        assert_eq!(placed(ms, Placement::Snap, None), ms);
    }
}
