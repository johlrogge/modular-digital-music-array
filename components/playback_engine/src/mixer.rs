// in mixer.rs
use crate::{error::PlaybackError, position::PlaybackPosition};
use playback_primitives::Deck;
use ringbuf::{HeapConsumer, HeapProducer};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
};

pub struct Mixer {
    volumes: HashMap<Deck, f32>,
    output_producer: HeapProducer<f32>, // Mixer output
    position_trackers: HashMap<Deck, Arc<PlaybackPosition>>, // NEW
}

impl Mixer {
    pub fn new(output_producer: HeapProducer<f32>) -> Self {
        Self {
            volumes: HashMap::new(),
            output_producer,
            position_trackers: HashMap::new(), // Initialize empty
        }
    }

    // Add method to register position tracker
    pub fn register_position_tracker(&mut self, deck: Deck, tracker: Arc<PlaybackPosition>) {
        self.position_trackers.insert(deck, tracker);
    }

    pub fn mix(
        &mut self,
        output: &mut [f32], // Temporary buffer for mixing
        samples_per_callback: usize,
        consumers: &mut HashMap<Deck, HeapConsumer<f32>>,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Mix each active track
        let mut active_tracks = 0;

        for (deck, consumer) in consumers.iter_mut() {
            // Get volume
            let volume = *self.volumes.get(deck).unwrap_or(&1.0);

            // Read from consumer and mix with volume
            let available = consumer.len();
            let to_mix = std::cmp::min(available, samples_per_callback);

            if to_mix > 0 {
                active_tracks += 1;

                // Mix samples
                for i in 0..to_mix {
                    if let Some(sample) = consumer.pop() {
                        output[i] += sample * volume;

                        // If we have a position tracker for this deck, record consumption
                        if let Some(tracker) = self.position_trackers.get(deck) {
                            tracker.record_consumption(1);
                        }
                    }
                }
            }
        }

        // Now write the mixed output to the output producer
        let mut written = 0;
        let to_write = samples_per_callback;

        while written < to_write {
            let remaining = to_write - written;
            let pushed = self.output_producer.push_slice(&output[written..to_write]);

            written += pushed;

            // If we couldn't write everything, yield and retry
            if pushed < remaining {
                tracing::debug!(
                    "Output buffer full, wrote {}/{} samples, yielding and retrying",
                    pushed,
                    remaining
                );
                std::thread::yield_now(); // Standard library yield, not tokio
            }
        }
        if written < to_write {
            tracing::debug!(
                "Output buffer full, {} samples unwritten",
                to_write - written
            );
        }

        Ok(())
    }
    pub(crate) fn set_volume(&mut self, deck: Deck, db: f32) {
        self.volumes.insert(deck, db);
    }
}
