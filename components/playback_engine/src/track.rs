use crate::buffer::SegmentedBuffer;
use crate::error::PlaybackError;
use crate::source::Source;
use crate::source::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};

use tokio::sync::mpsc;

use parking_lot::RwLock;
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
    samples: Vec<f32>,
}

#[cfg(test)]
impl Source for TestSource {
    fn decode_segments(&self, max_segments: usize) -> Result<Vec<DecodedSegment>, PlaybackError> {
        // Early return if no more data
        if self.samples.is_empty() {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();
        let mut remaining_samples = self.samples.len();
        let mut current_position = 0;

        // Create segments until we run out of samples or reach max_segments
        for _ in 0..max_segments {
            if remaining_samples == 0 {
                break;
            }

            // Calculate segment index
            let segment_index = SegmentIndex::from_sample_position(current_position);

            // Create a segment
            let mut segment = AudioSegment {
                samples: [0.0; SEGMENT_SIZE],
            };

            // Calculate how many samples to copy
            let samples_to_copy = std::cmp::min(remaining_samples, SEGMENT_SIZE);

            // Copy samples to segment (zero-padded if needed)
            for i in 0..samples_to_copy {
                segment.samples[i] = self.samples[current_position + i];
            }

            // For any remaining samples in the segment, fill with zeros
            for i in samples_to_copy..SEGMENT_SIZE {
                segment.samples[i] = 0.0;
            }

            // Add segment to result
            result.push(DecodedSegment {
                index: segment_index,
                segment,
            });

            // Update position and remaining count
            current_position += samples_to_copy;
            remaining_samples -= samples_to_copy;
        }

        Ok(result)
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // For TestSource, seek is a no-op since we have all samples in memory
        // Just validate the position is within bounds
        if position > self.samples.len() {
            return Err(PlaybackError::Decoder("Seek position out of bounds".into()));
        }
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

        Self::new(TestSource { samples }).await
    }

    // Add this method for tests
    pub(crate) async fn ensure_ready_for_test(&mut self) -> Result<(), PlaybackError> {
        // For test tracks, explicitly fill the buffer immediately
        let segment = self.source.decode_segments(1)?;

        {
            let mut buffer = self.buffer.write();
            buffer.add_segments(segment);
        }

        // Wait a bit to ensure buffer management task processes the data
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        Ok(())
    }
}
