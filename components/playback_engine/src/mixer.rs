// in mixer.rs
use crate::error::PlaybackError;
use playback_primitives::Deck;
use ringbuf::{HeapConsumer, HeapProducer};
use std::collections::HashMap;

pub struct Mixer {
    mix_buffer: Vec<f32>,
    volumes: HashMap<Deck, f32>,
    output_producer: HeapProducer<f32>, // Mixer output
}

impl Mixer {
    pub fn new(output_producer: HeapProducer<f32>) -> Self {
        Self {
            mix_buffer: vec![0.0; 1920 * 2], // Buffer size for stereo
            volumes: HashMap::new(),
            output_producer,
        }
    }

    pub fn mix(
        &mut self,
        output: &mut [f32], // Temporary buffer
        samples_per_callback: usize,
        consumers: &mut HashMap<Deck, HeapConsumer<f32>>,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Active tracks counter for debug
        let mut active_tracks = 0;

        // Mix each active track
        for (deck, consumer) in consumers.iter_mut() {
            // Get volume (default to 1.0 if not set)
            let volume = *self.volumes.get(deck).unwrap_or(&1.0);

            // Read from consumer and mix with volume
            let available = consumer.len();
            let to_mix = std::cmp::min(available, samples_per_callback);

            if to_mix > 0 {
                tracing::debug!("Mixing {} samples from deck {:?}", to_mix, deck);
                active_tracks += 1;

                for i in 0..to_mix {
                    if let Some(sample) = consumer.pop() {
                        output[i] += sample * volume;
                    }
                }
            }
        }

        // Write mixed output to the output producer
        let mut written = 0;
        for i in 0..samples_per_callback {
            if self.output_producer.push(output[i]).is_ok() {
                written += 1;
            }
        }

        if written > 0 {
            tracing::debug!(
                "Wrote {} samples to mixer output, from {} active tracks",
                written,
                active_tracks
            );
        }

        Ok(())
    }

    pub(crate) fn set_volume(&mut self, deck: Deck, db: f32) {
        self.volumes.insert(deck, db);
    }
}
