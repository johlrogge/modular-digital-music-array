use crate::error::PlaybackError;
use ringbuf::{HeapConsumer, HeapProducer};
use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// A track registered with the mixer.
///
/// `id` must be non-zero (0 means "no track" in [`MixerStatus`]).
/// `input_complete` is set (Release) by the decoder once EOF was reached and
/// every sample has been pushed into `consumer`'s ring.
pub struct Slot {
    pub id: u64,
    pub consumer: HeapConsumer<f32>,
    pub input_complete: Arc<AtomicBool>,
}

/// The most recent gapless promotion (next became current).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Promotion {
    /// Value of `produced_total` at the instant of the switch: samples with
    /// index `>= boundary_sample` (in the mixer output stream) belong to
    /// `promoted_id`.
    pub boundary_sample: u64,
    pub promoted_id: u64,
}

/// One coherent-enough read of the mixer's published state; see
/// [`MixerStatus::snapshot`] for exactly what that guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixerSnapshot {
    /// 0 when the mixer has no current track.
    pub current_id: u64,
    /// 0 when no next track is registered.
    pub next_id: u64,
    pub last_promotion: Option<Promotion>,
}

impl MixerSnapshot {
    /// Whether the mixer (current or next) still references `id`.
    pub fn references(&self, id: u64) -> bool {
        id != 0 && (self.current_id == id || self.next_id == id)
    }
}

/// Lock-free state published by the mix thread for other threads to read.
///
/// All values are written by the single mix thread only.
///
/// * `produced_total`: every sample pushed to the output ring since the
///   mixer was created. Stored (Release) at the end of a mix block, *after*
///   the promotion record of that block, so a reader that sees
///   `produced_total >= boundary` also sees the promotion.
/// * `current_id` / `next_id`: 0 when empty. Written current first, then next.
///   Read them only through [`MixerStatus::snapshot`] when both matter.
/// * The promotion record is a (boundary, id) pair protected by a seqlock
///   (`promo_seq`: odd while being written), so [`MixerStatus::last_promotion`]
///   can never return a torn pair.
#[derive(Debug, Default)]
pub struct MixerStatus {
    produced_total: AtomicU64,
    current_id: AtomicU64,
    next_id: AtomicU64,
    promo_seq: AtomicU64,
    promo_boundary: AtomicU64,
    promo_id: AtomicU64,
}

impl MixerStatus {
    pub fn produced_total(&self) -> u64 {
        self.produced_total.load(Ordering::Acquire)
    }

    /// Single-field read, for tests. Production readers use
    /// [`MixerStatus::snapshot`], which reads both ids in the right order.
    #[cfg(test)]
    pub fn current_id(&self) -> u64 {
        self.current_id.load(Ordering::Acquire)
    }

    /// See [`MixerStatus::current_id`].
    #[cfg(test)]
    pub fn next_id(&self) -> u64 {
        self.next_id.load(Ordering::Acquire)
    }

    /// Read `next_id`, then `current_id`, then the last promotion (all
    /// Acquire), in that order.
    ///
    /// The mix thread publishes in the opposite order (promotion record,
    /// then `current_id`, then `next_id`; each store is Release). So a reader
    /// that observes a new `next_id` is guaranteed to observe the matching new
    /// `current_id` and promotion, and can never see the torn state
    /// "current = old track, next = 0" in the middle of a promotion (which
    /// would make the promoted track look unreferenced). The stale
    /// direction is harmless: at worst a just-promoted id shows up as both
    /// `next_id` and `current_id`, or an old id lingers for one more read,
    /// which only delays dropping a track.
    pub fn snapshot(&self) -> MixerSnapshot {
        let next_id = self.next_id.load(Ordering::Acquire);
        let current_id = self.current_id.load(Ordering::Acquire);
        let last_promotion = self.last_promotion();
        MixerSnapshot {
            current_id,
            next_id,
            last_promotion,
        }
    }

    /// Latest promotion, or `None` if none happened yet. Consistent pair.
    ///
    /// Seqlock reader: spins while the single writer is mid-update (the
    /// window is a few instructions in the mix thread, which never blocks
    /// inside it). Only for non-real-time readers (the engine / server
    /// thread): never call this from the output callback.
    pub fn last_promotion(&self) -> Option<Promotion> {
        loop {
            let s1 = self.promo_seq.load(Ordering::Acquire);
            if s1 == 0 {
                return None;
            }
            if s1 % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let boundary_sample = self.promo_boundary.load(Ordering::Relaxed);
            let promoted_id = self.promo_id.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.promo_seq.load(Ordering::Relaxed) == s1 {
                return Some(Promotion {
                    boundary_sample,
                    promoted_id,
                });
            }
        }
    }

    /// Mix thread only (single writer).
    fn publish_promotion(&self, boundary: u64, id: u64) {
        let s = self.promo_seq.load(Ordering::Relaxed);
        self.promo_seq.store(s + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.promo_boundary.store(boundary, Ordering::Relaxed);
        self.promo_id.store(id, Ordering::Relaxed);
        self.promo_seq.store(s + 2, Ordering::Release);
    }
}

pub struct Mixer {
    output_producer: HeapProducer<f32>,
    current: Option<Slot>,
    next: Option<Slot>,
    produced_total: u64,
    status: Arc<MixerStatus>,
}

impl Mixer {
    pub fn new(output_producer: HeapProducer<f32>) -> Self {
        Self {
            output_producer,
            current: None,
            next: None,
            produced_total: 0,
            status: Arc::new(MixerStatus::default()),
        }
    }

    /// Shared handle to the atomics this mixer publishes.
    pub fn status(&self) -> Arc<MixerStatus> {
        Arc::clone(&self.status)
    }

    /// Replace the current track (and drop any pending next).
    pub fn register_current(&mut self, slot: Slot) {
        self.next = None;
        self.current = Some(slot);
        self.publish_ids();
    }

    /// Set the preloaded next track.
    ///
    /// The engine never registers a next without a current track (preloading
    /// requires an audible track and every cut keeps the mixer's current
    /// until it is replaced). If it happens anyway the slot becomes current,
    /// without a playhead segment, so it is logged as a bug.
    pub fn register_next(&mut self, slot: Slot) {
        if self.current.is_none() {
            tracing::warn!(
                "Mixer: register_next without a current track; track {} becomes current",
                slot.id
            );
            self.current = Some(slot);
            self.next = None;
        } else {
            self.next = Some(slot);
        }
        self.publish_ids();
    }

    /// Drop the pending next if it has this id. A no-op if that id was
    /// already promoted (or never registered).
    pub fn clear_next(&mut self, id: u64) {
        if self.next.as_ref().is_some_and(|s| s.id == id) {
            self.next = None;
            self.publish_ids();
        }
    }

    /// Drop both slots.
    pub fn clear_all(&mut self) {
        self.current = None;
        self.next = None;
        self.publish_ids();
    }

    /// Drop the current track but keep the pending next (it is not promoted
    /// while there is no current). Used to cut to a preloaded track: after
    /// this the mixer produces nothing until [`Mixer::promote_next`].
    pub fn clear_current(&mut self) {
        self.current = None;
        self.publish_ids();
    }

    /// Make the pending next (if it has this id) the current track, without
    /// recording a gapless promotion. Returns the number of samples produced
    /// so far, i.e. the stream position where the track starts.
    pub fn promote_next(&mut self, id: u64) -> Option<u64> {
        if self.next.as_ref().is_some_and(|s| s.id == id) {
            self.current = self.next.take();
            self.publish_ids();
            Some(self.produced_total)
        } else {
            None
        }
    }

    fn publish_ids(&self) {
        self.status
            .current_id
            .store(self.current.as_ref().map_or(0, |s| s.id), Ordering::Release);
        self.status
            .next_id
            .store(self.next.as_ref().map_or(0, |s| s.id), Ordering::Release);
    }

    /// Move up to `min(samples_per_callback, output.len(), output free space)`
    /// samples from the current track (switching to `next` mid-block when the
    /// current one is fully consumed) into the output ring. Never pads with
    /// zeros and never blocks: the output ring is a gap-free stream.
    pub fn mix(
        &mut self,
        output: &mut [f32],
        samples_per_callback: usize,
    ) -> Result<(), PlaybackError> {
        let to_mix = self
            .output_producer
            .free_len()
            .min(samples_per_callback)
            .min(output.len());
        if to_mix == 0 {
            return Ok(());
        }

        let mut filled = 0;
        let mut promoted = false;
        while filled < to_mix {
            let Some(cur) = self.current.as_mut() else {
                break;
            };
            filled += cur.consumer.pop_slice(&mut output[filled..to_mix]);
            if filled == to_mix {
                break;
            }
            // Short. Only a *complete* input whose ring is now empty counts
            // as end of track. Flag first (Acquire), then drain once more:
            // everything pushed before the flag is visible.
            if !cur.input_complete.load(Ordering::Acquire) {
                break; // decoder underrun: never switch
            }
            filled += cur.consumer.pop_slice(&mut output[filled..to_mix]);
            if filled == to_mix || !cur.consumer.is_empty() || self.next.is_none() {
                break;
            }
            // Promote next; the boundary is the stream position right here.
            let boundary = self.produced_total + filled as u64;
            self.current = self.next.take();
            let id = self.current.as_ref().map_or(0, |s| s.id);
            self.status.publish_promotion(boundary, id);
            promoted = true;
        }
        if promoted {
            self.publish_ids();
        }

        if filled > 0 {
            // Single producer and `filled <= free_len`: writes everything.
            let pushed = self.output_producer.push_slice(&output[..filled]);
            debug_assert_eq!(pushed, filled);
            self.produced_total += pushed as u64;
            self.status
                .produced_total
                .store(self.produced_total, Ordering::Release);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::{HeapProducer, HeapRb};

    fn make_mixer(capacity: usize) -> (Mixer, HeapConsumer<f32>) {
        let (output_producer, output_consumer) = HeapRb::new(capacity).split();
        (Mixer::new(output_producer), output_consumer)
    }

    /// A slot pre-filled with `count` samples of `value`. Returns the track
    /// producer so tests can push more.
    fn slot(id: u64, count: usize, value: f32, complete: bool) -> (Slot, HeapProducer<f32>) {
        let (mut producer, consumer) = HeapRb::new(count + 1024).split();
        for _ in 0..count {
            producer.push(value).ok();
        }
        (
            Slot {
                id,
                consumer,
                input_complete: Arc::new(AtomicBool::new(complete)),
            },
            producer,
        )
    }

    fn drain(c: &mut HeapConsumer<f32>) -> Vec<f32> {
        let mut v = vec![0.0f32; c.len()];
        let n = c.pop_slice(&mut v);
        v.truncate(n);
        v
    }

    #[test]
    fn mix_writes_only_available_samples_not_full_callback_width() {
        let (mut mixer, mut out_c) = make_mixer(4096);
        let (s, _p) = slot(1, 100, 1.0, false);
        mixer.register_current(s);
        let mut output = vec![0.0f32; 3840];
        mixer.mix(&mut output, 3840).unwrap();
        let got = drain(&mut out_c);
        assert_eq!(got.len(), 100);
        assert!(got.iter().all(|s| (*s - 1.0).abs() < 1e-6));
    }

    #[test]
    fn mix_pushes_only_free_space_and_does_not_block() {
        let (mut mixer, mut out_c) = make_mixer(10);
        let (s, _p) = slot(1, 100, 1.0, false);
        mixer.register_current(s);
        let mut output = vec![0.0f32; 3840];
        mixer.mix(&mut output, 3840).unwrap();
        assert_eq!(out_c.len(), 10);
        // Unpopped samples stay in the track ring.
        assert_eq!(mixer.current.as_ref().unwrap().consumer.len(), 90);
        drain(&mut out_c);
        mixer.mix(&mut output, 3840).unwrap();
        assert_eq!(out_c.len(), 10);
    }

    #[test]
    fn mix_writes_nothing_when_empty_or_no_track() {
        let (mut mixer, out_c) = make_mixer(4096);
        let mut output = vec![0.0f32; 64];
        mixer.mix(&mut output, 64).unwrap();
        let (s, _p) = slot(1, 0, 0.0, false);
        mixer.register_current(s);
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(out_c.len(), 0);
    }

    #[test]
    fn promotes_next_within_same_block_without_gap() {
        let (mut mixer, mut out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 100, 1.0, true);
        let (b, _pb) = slot(2, 200, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        assert_eq!((status.current_id(), status.next_id()), (1, 2));
        assert_eq!(status.last_promotion(), None);

        let mut output = vec![0.0f32; 256];
        mixer.mix(&mut output, 256).unwrap();

        let got = drain(&mut out_c);
        assert_eq!(got.len(), 256);
        assert!(got[..100].iter().all(|s| *s == 1.0));
        assert!(got[100..].iter().all(|s| *s == 2.0), "contiguous, no zeros");
        assert_eq!(
            status.last_promotion(),
            Some(Promotion {
                boundary_sample: 100,
                promoted_id: 2
            })
        );
        assert_eq!(status.current_id(), 2);
        assert_eq!(status.next_id(), 0);
        assert_eq!(status.produced_total(), 256);
    }

    #[test]
    fn boundary_includes_previously_produced_samples() {
        let (mut mixer, mut out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, mut pa) = slot(1, 50, 1.0, false);
        let (b, _pb) = slot(2, 200, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        let mut output = vec![0.0f32; 256];
        mixer.mix(&mut output, 256).unwrap(); // 50 produced, incomplete
        assert_eq!(status.produced_total(), 50);
        for _ in 0..30 {
            pa.push(1.0).ok();
        }
        // Flag set after the last push.
        mixer
            .current
            .as_ref()
            .unwrap()
            .input_complete
            .store(true, Ordering::Release);
        mixer.mix(&mut output, 256).unwrap();
        assert_eq!(
            status.last_promotion(),
            Some(Promotion {
                boundary_sample: 80,
                promoted_id: 2
            })
        );
        // 50 + 30 from a, then all 200 of b (fewer than a full block).
        assert_eq!(drain(&mut out_c).len(), 50 + 30 + 200);
    }

    #[test]
    fn empty_but_incomplete_current_does_not_promote() {
        let (mut mixer, mut out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 100, 1.0, false);
        let (b, _pb) = slot(2, 200, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        let mut output = vec![0.0f32; 256];
        mixer.mix(&mut output, 256).unwrap();
        assert_eq!(drain(&mut out_c).len(), 100);
        assert_eq!(status.current_id(), 1);
        assert_eq!(status.next_id(), 2);
        assert_eq!(status.last_promotion(), None);
    }

    #[test]
    fn complete_current_without_next_stays_and_produces_nothing_more() {
        let (mut mixer, mut out_c) = make_mixer(4096);
        let (a, _pa) = slot(1, 100, 1.0, true);
        mixer.register_current(a);
        let mut output = vec![0.0f32; 256];
        mixer.mix(&mut output, 256).unwrap();
        assert_eq!(drain(&mut out_c).len(), 100);
        mixer.mix(&mut output, 256).unwrap();
        assert_eq!(out_c.len(), 0);
    }

    #[test]
    fn clear_next_ignored_for_promoted_id_and_removes_pending() {
        let (mut mixer, _out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 10, 1.0, true);
        let (b, _pb) = slot(2, 10, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        let mut output = vec![0.0f32; 256];
        mixer.mix(&mut output, 256).unwrap(); // promotes 2
        assert_eq!(status.current_id(), 2);
        mixer.clear_next(2);
        assert_eq!(status.current_id(), 2, "promoted id must be ignored");
        assert!(mixer.current.is_some());

        let (c, _pc) = slot(3, 10, 3.0, true);
        mixer.register_next(c);
        assert_eq!(status.next_id(), 3);
        mixer.clear_next(99);
        assert_eq!(status.next_id(), 3, "other ids ignored");
        mixer.clear_next(3);
        assert_eq!(status.next_id(), 0);
        assert_eq!(status.current_id(), 2);
    }

    #[test]
    fn clear_all_clears_both_slots() {
        let (mut mixer, out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 10, 1.0, false);
        let (b, _pb) = slot(2, 10, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        mixer.clear_all();
        assert_eq!((status.current_id(), status.next_id()), (0, 0));
        let mut output = vec![0.0f32; 64];
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(out_c.len(), 0);
    }

    #[test]
    fn produced_total_counts_every_pushed_sample() {
        let (mut mixer, _out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, mut pa) = slot(1, 30, 1.0, false);
        mixer.register_current(a);
        let mut output = vec![0.0f32; 64];
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(status.produced_total(), 30);
        for _ in 0..20 {
            pa.push(1.0).ok();
        }
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(status.produced_total(), 50);
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(status.produced_total(), 50);
    }

    #[test]
    fn register_next_without_current_becomes_current() {
        let (mut mixer, _out_c) = make_mixer(64);
        let status = mixer.status();
        let (b, _pb) = slot(5, 1, 1.0, false);
        mixer.register_next(b);
        assert_eq!((status.current_id(), status.next_id()), (5, 0));
    }

    #[test]
    fn snapshot_reports_ids_and_promotion_together() {
        let (mut mixer, _out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 10, 1.0, true);
        let (b, _pb) = slot(2, 10, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        let before = status.snapshot();
        assert_eq!((before.current_id, before.next_id), (1, 2));
        assert_eq!(before.last_promotion, None);
        assert!(before.references(1) && before.references(2));
        assert!(!before.references(0), "0 means none");

        let mut output = vec![0.0f32; 64];
        mixer.mix(&mut output, 64).unwrap();
        let after = status.snapshot();
        assert_eq!((after.current_id, after.next_id), (2, 0));
        assert_eq!(
            after.last_promotion,
            Some(Promotion {
                boundary_sample: 10,
                promoted_id: 2
            })
        );
        assert!(!after.references(1));
    }

    #[test]
    fn clear_current_keeps_next_and_produces_nothing() {
        let (mut mixer, out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 100, 1.0, true);
        let (b, _pb) = slot(2, 100, 2.0, true);
        mixer.register_current(a);
        mixer.register_next(b);
        mixer.clear_current();
        assert_eq!((status.current_id(), status.next_id()), (0, 2));
        let mut output = vec![0.0f32; 64];
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(out_c.len(), 0);
    }

    #[test]
    fn promote_next_makes_it_current_and_reports_start() {
        let (mut mixer, mut out_c) = make_mixer(4096);
        let status = mixer.status();
        let (a, _pa) = slot(1, 50, 1.0, false);
        let (b, _pb) = slot(2, 10, 2.0, false);
        mixer.register_current(a);
        let mut output = vec![0.0f32; 64];
        mixer.mix(&mut output, 64).unwrap();
        mixer.register_next(b);
        mixer.clear_current();
        assert_eq!(mixer.promote_next(9), None);
        assert_eq!(mixer.promote_next(2), Some(50));
        assert_eq!((status.current_id(), status.next_id()), (2, 0));
        assert_eq!(status.last_promotion(), None, "not a gapless promotion");
        drain(&mut out_c);
        mixer.mix(&mut output, 64).unwrap();
        assert_eq!(drain(&mut out_c), vec![2.0; 10]);
    }
}
