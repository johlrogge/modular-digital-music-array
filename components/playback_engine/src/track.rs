use crate::buffer::SegmentedBuffer;
use crate::error::PlaybackError;
use crate::source::Source;
use crate::source::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};

use tokio::sync::mpsc;

use parking_lot::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::warn; // Using parking_lot for better RwLock performance

pub struct Track<S: Source + Send + Sync + 'static> {
    source: Arc<S>,
    position: usize,
    playing: bool,
    volume: f32,
    buffer: Arc<RwLock<SegmentedBuffer>>,
    command_tx: mpsc::Sender<TrackCommand>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

// Update TrackCommand to include potential new commands
pub enum TrackCommand {
    FillFrom(usize),
    Shutdown,
}

impl<S: Source + Send + Sync> Track<S> {
    pub async fn new(source: S) -> Result<Self, PlaybackError> {
        let source = Arc::new(source);
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));
        let (command_tx, command_rx) = mpsc::channel(32);

        // Add a dedicated shutdown channel

        let source_clone = Arc::clone(&source);
        let buffer_clone = Arc::clone(&buffer);

        let task_handle = tokio::spawn(async move {
            Self::buffer_management_task(source_clone, buffer_clone, command_rx).await;
        });

        let track = Self {
            source,
            position: 0,
            playing: false,
            volume: 1.0,
            buffer,
            command_tx,
            task_handle: Some(task_handle),
        };

        // Send initial fill command
        track
            .command_tx
            .send(TrackCommand::FillFrom(0))
            .await
            .expect("failed to send track command");

        Ok(track)
    }

    // In track.rs - modify the buffer_management_task
    async fn buffer_management_task(
        source: Arc<S>,
        buffer: Arc<RwLock<SegmentedBuffer>>,
        mut command_rx: mpsc::Receiver<TrackCommand>,
    ) {
        todo!("implement background loading")
    }

    pub fn play(&mut self) {
        todo!("Implement play")
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        todo!("implement seek")
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn is_ready(&self) -> bool {
        todo!("impement is_ready")
    }

    pub fn get_next_samples(&mut self, output: &mut [f32]) -> Result<usize, PlaybackError> {
        todo!("implememnt get next samples")
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

impl<S: Source + Send + Sync + 'static> Drop for Track<S> {
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

        // 3. Abort the task
        if let Some(task) = self.task_handle.take() {
            tracing::info!("Aborting background task");
            task.abort();
        }

        // 4. Clear buffer
        {
            let mut buffer = self.buffer.write();
            buffer.clear();
            tracing::info!("Buffer cleared");
        }

        tracing::info!("Track drop completed");
    }
}

#[cfg(test)]
pub struct TestSource {
    position: AtomicUsize,
    samples: Vec<Vec<DecodedSegment>>,
}

#[cfg(test)]
impl TestSource {
    // Create a new TestSource from raw samples
    pub fn new_from_samples(samples: Vec<f32>) -> Self {
        let segments = Self::create_segments_from_samples(samples);
        Self {
            position: AtomicUsize::new(0),
            samples: vec![segments], // Wrap in vector to simulate frames
        }
    }

    // Create a test source with multiple frames (for more complex testing)
    pub fn new_with_frames(frames: Vec<Vec<f32>>) -> Self {
        let frames_of_segments = frames
            .into_iter()
            .map(Self::create_segments_from_samples)
            .collect();

        Self {
            position: AtomicUsize::new(0),
            samples: frames_of_segments,
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
        let result = self.samples[pos].clone();

        // Move to next frame
        self.position.store(pos + 1, Ordering::Relaxed);

        Ok(result)
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // For TestSource, position refers to frame index
        if position >= self.samples.len() {
            return Err(PlaybackError::Decoder("Seek position out of bounds".into()));
        }

        // Update position
        self.position.store(position, Ordering::Relaxed);

        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        48000
    }

    fn audio_channels(&self) -> u16 {
        2
    }
}

#[cfg(test)]
impl Track<TestSource> {
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

        Self::new(TestSource::new_from_samples(samples)).await
    }

    // Add this method for tests
    pub(crate) async fn ensure_ready_for_test(&mut self) -> Result<(), PlaybackError> {
        // For test tracks, explicitly fill the buffer immediately
        let segment = self.source.decode_next_frame()?;

        {
            let mut buffer = self.buffer.write();
            buffer.add_segments(segment);
        }

        // Wait a bit to ensure buffer management task processes the data
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        Ok(())
    }
}
