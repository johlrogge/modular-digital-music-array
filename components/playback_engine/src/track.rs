use std::mem::MaybeUninit;
use std::path::Path;
use std::sync::Arc;

use ringbuf::{Consumer, HeapRb, Producer, SharedRb};

use crate::error::PlaybackError;
use crate::source::{FlacSource, Source};

pub struct Track<S: Source + Send + Sync> {
    source: Arc<S>,  // Shared audio source
    position: usize, // Current playback position
    playing: bool,   // Playback state
    volume: f32,     // Volume multiplier
    producer: Producer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>,
    consumer: Consumer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>,
}

impl<S: Source + Send + Sync> Track<S> {
    const BUFFER_SIZE: usize = 4096; // Small buffer size to start with

    pub async fn new(path: &Path) -> Result<Track<FlacSource>, PlaybackError> {
        let source = Arc::new(FlacSource::new(path)?);

        // Create the ring buffer and wrap it in Arc
        let rb = HeapRb::<f32>::new(Self::BUFFER_SIZE);
        let (producer, consumer) = rb.split();

        Ok(Track {
            source,
            position: 0,
            playing: false,
            volume: 1.0,
            producer,
            consumer,
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
        tracing::info!(
            "Track playback started: playing={}, position={}, volume={}",
            self.playing,
            self.position,
            self.volume
        );
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        let max_position = self.source.len();
        let target_position = position.min(max_position);

        // Try to perform a real seek via the source's seek method
        // Now using immutable reference which works with Arc
        if let Err(e) = self.source.seek(position) {
            tracing::warn!("Source seek failed: {}", e);
            // Continue anyway - we'll update the position counter
        }

        // Update position counter
        self.position = target_position;

        tracing::info!(
            "Track seeked to position={}/{}, playing={}, volume={}",
            self.position,
            max_position,
            self.playing,
            self.volume
        );

        Ok(())
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn length(&self) -> usize {
        self.source.len()
    }

    pub fn get_next_samples(&mut self, output: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            return Ok(0);
        }

        // First, try to read any existing data from the ring buffer
        let mut read_from_buffer = 0;
        for i in 0..output.len() {
            if let Some(sample) = self.consumer.pop() {
                output[i] = sample;
                read_from_buffer += 1;
            } else {
                break;
            }
        }

        // If we read enough from the buffer, we're done
        if read_from_buffer == output.len() {
            // Apply volume
            for sample in &mut output[..read_from_buffer] {
                *sample *= self.volume;
            }
            self.position += read_from_buffer;
            return Ok(read_from_buffer);
        }

        // Otherwise, read directly from the source
        let remaining = output.len() - read_from_buffer;
        let read_from_source = self.source.read_samples(
            self.position + read_from_buffer,
            &mut output[read_from_buffer..],
        )?;

        if read_from_source == 0 && read_from_buffer == 0 {
            self.playing = false;
            return Ok(0);
        }

        // Apply volume
        let total_read = read_from_buffer + read_from_source;
        for sample in &mut output[..total_read] {
            *sample *= self.volume;
        }

        self.position += total_read;

        // Try to fill the buffer with new data for next time
        self.fill_buffer()?;

        Ok(total_read)
    }

    // Simple method to fill the buffer
    fn fill_buffer(&mut self) -> Result<usize, PlaybackError> {
        // Get the available space in the producer
        let space = self.producer.capacity() - self.producer.len();
        if space == 0 {
            return Ok(0);
        }

        // Create temporary buffer for reading
        let mut temp = vec![0.0; space.min(1024)]; // Read at most 1024 samples at a time

        // Calculate the position from which to read more data
        let next_position = self.position;

        // Read from source
        let read = self.source.read_samples(next_position, &mut temp)?;

        // Push samples to buffer
        let mut written = 0;
        for i in 0..read {
            if self.producer.push(temp[i]).is_ok() {
                written += 1;
            } else {
                break; // Buffer is full
            }
        }

        Ok(written)
    }
    pub fn stop(&mut self) {
        self.playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_volume(&mut self, db: f32) {
        // Convert dB to linear amplitude
        self.volume = 10.0f32.powf(db / 20.0);
    }

    pub fn get_volume(&self) -> f32 {
        self.volume
    }
}

#[cfg(test)]
pub struct TestSource {
    samples: Vec<f32>,
}

#[cfg(test)]
impl Source for TestSource {
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        if position >= self.samples.len() {
            return Ok(0);
        }
        let available = self.samples.len() - position;
        let count = buffer.len().min(available);

        buffer[..count].copy_from_slice(&self.samples[position..position + count]);
        Ok(count)
    }

    fn sample_rate(&self) -> u32 {
        48000
    }

    fn audio_channels(&self) -> u16 {
        2
    }

    fn len(&self) -> usize {
        self.samples.len()
    }
}

#[cfg(test)]
impl Track<TestSource> {
    pub(crate) fn new_test() -> Self {
        // Create a simple 1-second sine wave source

        // Generate 1 second of 440Hz test tone
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let mut samples = Vec::with_capacity(sample_rate);

        for i in 0..sample_rate {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.1;
            samples.push(sample);
        }

        // Create the ring buffer and wrap it in Arc
        let rb = HeapRb::<f32>::new(Self::BUFFER_SIZE);
        let (producer, consumer) = rb.split();
        Self {
            source: Arc::new(TestSource { samples }),
            position: 0,
            playing: false,
            volume: 1.0,
            producer,
            consumer,
        }
    }
}
