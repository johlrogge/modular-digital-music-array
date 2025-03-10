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
        self.segments.insert(segment.index, segment.segment);
    }

    /// Add multiple segments to the buffer
    pub fn add_segments(&mut self, segments: Vec<DecodedSegment>) {
        for segment in segments {
            self.add_segment(segment);
        }
    }

    /// Get samples from the buffer starting at the given position
    /// Returns the number of samples read
    pub fn get_samples(&self, position: usize, output: &mut [f32]) -> usize {
        let mut samples_read = 0;
        let mut current_pos = position;

        while samples_read < output.len() {
            // Calculate which segment and offset within that segment
            let segment_index = SegmentIndex::from_sample_position(current_pos);
            let offset_in_segment = current_pos % SEGMENT_SIZE;

            // If we don't have this segment, we're done
            if !self.segments.contains_key(&segment_index) {
                break;
            }

            // Get the segment
            let segment = &self.segments[&segment_index];

            // Calculate how many samples we can copy
            let samples_available = SEGMENT_SIZE - offset_in_segment;
            let samples_needed = output.len() - samples_read;
            let samples_to_copy = std::cmp::min(samples_available, samples_needed);

            // Copy samples
            for i in 0..samples_to_copy {
                output[samples_read + i] = segment.samples[offset_in_segment + i];
            }

            // Update position and count
            current_pos += samples_to_copy;
            samples_read += samples_to_copy;
        }

        samples_read
    }

    /// Check if a segment is loaded
    pub fn is_segment_loaded(&self, index: SegmentIndex) -> bool {
        self.segments.contains_key(&index)
    }

    /// Check if buffer is ready for playback at position
    pub fn is_ready_at(&self, position: usize) -> bool {
        // Only need one segment loaded to start playback
        let current_segment = SegmentIndex::from_sample_position(position);
        self.is_segment_loaded(current_segment)
    }

    /// Clear all segments from the buffer
    pub fn clear(&mut self) {
        self.segments.clear();
    }
}
