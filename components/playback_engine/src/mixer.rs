use crate::error::PlaybackError;
use playback_primitives::{Db, Volume};
use ringbuf::{HeapConsumer, HeapProducer};

pub struct Mixer {
    volume: f32,
    output_producer: HeapProducer<f32>,
}

impl Mixer {
    pub fn new(output_producer: HeapProducer<f32>) -> Self {
        Self {
            volume: 1.0,
            output_producer,
        }
    }

    pub fn mix(
        &mut self,
        output: &mut [f32],
        samples_per_callback: usize,
        consumer: &mut Option<HeapConsumer<f32>>,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Mix the single track if present
        if let Some(consumer) = consumer {
            let available = consumer.len();
            let to_mix = std::cmp::min(available, samples_per_callback);

            if to_mix > 0 {
                (0..to_mix).for_each(|i| {
                    if let Some(sample) = consumer.pop() {
                        output[i] += sample * self.volume;
                    }
                });
            }
        }

        // Write the mixed output to the output producer
        let mut written = 0;
        let to_write = samples_per_callback;

        let mut write_attempts = 0;
        while written < to_write {
            let remaining = to_write - written;
            let pushed = self.output_producer.push_slice(&output[written..to_write]);

            written += pushed;

            if pushed < remaining {
                write_attempts += 1;
                if write_attempts % 1_000_000 == 0 {
                    tracing::debug!(
                        "Output buffer full attempt {}, wrote {}/{} samples, yielding and retrying",
                        write_attempts,
                        pushed,
                        remaining
                    );
                }
                std::thread::sleep(std::time::Duration::from_micros(500));
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

    pub(crate) fn set_volume(&mut self, volume: Volume) {
        self.volume = volume.to_linear();
    }
}
