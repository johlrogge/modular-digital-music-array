// src/buffer.rs
use crate::source::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};
use std::collections::HashMap;

/// A buffer that stores audio data in fixed-size segments
pub struct SegmentedBuffer {
    segments: HashMap<SegmentIndex, AudioSegment>,
}

impl SegmentedBuffer {
    /// Create a new empty buffer
    pub fn new() -> Self {
        Self {
            segments: HashMap::new(),
        }
    }

    /// Add a decoded segment to the buffer
    pub fn add_segment(&mut self, segment: DecodedSegment) {
        // Log to help diagnose what's happening
        tracing::debug!(
            "Adding segment {} at sample position {}",
            segment.index.0,
            segment.index.start_position()
        );
        self.segments.insert(segment.index, segment.segment);
    }

    /// Get samples from the buffer starting at the given position
    /// Returns the number of samples read
    pub fn get_samples(&self, position: usize, output: &mut [f32]) -> usize {
        todo!("implement get_samples")
    }

    /// Add multiple segments to the buffer
    pub fn add_segments(&mut self, segments: Vec<DecodedSegment>) {
        for segment in segments {
            self.add_segment(segment);
        }
    }

    /// Check if a segment is loaded
    pub fn is_segment_loaded(&self, index: SegmentIndex) -> bool {
        self.segments.contains_key(&index)
    }

    /// Check if buffer is ready for playback at position
    pub fn is_ready_at(&self, position: usize) -> bool {
        todo!("implement is ready at")
    }

    /// Clear all segments from the buffer
    pub fn clear(&mut self) {
        self.segments.clear();
    }
}
