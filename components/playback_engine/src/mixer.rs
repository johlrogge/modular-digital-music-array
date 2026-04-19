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
        // How many samples are ready from the track?
        let to_mix = match consumer.as_ref() {
            Some(c) => std::cmp::min(c.len(), samples_per_callback),
            None => 0,
        };

        if to_mix == 0 {
            // Nothing to produce this cycle. Let the mixer ring buffer drain
            // naturally; caller will retry after its own sleep.
            return Ok(());
        }

        // Clear only the slice we will fill
        output[..to_mix].fill(0.0);

        // Mix the single track
        if let Some(consumer) = consumer {
            (0..to_mix).for_each(|i| {
                if let Some(sample) = consumer.pop() {
                    output[i] += sample * self.volume;
                }
            });
        }

        // Write exactly `to_mix` samples — no silence padding
        let mut written = 0;
        while written < to_mix {
            let pushed = self.output_producer.push_slice(&output[written..to_mix]);
            written += pushed;
            if written < to_mix {
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
        }

        Ok(())
    }

    pub(crate) fn set_volume(&mut self, volume: Volume) {
        self.volume = volume.to_linear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;

    fn make_mixer(capacity: usize) -> (Mixer, HeapConsumer<f32>) {
        let output_rb = HeapRb::new(capacity);
        let (output_producer, output_consumer) = output_rb.split();
        let mixer = Mixer::new(output_producer);
        (mixer, output_consumer)
    }

    fn push_samples(count: usize, value: f32) -> (HeapConsumer<f32>, ringbuf::HeapProducer<f32>) {
        let rb = HeapRb::new(count + 1);
        let (mut producer, consumer) = rb.split();
        for _ in 0..count {
            producer.push(value).ok();
        }
        (consumer, producer)
    }

    #[test]
    fn mix_writes_only_available_samples_not_full_callback_width() {
        let (mut mixer, mut output_consumer) = make_mixer(4096);
        let (track_consumer, _track_producer) = push_samples(100, 1.0);
        let mut consumer_opt: Option<HeapConsumer<f32>> = Some(track_consumer);

        let mut output = vec![0.0f32; 3840];
        let result = mixer.mix(&mut output, 3840, &mut consumer_opt);
        assert!(result.is_ok());

        // Should have written exactly 100 samples, not 3840
        assert_eq!(
            output_consumer.len(),
            100,
            "expected 100 samples in output, got {}",
            output_consumer.len()
        );

        // Verify values are scaled by volume (default 1.0, so sample * 1.0 = 1.0)
        let mut drained = vec![0.0f32; 100];
        let read = output_consumer.pop_slice(&mut drained);
        assert_eq!(read, 100);
        for s in &drained {
            assert!((*s - 1.0).abs() < 1e-6, "expected sample 1.0, got {}", s);
        }
    }

    #[test]
    fn mix_returns_ok_and_writes_nothing_when_consumer_empty() {
        let (mut mixer, output_consumer) = make_mixer(4096);
        let (track_consumer, _track_producer) = push_samples(0, 0.0);
        let mut consumer_opt: Option<HeapConsumer<f32>> = Some(track_consumer);

        let mut output = vec![0.0f32; 3840];
        let result = mixer.mix(&mut output, 3840, &mut consumer_opt);
        assert!(result.is_ok());
        assert_eq!(output_consumer.len(), 0, "expected 0 samples in output");
    }

    #[test]
    fn mix_returns_ok_and_writes_nothing_when_no_consumer() {
        let (mut mixer, output_consumer) = make_mixer(4096);
        let mut consumer_opt: Option<HeapConsumer<f32>> = None;

        let mut output = vec![0.0f32; 3840];
        let result = mixer.mix(&mut output, 3840, &mut consumer_opt);
        assert!(result.is_ok());
        assert_eq!(output_consumer.len(), 0, "expected 0 samples in output");
    }
}
