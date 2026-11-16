//! Pure liveness judgement for the audio output.
//!
//! The output's callback heartbeat is the single source of truth (ADR-028):
//! a count that advances means PipeWire is pulling samples; a count that stays
//! put for [`STALL_AFTER`] means it is not, whatever the stream state claims.
//! Time is passed in, so tests use synthetic `Instant`s and never sleep.

use std::time::{Duration, Instant};

use playback_primitives::OutputHealth;

/// How long the heartbeat may stand still before the output is `Stalled`.
pub const STALL_AFTER: Duration = Duration::from_millis(1000);

#[derive(Debug)]
pub(crate) struct OutputWatch {
    last_count: u64,
    last_change: Instant,
    verdict: OutputHealth,
}

impl OutputWatch {
    /// A watch for a freshly created output: `Starting` until judged.
    ///
    /// `count` is the heartbeat as it stands now. The counter survives output
    /// re-creation, so seeding from it makes only a callback that runs after
    /// this point count as proof of life.
    pub fn new(count: u64, now: Instant) -> Self {
        Self {
            last_count: count,
            last_change: now,
            verdict: OutputHealth::Starting,
        }
    }

    /// Fold in the heartbeat `count` seen at `now` and return the verdict.
    pub fn observe(&mut self, count: u64, now: Instant) -> OutputHealth {
        if count != self.last_count {
            self.last_count = count;
            self.last_change = now;
            self.verdict = OutputHealth::Running;
        } else if now.saturating_duration_since(self.last_change) >= STALL_AFTER {
            self.verdict = OutputHealth::Stalled;
        }
        self.verdict
    }

    pub fn verdict(&self) -> OutputHealth {
        self.verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn fresh_watch_is_starting_until_threshold() {
        let t0 = Instant::now();
        let mut w = OutputWatch::new(0, t0);
        assert_eq!(w.observe(0, t0), OutputHealth::Starting);
        assert_eq!(w.observe(0, t0 + ms(999)), OutputHealth::Starting);
        assert_eq!(w.observe(0, t0 + ms(1000)), OutputHealth::Stalled);
    }

    #[test]
    fn advancing_count_is_running() {
        let t0 = Instant::now();
        let mut w = OutputWatch::new(0, t0);
        assert_eq!(w.observe(5, t0 + ms(10)), OutputHealth::Running);
        assert_eq!(w.observe(9, t0 + ms(5000)), OutputHealth::Running);
    }

    #[test]
    fn running_then_frozen_for_threshold_is_stalled() {
        let t0 = Instant::now();
        let mut w = OutputWatch::new(0, t0);
        w.observe(5, t0 + ms(10));
        assert_eq!(w.observe(5, t0 + ms(1009)), OutputHealth::Running);
        assert_eq!(w.observe(5, t0 + ms(1010)), OutputHealth::Stalled);
    }

    #[test]
    fn stalled_then_advancing_recovers_to_running() {
        let t0 = Instant::now();
        let mut w = OutputWatch::new(0, t0);
        assert_eq!(w.observe(0, t0 + ms(2000)), OutputHealth::Stalled);
        assert_eq!(w.observe(1, t0 + ms(2100)), OutputHealth::Running);
    }

    #[test]
    fn watch_seeded_with_current_count_is_starting_not_running() {
        let t0 = Instant::now();
        let mut w = OutputWatch::new(500, t0);
        assert_eq!(w.observe(500, t0 + ms(100)), OutputHealth::Starting);
        assert_eq!(w.observe(500, t0 + ms(1000)), OutputHealth::Stalled);
        assert_eq!(w.observe(501, t0 + ms(1100)), OutputHealth::Running);
    }
}
