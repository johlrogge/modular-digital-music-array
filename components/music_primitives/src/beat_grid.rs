use std::num::NonZeroU8;

use crate::Bpm;

/// Hundredths of a BPM per millisecond-beat: one beat lasts
/// `BEAT_NUMERATOR / bpm_hundredths` milliseconds (60 000 ms per minute, times
/// 100 because [`Bpm`] counts hundredths).
const BEAT_NUMERATOR: i128 = 6_000_000;

/// A constant-tempo beat grid: one anchor (`first_beat_ms`, a downbeat) and one
/// tempo for the whole track.
///
/// This is deliberately all there is today: a track's grid is one anchor and
/// one BPM, so a track whose tempo drifts, or whose grid is anchored away from
/// its start, is only as accurate as that single anchor. Variable-tempo grids
/// would be a different type, not a flag on this one.
///
/// All positions are milliseconds from the start of the track. Beat numbers are
/// signed: beat 0 is the anchor, negative beats lie before it.
///
/// The snap rule lives here, so every client (CLI, TUI, console, a hardware
/// remote) snaps the same way and only passes "snap or not".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeatGrid {
    first_beat_ms: u32,
    bpm: Bpm,
    beats_per_bar: NonZeroU8,
}

impl BeatGrid {
    /// A grid anchored at `first_beat_ms`. A bar of zero beats has no lines,
    /// so it cannot be built.
    pub fn new(first_beat_ms: u32, bpm: Bpm, beats_per_bar: NonZeroU8) -> Self {
        Self {
            first_beat_ms,
            bpm,
            beats_per_bar,
        }
    }

    pub fn first_beat_ms(&self) -> u32 {
        self.first_beat_ms
    }

    pub fn bpm(&self) -> Bpm {
        self.bpm
    }

    pub fn beats_per_bar(&self) -> NonZeroU8 {
        self.beats_per_bar
    }

    fn hundredths(&self) -> i128 {
        i128::from(self.bpm.as_hundredths())
    }

    /// Position of beat `n`, rounded to the nearest millisecond. Negative for
    /// beats that fall before the start of the track. Saturates for beat
    /// numbers too far out to be a position at all.
    pub fn beat_ms(&self, n: i64) -> i64 {
        let h = self.hundredths();
        let offset = (2 * i128::from(n) * BEAT_NUMERATOR + h).div_euclid(2 * h);
        let offset = i64::try_from(offset).unwrap_or(if offset < 0 { i64::MIN } else { i64::MAX });
        i64::from(self.first_beat_ms).saturating_add(offset)
    }

    /// The number of the beat nearest to `ms` (halfway rounds up).
    pub fn nearest_beat_number(&self, ms: u64) -> i64 {
        let offset = i128::from(ms) - i128::from(self.first_beat_ms);
        (offset * self.hundredths() + BEAT_NUMERATOR / 2).div_euclid(BEAT_NUMERATOR) as i64
    }

    /// `ms` snapped to the nearest beat. A beat before the start of the track
    /// is not a position, so it snaps to 0.
    pub fn nearest_beat(&self, ms: u64) -> u64 {
        self.beat_ms(self.nearest_beat_number(ms)).max(0) as u64
    }

    /// Snap `from_ms` to the nearest beat, move `beats` beats (negative goes
    /// back), and clamp to `0..=duration_ms`.
    pub fn step(&self, from_ms: u64, beats: i32, duration_ms: u64) -> u64 {
        let target = self
            .nearest_beat_number(from_ms)
            .saturating_add(i64::from(beats));
        (self.beat_ms(target).max(0) as u64).min(duration_ms)
    }

    /// The bar lines within `0..=duration_ms`, in order. Beat 0 is a downbeat;
    /// bars repeat every `beats_per_bar` beats before and after it.
    pub fn bars(&self, duration_ms: u64) -> Vec<u64> {
        let per_bar = i64::from(self.beats_per_bar.get());
        // Start one bar early so rounding at the very start cannot lose a line.
        let mut bar = self.nearest_beat_number(0).div_euclid(per_bar) - 1;
        let end = i64::try_from(duration_ms).unwrap_or(i64::MAX);
        let mut lines = Vec::new();
        loop {
            let at = self.beat_ms(bar * per_bar);
            if at > end {
                return lines;
            }
            if at >= 0 {
                lines.push(at as u64);
            }
            bar += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    /// A grid of whole `bpm`, in bars of `per_bar` beats (not zero).
    fn grid(first_beat_ms: u32, bpm: u32, per_bar: u8) -> BeatGrid {
        BeatGrid::new(
            first_beat_ms,
            Bpm::from_u32(bpm).unwrap(),
            NonZeroU8::new(per_bar).unwrap(),
        )
    }

    /// 120 BPM: one beat every 500 ms, easy to reason about.
    fn grid_120() -> BeatGrid {
        grid(100, 120, 4)
    }

    #[test]
    fn beat_ms_saturates_for_beats_out_of_range() {
        let g = grid_120();
        assert_eq!(g.beat_ms(i64::MAX), i64::MAX);
        assert_eq!(g.beat_ms(i64::MIN), i64::MIN + 100);
    }

    #[test]
    fn beat_ms_walks_in_whole_beats_from_the_anchor() {
        let g = grid_120();
        assert_eq!(g.beat_ms(0), 100);
        assert_eq!(g.beat_ms(1), 600);
        assert_eq!(g.beat_ms(8), 4_100);
        assert_eq!(g.beat_ms(-1), -400);
    }

    #[test]
    fn beat_ms_rounds_a_fractional_period_to_the_nearest_ms() {
        // 128 BPM: 468.75 ms per beat.
        let g = grid(0, 128, 4);
        assert_eq!(g.beat_ms(1), 469);
        assert_eq!(g.beat_ms(2), 938);
        assert_eq!(g.beat_ms(4), 1_875);
    }

    #[test]
    fn nearest_beat_snaps_to_the_closer_beat_and_halves_round_up() {
        let g = grid_120();
        assert_eq!(g.nearest_beat(100), 100);
        assert_eq!(g.nearest_beat(340), 100);
        assert_eq!(g.nearest_beat(360), 600);
        assert_eq!(g.nearest_beat(350), 600, "halfway rounds up");
        assert_eq!(g.nearest_beat(90_000), 90_100);
    }

    #[test]
    fn nearest_beat_before_the_anchor_never_goes_negative() {
        let g = grid(1_000, 120, 4);
        assert_eq!(g.nearest_beat(0), 0, "beat -2 is at 0 ms");
        let late = grid(300, 120, 4);
        assert_eq!(late.nearest_beat(0), 0, "beat -1 is at -200 ms: clamped");
        assert_eq!(late.nearest_beat(100), 300);
    }

    #[test]
    fn step_snaps_first_then_steps() {
        let g = grid_120();
        // 1_230 snaps to beat 2 (1_100); +4 beats is beat 6 (3_100).
        assert_eq!(g.step(1_230, 4, 200_000), 3_100);
        assert_eq!(g.step(1_230, -2, 200_000), 100);
        assert_eq!(g.step(1_230, 0, 200_000), 1_100);
    }

    #[test]
    fn step_clamps_to_the_track() {
        let g = grid_120();
        assert_eq!(g.step(1_100, -32, 200_000), 0);
        assert_eq!(g.step(1_100, 32, 5_000), 5_000);
        assert_eq!(g.step(1_100, i32::MAX, 5_000), 5_000);
        assert_eq!(g.step(1_100, i32::MIN, 5_000), 0);
        assert_eq!(g.step(9_000, 0, 5_000), 5_000, "past the end clamps too");
    }

    #[test]
    fn bars_are_every_beats_per_bar_beats_within_the_track() {
        let g = grid_120();
        // 2 000 ms per bar from the anchor at 100.
        assert_eq!(g.bars(7_000), vec![100, 2_100, 4_100, 6_100]);
        assert_eq!(g.bars(6_100), vec![100, 2_100, 4_100, 6_100], "inclusive");
        assert_eq!(g.bars(50), Vec::<u64>::new());
    }

    #[test]
    fn bars_include_the_ones_before_a_late_anchor() {
        let g = grid(2_500, 120, 4);
        assert_eq!(g.bars(5_000), vec![500, 2_500, 4_500]);
    }

    #[test]
    fn a_one_beat_bar_has_a_line_on_every_beat() {
        assert_eq!(grid(0, 120, 1).bars(1_000), vec![0, 500, 1_000]);
    }

    prop_compose! {
        fn any_grid()(
            first in 0u32..120_000,
            hundredths in 2_000u32..=99_999,
            per_bar in 1u8..=12,
        ) -> BeatGrid {
            BeatGrid::new(
                first,
                Bpm::try_from(hundredths).unwrap(),
                NonZeroU8::new(per_bar).unwrap(),
            )
        }
    }

    proptest! {
        #[test]
        fn nearest_beat_is_monotonic(g in any_grid(), a in 0u64..1_000_000, b in 0u64..1_000_000) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(g.nearest_beat(lo) <= g.nearest_beat(hi));
        }

        #[test]
        fn beat_numbers_round_trip_through_beat_ms(g in any_grid(), n in 0i64..5_000) {
            let at = g.beat_ms(n);
            prop_assume!(at >= 0);
            prop_assert_eq!(g.nearest_beat_number(at as u64), n);
        }

        #[test]
        fn snapping_is_idempotent(g in any_grid(), ms in 0u64..1_000_000) {
            let once = g.nearest_beat(ms);
            prop_assert_eq!(g.nearest_beat(once), once);
        }

        #[test]
        fn step_never_leaves_the_track(
            g in any_grid(),
            from in 0u64..1_000_000,
            beats in any::<i32>(),
            duration in 0u64..1_000_000,
        ) {
            prop_assert!(g.step(from, beats, duration) <= duration);
        }

        #[test]
        fn step_forward_is_monotonic_in_beats(
            g in any_grid(),
            from in 0u64..1_000_000,
            a in -64i32..64,
            b in -64i32..64,
            duration in 0u64..2_000_000,
        ) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(g.step(from, lo, duration) <= g.step(from, hi, duration));
        }

        #[test]
        fn step_is_monotonic_in_the_start(
            g in any_grid(),
            a in 0u64..1_000_000,
            b in 0u64..1_000_000,
            beats in -64i32..64,
            duration in 0u64..2_000_000,
        ) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(g.step(lo, beats, duration) <= g.step(hi, beats, duration));
        }

        #[test]
        fn bars_are_sorted_and_inside_the_track(g in any_grid(), duration in 0u64..600_000) {
            let bars = g.bars(duration);
            prop_assert!(bars.windows(2).all(|w| w[0] < w[1]));
            prop_assert!(bars.iter().all(|&b| b <= duration));
        }
    }
}
