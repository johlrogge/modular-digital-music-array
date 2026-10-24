//! Pure bookkeeping for "which track is audible, and where is it".
//!
//! Positions are derived from the output's `consumed_total` (samples played
//! or flushed), not from decoder progress, so they never run ahead by the
//! buffered amount. A segment of the output stream belongs to one track:
//!
//! * manual load: starts at the mixer's `produced_total` when it first took
//!   from the track,
//! * gapless promotion: starts at the promotion boundary.
//!
//! The audible track switches once `consumed_total >= boundary`.

/// One track's slice of the output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Segment {
    pub id: u64,
    /// Output-stream sample index at which this track's audio starts.
    pub start_sample: u64,
    /// Track position (ms) at `start_sample` (non-zero after a seek).
    pub base_ms: u64,
    /// Channel count of the *output* stream. `consumed` counts samples the
    /// output consumed, i.e. interleaved at the output's channel count (not
    /// the source's: a mono source is still played through a stereo stream).
    pub output_channels: u32,
    /// Output sample rate (frames per second).
    pub rate: u32,
}

impl Segment {
    pub fn position_ms(&self, consumed: u64) -> u64 {
        let frames =
            consumed.saturating_sub(self.start_sample) / u64::from(self.output_channels.max(1));
        self.base_ms + frames * 1000 / u64::from(self.rate.max(1))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Playhead {
    audible: Option<Segment>,
    /// Promoted by the mixer but not yet heard (boundary not consumed).
    /// A second promotion before the first is heard (a track shorter than the
    /// output latency) force-resolves the first.
    pending: Option<Segment>,
    last_promotion_id: u64,
}

impl Playhead {
    /// Manual load / reuse: the track is audible immediately.
    ///
    /// `mixer_promotion_id` is the id of the mixer's latest gapless promotion
    /// at this moment (0 if none). It is recorded as already seen so that an
    /// older promotion still published by the mixer is never replayed on top
    /// of this segment.
    pub fn start(&mut self, segment: Segment, mixer_promotion_id: u64) {
        self.audible = Some(segment);
        self.pending = None;
        self.last_promotion_id = mixer_promotion_id;
    }

    pub fn clear(&mut self) {
        self.audible = None;
        self.pending = None;
    }

    /// Record a gapless promotion; `segment.start_sample` is the boundary.
    pub fn schedule(&mut self, segment: Segment) {
        if let Some(prev) = self.pending.take() {
            self.audible = Some(prev);
        }
        self.pending = Some(segment);
    }

    pub fn last_promotion_id(&self) -> u64 {
        self.last_promotion_id
    }

    pub fn note_promotion_seen(&mut self, id: u64) {
        self.last_promotion_id = id;
    }

    /// Switch to the pending track if its boundary has been consumed.
    pub fn resolve(&mut self, consumed: u64) {
        if self.pending.is_some_and(|p| consumed >= p.start_sample) {
            self.audible = self.pending.take();
        }
    }

    pub fn audible(&self) -> Option<&Segment> {
        self.audible.as_ref()
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Id of the promoted-but-not-yet-audible track, if any.
    pub fn pending_id(&self) -> Option<u64> {
        self.pending.map(|s| s.id)
    }

    pub fn references(&self, id: u64) -> bool {
        self.audible.is_some_and(|s| s.id == id) || self.pending.is_some_and(|s| s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: u64, start: u64) -> Segment {
        Segment {
            id,
            start_sample: start,
            base_ms: 0,
            output_channels: 2,
            rate: 1000,
        }
    }

    #[test]
    fn start_records_the_mixers_current_promotion_as_seen() {
        let mut p = Playhead::default();
        p.start(seg(5, 0), 4);
        assert_eq!(p.last_promotion_id(), 4);
    }

    #[test]
    fn manual_load_position_counts_from_start_sample() {
        let mut p = Playhead::default();
        p.start(seg(1, 400), 0);
        let s = p.audible().unwrap();
        assert_eq!(s.position_ms(400), 0);
        // 2000 samples = 1000 frames = 1000 ms at 1 kHz.
        assert_eq!(s.position_ms(2400), 1000);
    }

    #[test]
    fn position_saturates_before_start_is_consumed() {
        let s = seg(1, 400);
        assert_eq!(s.position_ms(100), 0);
    }

    #[test]
    fn gapless_switch_happens_exactly_at_boundary() {
        let mut p = Playhead::default();
        p.start(seg(1, 0), 0);
        p.schedule(seg(2, 10_000));

        p.resolve(9_999);
        assert_eq!(p.audible().unwrap().id, 1);
        assert!(p.has_pending());
        // Old track keeps counting until the switch.
        assert_eq!(p.audible().unwrap().position_ms(9_999), 4_999);

        p.resolve(10_000);
        assert_eq!(p.audible().unwrap().id, 2);
        assert!(!p.has_pending());
        assert_eq!(p.audible().unwrap().position_ms(10_000), 0);
        assert_eq!(p.audible().unwrap().position_ms(10_200), 100);
    }

    #[test]
    fn position_freezes_when_nothing_is_consumed() {
        let mut p = Playhead::default();
        p.start(seg(1, 0), 0);
        let s = *p.audible().unwrap();
        assert_eq!(s.position_ms(2_000), s.position_ms(2_000));
        p.resolve(2_000);
        assert_eq!(p.audible().unwrap().position_ms(2_000), 1_000);
    }

    #[test]
    fn seek_restarts_accounting_with_base_offset() {
        let mut p = Playhead::default();
        p.start(seg(1, 0), 0);
        // Seek to 30 s: the new segment starts where the mixer is now.
        p.start(
            Segment {
                base_ms: 30_000,
                ..seg(2, 8_000)
            },
            0,
        );
        let s = p.audible().unwrap();
        assert_eq!(s.position_ms(8_000), 30_000);
        assert_eq!(s.position_ms(8_000 + 2_000), 31_000);
    }

    #[test]
    fn flushed_samples_count_as_consumed() {
        // A flush advances consumed_total to produced_total; a track that
        // starts at produced_total therefore starts at position 0.
        let produced = 50_000;
        let consumed_after_flush = produced;
        let mut p = Playhead::default();
        p.start(seg(7, produced), 0);
        assert_eq!(
            p.audible().unwrap().position_ms(consumed_after_flush),
            0,
            "flushed audio must not count towards the new track"
        );
    }

    #[test]
    fn second_promotion_force_resolves_first() {
        let mut p = Playhead::default();
        p.start(seg(1, 0), 0);
        p.schedule(seg(2, 100));
        p.schedule(seg(3, 200));
        assert_eq!(p.audible().unwrap().id, 2);
        p.resolve(200);
        assert_eq!(p.audible().unwrap().id, 3);
    }

    #[test]
    fn manual_start_discards_pending() {
        let mut p = Playhead::default();
        p.start(seg(1, 0), 0);
        p.schedule(seg(2, 100));
        assert_eq!(p.pending_id(), Some(2));
        p.start(seg(3, 500), 0);
        assert_eq!(p.pending_id(), None);
        assert!(!p.has_pending());
        assert!(p.references(3));
        assert!(!p.references(1) && !p.references(2));
    }

    #[test]
    fn zero_channels_and_rate_do_not_divide_by_zero() {
        let s = Segment {
            output_channels: 0,
            rate: 0,
            ..seg(1, 0)
        };
        assert_eq!(s.position_ms(10), 10_000);
    }
}
