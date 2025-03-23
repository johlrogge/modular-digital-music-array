use crate::buffer::SegmentedBuffer;
use crate::error::PlaybackError;
#[cfg(test)]
use crate::source::{AudioSegment, SegmentIndex, SEGMENT_SIZE};
use crate::source::{DecodedSegment, Source};

use tokio::sync::mpsc;

use parking_lot::RwLock;
use std::sync::atomic::Ordering;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize},
    Arc,
};

// In src/track.rs
use ringbuf::{HeapProducer, HeapRb};

pub struct Track {
    position: Arc<AtomicUsize>,
    playing: Arc<AtomicBool>,
    buffer: Arc<RwLock<SegmentedBuffer>>,
    command_tx: mpsc::Sender<TrackCommand>,
    decoder_task: Option<tokio::task::JoinHandle<()>>,
    producer_task: Option<tokio::task::JoinHandle<()>>,
    ready_tx: tokio::sync::watch::Sender<bool>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
}
// Update TrackCommand to include potential new commands
pub enum TrackCommand {
    FillFrom(usize),
    Shutdown,
}

#[cfg(test)]
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

async fn producer_task(
    buffer: Arc<RwLock<SegmentedBuffer>>,
    mut producer: HeapProducer<f32>,
    position: Arc<AtomicUsize>,
    command_tx: mpsc::Sender<TrackCommand>,
) {
    let mut temp_buffer = vec![0.0; 1024]; // Temporary buffer for samples

    tracing::info!("Producer task started"); // Add logging

    loop {
        // Get current position
        let current_position = position.load(Ordering::Relaxed);

        // Fill as much of the ringbuffer as possible
        let available_space = producer.free_len();

        if available_space > 0 {
            // Read samples from buffer
            let to_read = std::cmp::min(available_space, temp_buffer.len());
            let read = {
                let buffer = buffer.read();
                buffer.get_samples(current_position, &mut temp_buffer[..to_read])
            };

            if read > 0 {
                // Write to ringbuffer
                for i in 0..read {
                    let _ = producer.push(temp_buffer[i]);
                }

                // Update position
                position.fetch_add(read, Ordering::Relaxed);

                tracing::debug!("Pushed {} samples to ringbuffer", read);
            } else {
                // No samples available, request more
                tracing::debug!(
                    "No samples at position {}, requesting more",
                    current_position
                );

                if let Err(e) = command_tx.try_send(TrackCommand::FillFrom(current_position)) {
                    tracing::debug!("Failed to send fill command: {}", e);
                }

                // Sleep a bit to avoid spinning
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        } else {
            // Ringbuffer is full, wait a bit
            tracing::debug!("Ringbuffer full, waiting");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}
async fn buffer_task(
    buffer: Arc<RwLock<SegmentedBuffer>>,
    mut segments_rx: mpsc::Receiver<Vec<DecodedSegment>>,
) {
    while let Some(segments) = segments_rx.recv().await {
        let mut buffer = buffer.write();
        buffer.add_segments(segments);
    }

    tracing::info!("Buffer task completed");
}

async fn decoder_task<S: Source + Send + Sync + 'static>(
    source: S,
    segments_tx: mpsc::Sender<Vec<DecodedSegment>>,
    mut command_rx: mpsc::Receiver<TrackCommand>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) {
    let mut current_position;
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

                // Seek to the requested position
                if let Err(e) = source.seek(current_position) {
                    tracing::error!("Error seeking: {}", e);
                    continue;
                }

                // Load initial segments
                const INITIAL_SEGMENTS: usize = 3;
                let mut segments_loaded = 0;

                for _ in 0..INITIAL_SEGMENTS {
                    match source.decode_next_frame() {
                        Ok(segments) => {
                            if segments.is_empty() {
                                // EOF reached
                                break;
                            }

                            // Send segments to Track
                            if let Err(e) = segments_tx.send(segments).await {
                                tracing::error!("Error sending segments: {}", e);
                                break;
                            }

                            segments_loaded += 1;
                        }
                        Err(e) => {
                            tracing::error!("Error decoding: {}", e);
                            break;
                        }
                    }
                }

                // Signal ready if we loaded enough segments
                if segments_loaded > 0 && !is_ready {
                    is_ready = true;
                    let _ = ready_tx.send(true);
                    tracing::debug!("Track ready for playback at position {}", current_position);
                }

                // Continue loading more segments in background
                const ADDITIONAL_SEGMENTS: usize = 7;
                for _ in 0..ADDITIONAL_SEGMENTS {
                    match source.decode_next_frame() {
                        Ok(segments) => {
                            if segments.is_empty() {
                                break; // EOF
                            }

                            if let Err(e) = segments_tx.send(segments).await {
                                tracing::error!("Error sending segments: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error decoding: {}", e);
                            break;
                        }
                    }
                }
            }
            TrackCommand::Shutdown => {
                tracing::info!("Decoder task received shutdown command");
                break;
            }
        }
    }

    tracing::info!("Decoder task completed");
}
impl Track {
    pub async fn new<S: Source + Send + Sync + 'static>(
        source: S,
        output_producer: HeapProducer<f32>,
    ) -> Result<Self, PlaybackError> {
        let buffer = Arc::new(RwLock::new(SegmentedBuffer::new()));
        let position = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(false));

        // Command channels
        let (command_tx, command_rx) = mpsc::channel(32);
        let (segments_tx, segments_rx) = mpsc::channel(100);
        let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);

        // Create decoder task
        let ready_tx_clone = ready_tx.clone();
        let decoder_task = tokio::spawn(async move {
            decoder_task(source, segments_tx, command_rx, ready_tx_clone).await;
        });

        // Create buffer task
        let buffer_clone = Arc::clone(&buffer);
        tokio::spawn(async move {
            buffer_task(buffer_clone, segments_rx).await;
        });

        // Create producer task - this now owns output_producer
        let position_clone = Arc::clone(&position);
        let buffer_clone = Arc::clone(&buffer);
        let command_tx_clone = command_tx.clone();
        let producer_task = tokio::spawn(async move {
            producer_task(
                buffer_clone,
                output_producer,
                position_clone,
                command_tx_clone,
            )
            .await;
        });

        let track = Self {
            position,
            playing,
            buffer,
            command_tx,
            decoder_task: Some(decoder_task),
            producer_task: Some(producer_task),
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

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        // Update position
        self.position.store(position, Ordering::Relaxed);

        // Request buffer filling from new position
        if let Err(e) = self.command_tx.try_send(TrackCommand::FillFrom(position)) {
            tracing::error!("Failed to send fill command after seek: {}", e);
        }

        Ok(())
    }

    pub fn position(&self) -> usize {
        self.position.load(Ordering::Relaxed)
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
        while !self.buffer.read().is_ready_at(0) {
            // Wait a bit to ensure buffer management task processes the data
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        Ok(())
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
