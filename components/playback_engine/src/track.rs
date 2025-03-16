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
        self.playing = true;
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
    async fn load_segment(&self, index: SegmentIndex) -> Result<(), PlaybackError> {
        // Check if segment already loaded
        {
            let buffer = self.buffer.read();
            if buffer.is_segment_loaded(index) {
                return Ok(());
            }
        }

        // Calculate position to seek to
        let seek_position = index.start_position();

        // Seek to the calculated position
        self.source.seek(seek_position)?;

        // Decode a frame of audio
        let segments = self.source.decode_next_frame()?;

        // Add the segments to the buffer
        let mut buffer = self.buffer.write();
        buffer.add_segments(segments);

        Ok(())
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
    // New field to track the target sample position for seeking
    seek_sample_position: AtomicUsize,
}

#[cfg(test)]
impl TestSource {
    pub fn new_from_samples(samples: Vec<f32>) -> Self {
        let segments = Self::create_segments_from_samples(samples);
        Self {
            position: AtomicUsize::new(0),
            samples: vec![segments], // Wrap in vector to simulate frames
            seek_sample_position: AtomicUsize::new(0),
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
            seek_sample_position: AtomicUsize::new(0),
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
        let seek_pos = self.seek_sample_position.load(Ordering::Relaxed);
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

        // Adjust segment indices based on seek position
        let adjusted_segments = self.adjust_segment_indices(segments);

        // Move to next frame
        self.position.store(pos + 1, Ordering::Relaxed);

        Ok(adjusted_segments)
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // Store the target sample position
        self.seek_sample_position.store(position, Ordering::Relaxed);

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
        // Create a test source that will produce predictable segments
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Create a track with this source
        let track = Track::new(source).await.unwrap();

        // Initially segment 0 should not be loaded
        {
            let buffer = track.buffer.read();
            assert!(!buffer.is_segment_loaded(SegmentIndex(0)));
        }

        // Load segment 0
        track.load_segment(SegmentIndex(0)).await.unwrap();

        // Now segment 0 should be loaded
        {
            let buffer = track.buffer.read();
            assert!(buffer.is_segment_loaded(SegmentIndex(0)));
        }
    }
    #[tokio::test]
    async fn test_load_segment_wrong_position() {
        // Create a test source
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Create a track with this source
        let track = Track::new(source).await.unwrap();

        // Load segment at position 2 (which requires seeking)
        let target_segment = SegmentIndex(2);
        track.load_segment(target_segment).await.unwrap();

        // Check that segment 2 is loaded
        {
            let buffer = track.buffer.read();
            assert!(buffer.is_segment_loaded(target_segment));
        }
    }
    #[tokio::test]
    async fn test_load_segment_at_eof() {
        // Create a test source that will reach EOF quickly
        let source = TestSource::new_with_pattern("ascending", 0.1); // Very short

        // Create a track with this source
        let track = Track::new(source).await.unwrap();

        // First, force the source to EOF by reading all its data
        loop {
            // We need to use decode_next_frame directly on the source
            let segments = track.source.decode_next_frame().unwrap();
            if segments.is_empty() {
                // Empty segments indicate EOF
                break;
            }
        }

        // Try to load a segment (should reset EOF and succeed)
        let target_segment = SegmentIndex(0);
        track.load_segment(target_segment).await.unwrap();

        // Check that segment 0 is loaded
        {
            let buffer = track.buffer.read();
            assert!(buffer.is_segment_loaded(target_segment));
        }
    }

    #[test]
    fn test_seek() {
        // Create a test source with an ascending pattern
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Verify initial state
        let initial_segments = source.decode_next_frame().unwrap();
        assert!(!initial_segments.is_empty(), "Should have initial segments");

        // Seek to a specific position (in TestSource, this is the frame index)
        let seek_position = 0; // Reset to first frame
        source.seek(seek_position).unwrap();

        // Decode again and verify we're back at the beginning
        let segments_after_seek = source.decode_next_frame().unwrap();
        assert!(
            !segments_after_seek.is_empty(),
            "Should have segments after seeking"
        );

        // The segments should be the same as our initial segments
        assert_eq!(
            initial_segments[0].index, segments_after_seek[0].index,
            "Segment indices should match after seeking to beginning"
        );
    }
    #[test]
    fn test_seek_sample_position() {
        // Create a test source with a pattern that has multiple segments
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Sample position that would be in the 3rd segment
        let sample_position = SEGMENT_SIZE * 2 + 100; // 2 full segments + 100 samples

        source
            .seek(sample_position)
            .expect("Should handle sample position seeks");

        // Decode a frame after seeking
        let segments = source
            .decode_next_frame()
            .expect("Should decode after seeking");

        // Should still get segments
        assert!(!segments.is_empty(), "Should get segments after seeking");

        // First segment should be at or close to the requested position
        let expected_segment_index = SegmentIndex::from_sample_position(sample_position);
        assert_eq!(
            segments[0].index, expected_segment_index,
            "Should get segment at the requested position"
        );
    }

    #[tokio::test]
    async fn test_load_already_loaded_segment() {
        // Create a test source
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Create a track with this source
        let track = Track::new(source).await.unwrap();

        // Load segment at position 0
        let target_segment = SegmentIndex(0);
        track.load_segment(target_segment).await.unwrap();

        // Check that segment 0 is loaded
        {
            let buffer = track.buffer.read();
            assert!(buffer.is_segment_loaded(target_segment));
        }

        // Now try to load the same segment again
        // This should be efficient (not require seeking or decoding again)
        // We don't have a direct way to test this efficiency in the current structure,
        // but we can at least ensure it doesn't fail
        track.load_segment(target_segment).await.unwrap();

        // Segment should still be loaded
        {
            let buffer = track.buffer.read();
            assert!(buffer.is_segment_loaded(target_segment));
        }
    }

    #[tokio::test]
    async fn test_load_multiple_segments() {
        // Create a test source
        let source = TestSource::new_with_pattern("ascending", 1.0);

        // Create a track with this source
        let track = Track::new(source).await.unwrap();

        // Load multiple segments in sequence
        for i in 0..3 {
            let target_segment = SegmentIndex(i);
            track.load_segment(target_segment).await.unwrap();
        }

        // Check that all segments are loaded
        {
            let buffer = track.buffer.read();
            for i in 0..3 {
                assert!(
                    buffer.is_segment_loaded(SegmentIndex(i)),
                    "Segment {} should be loaded",
                    i
                );
            }
        }

        // Load a non-sequential segment
        let target_segment = SegmentIndex(5);
        track.load_segment(target_segment).await.unwrap();

        // Check that the new segment is loaded
        {
            let buffer = track.buffer.read();
            assert!(buffer.is_segment_loaded(target_segment));
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
