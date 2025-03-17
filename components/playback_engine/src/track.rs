use crate::buffer::SegmentedBuffer;
use crate::error::PlaybackError;
#[cfg(test)]
use crate::source::{AudioSegment, DecodedSegment};
use crate::source::{SegmentIndex, Source, SEGMENT_SIZE};

use tokio::sync::mpsc;

use parking_lot::RwLock;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ringbuf::HeapRb;

pub struct Track<S: Source + Send + Sync + 'static> {
    source: Arc<S>,
    position: usize,
    playing: bool,
    volume: f32,
    buffer: Arc<RwLock<SegmentedBuffer>>,
    command_tx: mpsc::Sender<TrackCommand>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
    // Add the ringbuffer with producers/consumers
    sample_buffer: Arc<RwLock<HeapRb<f32>>>,
    ready_tx: tokio::sync::watch::Sender<bool>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
}

// Update TrackCommand to include potential new commands
pub enum TrackCommand {
    FillFrom(usize),
    Shutdown,
}

async fn load_segments_with<S: Source + Send + Sync>(
    source: &Arc<S>,
    buffer: &Arc<RwLock<SegmentedBuffer>>,
    position: usize,
    count: usize,
) -> usize {
    let mut loaded_count = 0;

    for offset in 0..count {
        let segment_index = SegmentIndex::from_sample_position(position + offset * SEGMENT_SIZE);
        let segment_position = segment_index.start_position();

        // Check if segment already loaded in buffer
        let needs_loading = {
            let buffer = buffer.read();
            !buffer.is_segment_loaded(segment_index)
        };

        if needs_loading {
            // Only seek if not already at the right position
            let current_pos = source.current_position();

            // Debug log to help diagnose test failures
            tracing::debug!(
                "Segment position: {}, Current position: {}",
                segment_position,
                current_pos
            );

            if current_pos != segment_position {
                if let Err(e) = source.seek(segment_position) {
                    tracing::error!("Error seeking: {}", e);
                    break;
                }
            }

            match source.decode_next_frame() {
                Ok(segments) => {
                    if segments.is_empty() {
                        // EOF reached
                        break;
                    }

                    // Add to buffer
                    let mut buffer = buffer.write();
                    buffer.add_segments(segments);
                    loaded_count += 1;
                }
                Err(e) => {
                    tracing::error!("Error decoding: {}", e);
                    break;
                }
            }
        } else {
            // Segment already loaded in buffer
            loaded_count += 1;
        }
    }

    loaded_count
}
async fn buffer_management_task<S: Source + Send + Sync + 'static>(
    source: Arc<S>,
    buffer: Arc<RwLock<SegmentedBuffer>>,
    mut command_rx: mpsc::Receiver<TrackCommand>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) {
    let mut current_position = 0;
    let mut is_ready = false;

    while let Some(command) = command_rx.recv().await {
        match command {
            TrackCommand::FillFrom(position) => {
                current_position = position;

                // Set not ready when starting to fill from a new position
                if is_ready {
                    is_ready = false;
                    let _ = ready_tx.send(false);
                }

                // Load initial segments (minimum needed for playback)
                const INITIAL_SEGMENTS: usize = 3;
                let loaded_count =
                    load_segments_with(&source, &buffer, current_position, INITIAL_SEGMENTS).await;

                // Signal ready if we loaded enough segments
                if loaded_count > 0 && !is_ready {
                    is_ready = true;
                    let _ = ready_tx.send(true);
                    tracing::debug!("Track ready for playback at position {}", current_position);
                }

                // Continue loading more segments in background
                let source_clone = source.clone();
                let buffer_clone = buffer.clone();
                let preload_position = current_position + INITIAL_SEGMENTS * SEGMENT_SIZE;

                tokio::spawn(async move {
                    const ADDITIONAL_SEGMENTS: usize = 7; // Load segments 3-10
                    let _ = load_segments_with(
                        &source_clone,
                        &buffer_clone,
                        preload_position,
                        ADDITIONAL_SEGMENTS,
                    )
                    .await;
                });
            }
            TrackCommand::Shutdown => {
                tracing::info!("Buffer management task received shutdown command");
                break;
            }
        }
    }

    tracing::info!("Buffer management task completed");
}

impl<S: Source + Send + Sync> Track<S> {
    pub async fn new(source: S) -> Result<Self, PlaybackError> {
        let source = Arc::new(source);
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));
        let (command_tx, command_rx) = mpsc::channel(32);

        // Create a ringbuffer for audio samples (2 seconds of stereo audio at 48kHz)
        let rb_capacity = 2 * 48000 * 2;
        let sample_buffer = Arc::new(RwLock::new(HeapRb::<f32>::new(rb_capacity)));

        let source_clone = Arc::clone(&source);
        let buffer_clone = Arc::clone(&buffer);

        // Add ready state tracking
        let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);

        // Pass ready_tx to the background task
        let ready_tx_clone = ready_tx.clone();

        let task_handle = tokio::spawn(async move {
            buffer_management_task(source_clone, buffer_clone, command_rx, ready_tx_clone).await;
        });

        let track = Self {
            source,
            position: 0,
            playing: false,
            volume: 1.0,
            buffer,
            command_tx,
            task_handle: Some(task_handle),
            sample_buffer,
            ready_tx,
            ready_rx,
        };

        // Send initial fill command
        track
            .command_tx
            .send(TrackCommand::FillFrom(0))
            .await
            .expect("failed to send track command");

        Ok(track)
    }

    pub async fn ensure_ready(&self) -> Result<(), PlaybackError> {
        // Get a clone of the receiver
        let mut rx = self.ready_rx.clone();

        // If already ready, return immediately
        if *rx.borrow() {
            return Ok(());
        }

        // Otherwise wait for the ready signal
        loop {
            // Wait for a change in the ready state
            rx.changed()
                .await
                .map_err(|_| PlaybackError::TaskCancelled)?;

            // Check if we're ready now
            if *rx.borrow() {
                return Ok(());
            }
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        // Update position
        self.position = position;

        // Request buffer filling from new position
        if let Err(e) = self.command_tx.try_send(TrackCommand::FillFrom(position)) {
            tracing::error!("Failed to send fill command after seek: {}", e);
        }

        Ok(())
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn is_ready(&self) -> bool {
        *self.ready_rx.borrow()
    }

    pub fn get_next_samples(&mut self, output: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            return Ok(0);
        }

        // Read from the segmented buffer
        let samples_read = {
            let buffer = self.buffer.read();
            buffer.get_samples(self.position, output)
        };

        // Update position
        if samples_read > 0 {
            self.position += samples_read;
        }

        // If we couldn't read enough samples, request more
        if samples_read < output.len() {
            // Try to load more data starting from current position
            if let Err(e) = self
                .command_tx
                .try_send(TrackCommand::FillFrom(self.position))
            {
                tracing::debug!("Failed to send fill command: {}", e);
            }
        }

        Ok(samples_read)
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

    // Create a test source with multiple frames (for more complex testing)
    pub fn new_with_frames(frames: Vec<Vec<f32>>) -> Self {
        let frames_of_segments = frames
            .into_iter()
            .map(Self::create_segments_from_samples)
            .collect();

        Self {
            position: AtomicUsize::new(0),
            samples: frames_of_segments,
            current_sample_position: AtomicUsize::new(0),
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SegmentIndex;
    #[tokio::test]
    async fn test_track_play_stop() {
        // Create a test source
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Create a track with this source
        let mut track = Track::new(source).await.unwrap();

        // Track should not be playing initially
        assert!(!track.is_playing(), "Track should not be playing initially");

        // Play the track
        track.play();

        // Track should now be playing
        assert!(
            track.is_playing(),
            "Track should be playing after play() call"
        );

        // Stop the track
        track.stop();

        // Track should no longer be playing
        assert!(
            !track.is_playing(),
            "Track should not be playing after stop() call"
        );
    }

    #[tokio::test]
    async fn test_get_next_samples() {
        // Create a test source with a known pattern
        let source = TestSource::new_with_pattern("alternating", 1.0);

        // Create a track with this source
        let mut track = Track::new(source).await.unwrap();

        // Wait for the track to be ready for playback
        track.ensure_ready().await.unwrap();

        // Set track to playing state
        track.play();

        // Create a buffer to hold samples
        let mut output_buffer = vec![0.0; 100];

        // Read the first batch of samples
        let samples_read = track.get_next_samples(&mut output_buffer).unwrap();

        // Should have read samples
        assert!(samples_read > 0, "Should have read samples");
        assert_eq!(
            samples_read,
            output_buffer.len(),
            "Should have filled the buffer"
        );

        // Check that the samples follow the alternating pattern
        // (high, zero, low) for the first few samples
        assert!(output_buffer[0] > 0.5, "First sample should be high");
        assert!(
            output_buffer[1].abs() < 0.01,
            "Second sample should be near zero"
        );
        assert!(output_buffer[2] < -0.5, "Third sample should be low");

        // Track's position should have advanced
        assert_eq!(
            track.position(),
            samples_read,
            "Position should have advanced"
        );

        // Read another batch of samples
        let previous_position = track.position();
        let samples_read_2 = track.get_next_samples(&mut output_buffer).unwrap();

        // Should have read more samples
        assert!(samples_read_2 > 0, "Should have read more samples");

        // Position should have advanced again
        assert_eq!(
            track.position(),
            previous_position + samples_read_2,
            "Position should have advanced again"
        );

        // When stopped, should not read samples
        track.stop();
        let samples_read_3 = track.get_next_samples(&mut output_buffer).unwrap();
        assert_eq!(samples_read_3, 0, "Should not read samples when stopped");
    }

    #[tokio::test]
    async fn test_load_segment() {
        let source = Arc::new(TestSource::new_with_pattern("alternating", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));
        // Initially segment 0 should not be loaded
        {
            let buffer = buffer.read();
            assert!(!buffer.is_segment_loaded(SegmentIndex(0)));
        }

        // Load segment 0
        load_segments_with(&source, &buffer, 0, 1).await;

        // Now segment 0 should be loaded
        {
            let buffer = buffer.read();
            assert!(buffer.is_segment_loaded(SegmentIndex(0)));
        }
    }

    #[tokio::test]
    async fn test_load_segment_wrong_position() {
        // Create a test source
        let source = Arc::new(TestSource::new_with_pattern("ascending", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load segment at position 2 (which requires seeking)
        load_segments_with(&source, &buffer, 2, 1).await;

        // Check that segment 2 is loaded
        {
            let buffer = buffer.read();
            assert!(buffer.is_segment_loaded(SegmentIndex(2)));
        }
    }
    #[tokio::test]
    async fn test_load_segment_at_eof() {
        // Create a test source that will reach EOF quickly
        let source = Arc::new(TestSource::new_with_pattern("ascending", 0.1));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // First, force the source to EOF by reading all its data
        loop {
            let segments = source.decode_next_frame().unwrap();
            if segments.is_empty() {
                // Empty segments indicate EOF
                break;
            }
        }

        // Seeking should reset EOF state
        let target_position = 0;
        source.seek(target_position).unwrap();

        // Now try to load a segment - should work even after previous EOF
        load_segments_with(&source, &buffer, 0, 1).await;

        // Check that segment 0 is loaded
        {
            let buffer = buffer.read();
            assert!(
                buffer.is_segment_loaded(SegmentIndex(0)),
                "Segment should be loaded after seeking from EOF"
            );
        }
    }

    #[tokio::test]
    async fn test_load_already_loaded_segment() {
        // Create a test source
        let source = Arc::new(TestSource::new_with_pattern("ascending", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load segment at position 0
        load_segments_with(&source, &buffer, 0, 1).await;

        // Check that segment 0 is loaded
        {
            let buffer = buffer.read();
            assert!(buffer.is_segment_loaded(SegmentIndex(0)));
        }

        // Now try to load the same segment again
        // This should be efficient (not require seeking or decoding again)
        // We don't have a direct way to test this efficiency in the current structure,
        // but we can at least ensure it doesn't fail
        load_segments_with(&source, &buffer, 0, 1).await;

        // Segment should still be loaded
        {
            let buffer = buffer.read();
            assert!(buffer.is_segment_loaded(SegmentIndex(0)));
        }
    }

    #[tokio::test]
    async fn test_load_multiple_segments() {
        // Create a test source
        let source = Arc::new(TestSource::new_with_pattern("ascending", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load multiple segments in sequence
        for i in 0..3 {
            let target_segment = SegmentIndex(i);
            load_segments_with(&source, &buffer, i, 1).await;
        }

        // Check that all segments are loaded
        {
            let buffer = buffer.read();
            for i in 0..3 {
                assert!(
                    buffer.is_segment_loaded(SegmentIndex(i)),
                    "Segment {} should be loaded",
                    i
                );
            }
        }

        // Load a non-sequential segment
        load_segments_with(&source, &buffer, 5, 1).await;

        // Check that the new segment is loaded
        {
            let buffer = buffer.read();
            assert!(buffer.is_segment_loaded(SegmentIndex(5)));
        }
    }

    #[tokio::test]
    async fn test_load_segments_with() {
        let source = Arc::new(TestSource::new_with_pattern("alternating", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load segments
        let position = 0;
        let count = 3;
        let loaded_count = load_segments_with(&source, &buffer, position, count).await;

        // Verify segments were loaded
        assert_eq!(loaded_count, count);

        // Check that segments are in the buffer
        for offset in 0..count {
            let segment_index = SegmentIndex(offset);
            let buffer_read = buffer.read();
            assert!(buffer_read.is_segment_loaded(segment_index));
        }
    }

    #[tokio::test]
    async fn test_segment_content_verification() {
        // Create a test source with the alternating pattern
        // The alternating pattern generates: [0.9, 0.0, -0.9, 0.9, 0.0, -0.9, ...]
        let source = Arc::new(TestSource::new_with_pattern("alternating", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load a segment
        let loaded = load_segments_with(&source, &buffer, 0, 1).await;
        assert_eq!(loaded, 1, "Should successfully load one segment");

        // Create a buffer to read samples into
        let mut output = vec![0.0; 12]; // Read 4 complete cycles

        // Read samples from the buffer
        let samples_read = buffer.read().get_samples(0, &mut output);

        // Verify we got all requested samples
        assert_eq!(samples_read, 12, "Should read all requested samples");

        // Define expected values for alternating pattern
        // Pattern repeats: [0.9, 0.0, -0.9, 0.9, 0.0, -0.9, ...]
        let expected_values = [
            0.9, 0.0, -0.9, 0.9, 0.0, -0.9, 0.9, 0.0, -0.9, 0.9, 0.0, -0.9,
        ];

        // Verify each sample exactly matches the expected value
        // Use a small epsilon for floating point comparison
        const EPSILON: f32 = 1e-6;

        for i in 0..expected_values.len() {
            assert!(
                (output[i] - expected_values[i]).abs() < EPSILON,
                "Sample at position {} should be {}, got {}",
                i,
                expected_values[i],
                output[i]
            );
        }
    }

    #[tokio::test]
    async fn test_segment_content_ascending() {
        // Create a test source with the ascending pattern
        // The ascending pattern generates a ramp from -0.9 to 0.9
        let source = Arc::new(TestSource::new_with_pattern("ascending", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load a segment
        let loaded = load_segments_with(&source, &buffer, 0, 1).await;
        assert_eq!(loaded, 1, "Should successfully load one segment");

        // Create a buffer to read samples into
        let mut output = vec![0.0; 5]; // Sample a few points along the ramp

        // Read samples from the buffer
        let samples_read = buffer.read().get_samples(0, &mut output);
        assert_eq!(samples_read, 5, "Should read all requested samples");

        // With ascending pattern, each sample should be greater than the previous
        for i in 1..output.len() {
            assert!(
                output[i] > output[i - 1],
                "Sample at position {} ({}) should be greater than position {} ({})",
                i,
                output[i],
                i - 1,
                output[i - 1]
            );
        }

        // First sample should be near -0.9 and last sample should be higher
        const EPSILON: f32 = 0.01;
        assert!(
            (output[0] + 0.9).abs() < EPSILON,
            "First sample should be near -0.9, got {}",
            output[0]
        );
    }
    #[tokio::test]
    async fn test_segment_boundary_content() {
        // Create a test source with the alternating pattern
        let source = Arc::new(TestSource::new_with_pattern("alternating", 1.0));
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

        // Load two consecutive segments
        let loaded = load_segments_with(&source, &buffer, 0, 2).await;
        assert_eq!(loaded, 2, "Should successfully load two segments");

        // Create a buffer to read samples across segment boundary
        // Assuming SEGMENT_SIZE is 1024, read 10 samples before and after boundary
        let boundary_position = SEGMENT_SIZE - 5;
        let mut output = vec![0.0; 10];

        // Read samples spanning the boundary
        let samples_read = buffer.read().get_samples(boundary_position, &mut output);
        assert_eq!(samples_read, 10, "Should read all requested samples");

        // Test pattern should continue seamlessly across boundary
        // For alternating pattern, we should see the continuing [0.9, 0.0, -0.9] pattern

        // Define expected values based on where in the pattern we expect to be
        // This needs to be calculated based on the boundary_position
        let pattern_position = boundary_position % 3; // Since pattern repeats every 3 samples

        let mut expected_values = Vec::with_capacity(10);
        for i in 0..10 {
            let pattern_index = (pattern_position + i) % 3;
            let value = match pattern_index {
                0 => 0.9,
                1 => 0.0,
                2 => -0.9,
                _ => unreachable!(),
            };
            expected_values.push(value);
        }

        // Verify values across boundary
        const EPSILON: f32 = 1e-6;
        for i in 0..10 {
            assert!(
                (output[i] - expected_values[i]).abs() < EPSILON,
                "Sample at boundary+{} should be {}, got {}",
                i - 5,
                expected_values[i],
                output[i]
            );
        }
    }
}

#[cfg(test)]
mod ringbuffer_tests {
    use ringbuf::HeapRb;

    #[test]
    fn test_basic_ringbuffer_usage() {
        // Create a heap-allocated ringbuffer with capacity for 10 samples
        let rb = HeapRb::<f32>::new(10);
        let (mut producer, mut consumer) = rb.split();

        // Push some samples
        for i in 0..5 {
            assert!(producer.push(i as f32).is_ok());
        }

        // Should have 5 items
        assert_eq!(consumer.len(), 5);

        // Read some samples
        let mut output = vec![0.0; 3];
        let read = consumer.pop_slice(&mut output);
        assert_eq!(read, 3);
        assert_eq!(output, vec![0.0, 1.0, 2.0]);

        // Should have 2 items left
        assert_eq!(consumer.len(), 2);
    }
}
#[cfg(test)]
struct SeekCountingSource {
    inner: TestSource,
    seek_count: AtomicUsize,
}

#[cfg(test)]
impl SeekCountingSource {
    fn new() -> Self {
        Self {
            inner: TestSource::new_with_pattern("alternating", 1.0),
            seek_count: AtomicUsize::new(0),
        }
    }

    fn get_seek_count(&self) -> usize {
        self.seek_count.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
impl Source for SeekCountingSource {
    fn decode_next_frame(&self) -> Result<Vec<DecodedSegment>, PlaybackError> {
        self.inner.decode_next_frame()
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // Count the seek first
        self.seek_count.fetch_add(1, Ordering::SeqCst);
        // Then delegate to inner source
        self.inner.seek(position)
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn audio_channels(&self) -> u16 {
        self.inner.audio_channels()
    }

    fn current_position(&self) -> usize {
        self.inner.current_position()
    }
}

#[tokio::test]
async fn test_seek_optimization() {
    // Create a source with explicit position tracking
    struct TestSeekSource {
        position: AtomicUsize,
        seek_count: AtomicUsize,
    }

    impl TestSeekSource {
        fn new(initial_position: usize) -> Self {
            Self {
                position: AtomicUsize::new(initial_position),
                seek_count: AtomicUsize::new(0),
            }
        }

        fn get_seek_count(&self) -> usize {
            self.seek_count.load(Ordering::SeqCst)
        }
    }

    impl Source for TestSeekSource {
        fn decode_next_frame(&self) -> Result<Vec<DecodedSegment>, PlaybackError> {
            // Generate a single segment at current position
            let current_pos = self.position.load(Ordering::SeqCst);
            let segment_index = SegmentIndex::from_sample_position(current_pos);

            // Create sample data
            let mut samples = [0.0; SEGMENT_SIZE];
            (0..SEGMENT_SIZE).for_each(|i| {
                samples[i] = 0.5; // Simple constant value
            });

            // Update position to after this segment
            self.position
                .store(current_pos + SEGMENT_SIZE, Ordering::SeqCst);

            Ok(vec![DecodedSegment {
                index: segment_index,
                segment: AudioSegment { samples },
            }])
        }

        fn seek(&self, position: usize) -> Result<(), PlaybackError> {
            // Record the seek and update position
            self.seek_count.fetch_add(1, Ordering::SeqCst);
            self.position.store(position, Ordering::SeqCst);
            Ok(())
        }

        fn sample_rate(&self) -> u32 {
            48000
        }
        fn audio_channels(&self) -> u16 {
            2
        }

        fn current_position(&self) -> usize {
            self.position.load(Ordering::SeqCst)
        }
    }

    // Create source starting at position 0
    let source = Arc::new(TestSeekSource::new(0));
    let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));

    // Verify initial state
    assert_eq!(source.current_position(), 0, "Initial position should be 0");
    assert_eq!(source.get_seek_count(), 0, "Initial seek count should be 0");

    // Load segment at position 0 - this should NOT require a seek (already at position 0)
    load_segments_with(&source, &buffer, 0, 1).await;

    // Verify no seek happened
    assert_eq!(
        source.get_seek_count(),
        0,
        "First segment should not require a seek"
    );

    // Position should now be after the first segment
    assert_eq!(
        source.current_position(),
        SEGMENT_SIZE,
        "Position should be updated"
    );

    // Load a segment at a different position - should require a seek
    load_segments_with(&source, &buffer, SEGMENT_SIZE * 2, 1).await;
    assert_eq!(
        source.get_seek_count(),
        1,
        "Loading different segment should require a seek"
    );

    // Source is now at position 3*SEGMENT_SIZE

    // Load the same segment again - should NOT require a seek (already buffered)
    source.seek_count.store(0, Ordering::SeqCst); // Reset counter
    load_segments_with(&source, &buffer, SEGMENT_SIZE * 2, 1).await;
    assert_eq!(
        source.get_seek_count(),
        0,
        "Loading already buffered segment should not seek"
    );
}
