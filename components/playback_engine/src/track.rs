use crate::error::PlaybackError;
#[cfg(test)]
use crate::source::{AudioSegment, SegmentIndex, SEGMENT_SIZE};
use crate::source::{DecodedSegment, Source};

use tokio::sync::mpsc;

use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use std::sync::{atomic::AtomicBool, Arc};

use ringbuf::HeapProducer;
#[cfg(test)]
use ringbuf::HeapRb;

pub struct Track {
    playing: Arc<AtomicBool>,
    command_tx: mpsc::Sender<TrackCommand>,
    decoder_task: Option<tokio::task::JoinHandle<()>>,
    ready_tx: tokio::sync::watch::Sender<bool>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
}

// Update TrackCommand to include potential new commands
pub enum TrackCommand {
    FillFrom(usize),
    Shutdown,
}

async fn decoder_task<S: Source + Send + Sync + 'static>(
    source: S,
    mut output: HeapProducer<f32>,
    mut command_rx: mpsc::Receiver<TrackCommand>,
) {
    let mut decoded_segments = VecDeque::new();
    let mut next_segment: Option<DecodedSegment> = None;
    let mut written: usize = 0;

    loop {
        if decoded_segments.is_empty() {
            tracing::debug!("no more segments, decode");
            match source.decode_next_frame() {
                Ok(next_frame) => {
                    for segment in next_frame {
                        decoded_segments.push_back(segment);
                    }
                }
                Err(error) => {
                    tracing::error!("failed to decode segment: {error}");
                }
            }
        }

        if let Some(ref segment) = next_segment {
            let to_write = segment.segment.samples.len() - written;
            let actually_written = output.push_slice(&segment.segment.samples[written..]);
            written += actually_written;

            // Check if we've written the entire segment
            if written == segment.segment.samples.len() {
                next_segment = None;
                written = 0;
            }

            // If we couldn't write everything, yield to let the mixer consume some data
            if actually_written < to_write {
                tokio::task::yield_now().await;
            }
        } else {
            next_segment = decoded_segments.pop_front();
        }

        while let Ok(command) = command_rx.try_recv() {
            match command {
                TrackCommand::FillFrom(position) => {
                    tracing::debug!("seek to {position}");
                    if let Err(res) = source.seek(position) {
                        tracing::error!("failed to seek {res}");
                    } else {
                        //current_position = position;
                        tracing::debug!("seeked to position {position}");
                    }
                }
                TrackCommand::Shutdown => {
                    tracing::info!("Decoder task received shutdown command");
                    return;
                }
            }
        }
    }
}
impl Track {
    pub async fn new<S: Source + Send + Sync + 'static>(
        source: S,
        output_producer: HeapProducer<f32>,
    ) -> Result<Self, PlaybackError> {
        let playing = Arc::new(AtomicBool::new(false));

        // Command channels
        let (command_tx, command_rx) = mpsc::channel(32);
        let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);

        // Create decoder task
        let decoder_task = tokio::spawn(async move {
            decoder_task(source, output_producer, command_rx).await;
        });

        let track = Self {
            playing,
            command_tx,
            decoder_task: Some(decoder_task),
            ready_tx,
            ready_rx,
        };

        Ok(track)
    }

    // Update seek to use the tracker
    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        // Request buffer filling from new position (unchanged)
        if let Err(e) = self.command_tx.try_send(TrackCommand::FillFrom(position)) {
            tracing::error!("Failed to send fill command after seek: {}", e);
        }

        Ok(())
    }

    pub fn play(&mut self) {
        // Wait for the track to be ready - but since we can't await here,
        // we'll just log and continue
        if !self.is_ready() {
            tracing::warn!("Playing track that's not ready yet");
        }

        self.playing.store(true, Ordering::Relaxed);
        tracing::info!("Track set to playing state");
    }

    pub fn is_ready(&self) -> bool {
        *self.ready_rx.borrow()
    }

    pub fn stop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        tracing::info!("Track drop beginning");

        // 1. Send shutdown command first
        let _ = self.command_tx.try_send(TrackCommand::Shutdown);
        tracing::info!("Shutdown command sent (or attempted)");

        // 2. Close the command channel
        // Create dummy sender - this safely drops the original
        let dummy_tx = mpsc::channel::<TrackCommand>(1).0;
        let _ = std::mem::replace(&mut self.command_tx, dummy_tx);
        tracing::info!("Command channel closed");

        // 3. Abort the decoder task
        if let Some(task) = self.decoder_task.take() {
            tracing::info!("Aborting decoder task");
            task.abort();
        }

        tracing::info!("Track drop completed");
    }
}

#[cfg(test)]
pub struct TestSource {
    position: AtomicUsize, // Track which frame we're on
    samples: Vec<Vec<DecodedSegment>>,
    current_sample_position: AtomicUsize, // Track current sample position
}

#[cfg(test)]
impl TestSource {
    pub fn new_from_samples(samples: Vec<f32>) -> Self {
        let segments = Self::create_segments_from_samples(samples);
        Self {
            position: AtomicUsize::new(0),
            samples: vec![segments], // Wrap in vector to simulate frames
            current_sample_position: AtomicUsize::new(0), // Initialize to 0
        }
    }

    // Create decoded segments from a flat vector of samples
    fn create_segments_from_samples(samples: Vec<f32>) -> Vec<DecodedSegment> {
        let mut segments = Vec::new();
        let mut start_pos = 0;

        // Process complete segments (of SEGMENT_SIZE)
        for chunk_idx in 0..(samples.len() / SEGMENT_SIZE) {
            let segment_index = SegmentIndex::from_sample_position(start_pos);

            // Create segment data
            let mut segment_samples = [0.0; SEGMENT_SIZE];
            let start = chunk_idx * SEGMENT_SIZE;
            let end = start + SEGMENT_SIZE;

            segment_samples.copy_from_slice(&samples[start..end]);

            // Add segment
            segments.push(DecodedSegment {
                index: segment_index,
                segment: AudioSegment {
                    samples: segment_samples,
                },
            });

            start_pos += SEGMENT_SIZE;
        }

        // Handle any remaining samples (partial segment)
        let remaining = samples.len() % SEGMENT_SIZE;
        if remaining > 0 {
            let segment_index = SegmentIndex::from_sample_position(start_pos);

            // Create segment data
            let mut segment_samples = [0.0; SEGMENT_SIZE];
            let start = samples.len() - remaining;

            // Copy remaining samples and leave the rest as zeros
            segment_samples[..remaining].copy_from_slice(&samples[start..]);

            // Add segment
            segments.push(DecodedSegment {
                index: segment_index,
                segment: AudioSegment {
                    samples: segment_samples,
                },
            });
        }

        segments
    }
    fn adjust_segment_indices(&self, segments: Vec<DecodedSegment>) -> Vec<DecodedSegment> {
        let seek_pos = self.current_sample_position.load(Ordering::Relaxed);
        if seek_pos == 0 {
            return segments; // No adjustment needed
        }

        // Create base segment index from seek position
        let base_index = SegmentIndex::from_sample_position(seek_pos);

        // Adjust each segment's index
        segments
            .into_iter()
            .enumerate()
            .map(|(i, mut segment)| {
                segment.index = SegmentIndex(base_index.0 + i);
                segment
            })
            .collect()
    }
    // Convenience method to generate various test patterns
    pub fn new_with_pattern(pattern: &str, seconds: f32) -> Self {
        let sample_rate = 48000;
        let channels = 2;
        let total_samples = (seconds * sample_rate as f32 * channels as f32) as usize;

        let samples = match pattern {
            "sine" => {
                // Generate sine wave at 440Hz
                let mut data = Vec::with_capacity(total_samples);
                let frequency = 440.0;

                for i in 0..total_samples {
                    let t = i as f32 / (sample_rate as f32 * channels as f32);
                    let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
                    data.push(sample);
                }
                data
            }
            "ascending" => {
                // Generate ascending ramp from -0.9 to 0.9
                let mut data = Vec::with_capacity(total_samples);
                for i in 0..total_samples {
                    let sample = -0.9 + (1.8 * i as f32 / total_samples as f32);
                    data.push(sample);
                }
                data
            }
            "alternating" => {
                // Generate alternating pattern (high, zero, low)
                let mut data = Vec::with_capacity(total_samples);
                for i in 0..total_samples {
                    let sample = match i % 3 {
                        0 => 0.9,
                        1 => 0.0,
                        _ => -0.9,
                    };
                    data.push(sample);
                }
                data
            }
            "silence" => {
                // All zeros
                vec![0.0; total_samples]
            }
            "impulses" => {
                // Periodic impulses
                let mut data = Vec::with_capacity(total_samples);
                for i in 0..total_samples {
                    let sample = if i % 100 == 0 { 0.9 } else { 0.0 };
                    data.push(sample);
                }
                data
            }
            _ => {
                // Default to silence if pattern unknown
                vec![0.0; total_samples]
            }
        };

        Self::new_from_samples(samples)
    }
}

#[cfg(test)]
impl Source for TestSource {
    fn decode_next_frame(&self) -> Result<Vec<DecodedSegment>, PlaybackError> {
        // Get current position
        let pos = self.position.load(Ordering::Relaxed);

        // Check if we have any more frames
        if pos >= self.samples.len() {
            return Ok(Vec::new()); // EOF
        }

        // Get the current frame's segments
        let segments = self.samples[pos].clone();

        // Calculate how many samples this represents
        let sample_count: usize = segments.iter().map(|s| s.segment.samples.len()).sum();

        // Adjust segment indices based on seek position
        let adjusted_segments = self.adjust_segment_indices(segments);

        // Move to next frame
        self.position.store(pos + 1, Ordering::Relaxed);

        // Update current sample position
        let current_pos = self.current_sample_position.load(Ordering::Relaxed);
        self.current_sample_position
            .store(current_pos + sample_count, Ordering::Relaxed);

        Ok(adjusted_segments)
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // Store the target sample position
        self.current_sample_position
            .store(position, Ordering::Relaxed);

        // Reset frame position to beginning
        self.position.store(0, Ordering::Relaxed);

        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        48000
    }

    fn audio_channels(&self) -> u16 {
        2
    }

    fn current_position(&self) -> usize {
        self.current_sample_position.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
impl Track {
    pub(crate) async fn new_test() -> Result<Self, PlaybackError> {
        // Generate 1 second of 440Hz test tone
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let mut samples = Vec::with_capacity(sample_rate);

        for i in 0..sample_rate {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.1;
            samples.push(sample);
        }
        let buffer = HeapRb::new(1024 * 8);
        let (prod, cons) = buffer.split();
        Self::new(TestSource::new_from_samples(samples), prod).await
    }

    // Add this method for tests
    pub(crate) async fn ensure_ready_for_test(&mut self) -> Result<(), PlaybackError> {
        Ok(())
    }
}
