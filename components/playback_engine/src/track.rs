use crate::buffer::SegmentedBuffer;
use crate::error::PlaybackError;
use crate::source::Source;
#[cfg(test)]
use crate::source::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};

use tokio::sync::mpsc;

use parking_lot::RwLock; // Using parking_lot for better RwLock performance
use std::sync::Arc;

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

    async fn buffer_management_task(
        source: Arc<S>,
        buffer: Arc<RwLock<SegmentedBuffer>>,
        mut command_rx: mpsc::Receiver<TrackCommand>,
    ) {
        tracing::info!("Buffer management task started");

        // Start with frequent polling, then back off
        let mut poll_interval_ms = 10;

        loop {
            // Use timeout to periodically check even if no commands are received
            let command = tokio::time::timeout(
                tokio::time::Duration::from_millis(poll_interval_ms),
                command_rx.recv(),
            )
            .await;

            // If buffer is full enough, gradually increase poll interval
            // to reduce CPU usage (up to 100ms max)
            if poll_interval_ms < 100 {
                poll_interval_ms = (poll_interval_ms + 5).min(100);
            }

            // Check if we should continue processing
            match command {
                Ok(Some(TrackCommand::FillFrom(position))) => {
                    // For initial buffer fill, fetch less data but faster
                    let segments_per_batch = if position == 0 {
                        // Only fetch a few segments to get started quickly
                        2
                    } else {
                        // For continued filling, fetch more at once
                        10
                    };

                    // Seek to position
                    if let Err(e) = source.seek(position) {
                        tracing::error!("Failed to seek source: {}", e);
                        continue;
                    }

                    // Decode segments with adjusted batch size
                    match source.decode_segments(segments_per_batch) {
                        Ok(segments) => {
                            if segments.is_empty() {
                                tracing::debug!("No segments decoded");
                            } else {
                                tracing::debug!("Decoded {} segments", segments.len());

                                // Add segments to buffer
                                let mut buffer = buffer.write();
                                buffer.add_segments(segments);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to decode segments: {}", e);
                        }
                    }
                }
                Ok(Some(TrackCommand::Shutdown)) => {
                    tracing::info!("Shutdown command received");
                    break;
                }
                Ok(None) => {
                    // Channel closed
                    tracing::info!("Command channel closed, shutting down");
                    break;
                }
                Err(_) => {
                    // Timeout occurred - just continue and check for next command
                    tracing::trace!("Timeout waiting for command");

                    // Yield to allow task cancellation to be detected
                    tokio::task::yield_now().await;
                }
            }
        }

        tracing::info!("Buffer management task shutting down");

        // Cleanup before exit
        {
            // Clear the buffer to release memory
            let mut buffer_lock = buffer.write();
            buffer_lock.clear();
        }

        // Explicitly drop our reference to the source
        drop(source);

        tracing::info!("Buffer management task shut down, all resources released");
    }

    pub fn play(&mut self) {
        let start = std::time::Instant::now();

        // Start filling the buffer first
        if let Err(e) = self.fill_buffer() {
            tracing::warn!("Failed to fill buffer: {}", e);
        }

        // Wait for buffer to be ready
        self.await_ready();

        // Calculate time to ready
        let ready_time = start.elapsed();

        // Now mark as playing
        self.playing = true;

        tracing::info!(
            "Track playback started in {:?}: playing={}, position={}, volume={}",
            ready_time,
            self.playing,
            self.position,
            self.volume
        );
    }

    /// Wait for buffer to fill with a shorter timeout
    fn await_ready(&mut self) {
        let start = std::time::Instant::now();
        // Reduce from 100ms to 10ms for quicker startup
        let timeout = std::time::Duration::from_millis(10);

        while !self.is_ready() {
            if start.elapsed() > timeout {
                tracing::warn!("Buffer fill timeout - starting playback with partial buffer");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        // Update position
        self.position = position;

        // Clear the buffer
        {
            let mut buffer = self.buffer.write();
            buffer.clear();
        }

        // Send command to fill buffer from new position
        if let Err(e) = self
            .command_tx
            .try_send(TrackCommand::FillFrom(self.position))
        {
            tracing::warn!("Failed to send fill command after seek: {}", e);
        }

        tracing::info!(
            "Track seeked to position={}, playing={}, volume={}",
            self.position,
            self.playing,
            self.volume
        );

        Ok(())
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn length(&self) -> usize {
        // Use total_samples() instead of len()
        self.source.total_samples().unwrap_or(0)
    }

    pub fn is_ready(&self) -> bool {
        // We only need a minimal buffer to be ready to start playing
        // Check if buffer has at least one segment at current position
        let buffer = self.buffer.read();
        buffer.is_ready_at(self.position)
    }

    pub fn get_next_samples(&mut self, output: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            return Ok(0);
        }

        // Read from the buffer
        let samples_read = {
            let buffer = self.buffer.read();
            buffer.get_samples(self.position, output)
        };

        if samples_read > 0 {
            // Apply volume
            for sample in &mut output[..samples_read] {
                *sample *= self.volume;
            }

            // Update position
            self.position += samples_read;

            // If we didn't read enough samples, request more
            if samples_read < output.len() {
                self.fill_buffer()?;
            }

            return Ok(samples_read);
        }

        // Buffer didn't have our data, try to fill it
        self.fill_buffer()?;

        // Check if we've reached the end
        let total_samples = self.source.total_samples();
        if let Some(len) = total_samples {
            if self.position >= len {
                self.playing = false;
                tracing::info!("End of track reached");
                return Ok(0);
            }
        }

        // No samples read this time
        Ok(0)
    }

    fn fill_buffer(&mut self) -> Result<(), PlaybackError> {
        // Send a command to fill the buffer from the current position
        if let Err(e) = self
            .command_tx
            .try_send(TrackCommand::FillFrom(self.position))
        {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    // Channel is full, buffer is probably being filled already
                    // Just continue
                }
                mpsc::error::TrySendError::Closed(_) => {
                    // Channel is closed, this is an error
                    return Err(PlaybackError::AudioDevice(
                        "Buffer management task stopped".into(),
                    ));
                }
            }
        }

        Ok(())
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

    fn total_samples(&self) -> Option<usize> {
        Some(self.samples.len())
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
