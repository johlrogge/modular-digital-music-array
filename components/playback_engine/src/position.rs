use std::sync::atomic::{AtomicUsize, Ordering};

pub struct PlaybackPosition {
    // Current position in the track (samples)
    track_position: AtomicUsize,
    // How many samples have been consumed by the mixer
    pub consumed_samples: AtomicUsize,
    // How many samples we've pushed to the ringbuffer
    pub pushed_samples: AtomicUsize,
}

impl PlaybackPosition {
    pub fn new() -> Self {
        Self {
            track_position: AtomicUsize::new(0),
            consumed_samples: AtomicUsize::new(0),
            pushed_samples: AtomicUsize::new(0),
        }
    }

    pub fn record_push(&self, count: usize) {
        self.pushed_samples.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_consumption(&self, count: usize) {
        let consumed = self.consumed_samples.fetch_add(count, Ordering::Relaxed);

        // Update track position based on consumption, not pushing
        self.track_position.store(consumed, Ordering::Relaxed);
    }

    pub fn position(&self) -> usize {
        self.track_position.load(Ordering::Relaxed)
    }

    pub fn seek(&self, position: usize) {
        // Reset counters on seek
        self.track_position.store(position, Ordering::Relaxed);
        self.consumed_samples.store(position, Ordering::Relaxed);
        self.pushed_samples.store(position, Ordering::Relaxed);
    }

    // For debugging
    pub fn buffer_state(&self) -> (usize, usize, usize) {
        (
            self.track_position.load(Ordering::Relaxed),
            self.consumed_samples.load(Ordering::Relaxed),
            self.pushed_samples.load(Ordering::Relaxed),
        )
    }
}
