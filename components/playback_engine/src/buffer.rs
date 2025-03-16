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

#[cfg(test)]
mod segmented_buffer_tests {
    use super::*;

    #[test]
    fn test_get_samples_from_single_segment() {
        // Create a new buffer
        let mut buffer = SegmentedBuffer::new();

        // Create a segment with a simple pattern: ascending values
        let mut segment_samples = [0.0; SEGMENT_SIZE];
        (0..SEGMENT_SIZE).for_each(|i| {
            segment_samples[i] = i as f32 / 100.0;
        });

        // Add segment at position 0
        let segment_index = SegmentIndex(0);
        buffer.add_segment(DecodedSegment {
            index: segment_index,
            segment: AudioSegment {
                samples: segment_samples,
            },
        });

        // Try to read 100 samples from the beginning
        let mut output = vec![0.0; 100];
        let read = buffer.get_samples(0, &mut output);

        // Should read all requested samples
        assert_eq!(read, 100, "Should read all requested samples");

        // Verify the values match what we expect
        (0..100).for_each(|i| {
            assert_eq!(
                output[i],
                i as f32 / 100.0,
                "Sample at position {} should match",
                i
            );
        });
    }

    #[test]
    fn test_get_samples_spanning_segments() {
        // Create a new buffer
        let mut buffer = SegmentedBuffer::new();

        // Create two segments with sequential values
        let mut segment1_samples = [0.0; SEGMENT_SIZE];
        let mut segment2_samples = [0.0; SEGMENT_SIZE];

        for i in 0..SEGMENT_SIZE {
            segment1_samples[i] = i as f32;
            segment2_samples[i] = (SEGMENT_SIZE + i) as f32;
        }

        // Add both segments
        buffer.add_segment(DecodedSegment {
            index: SegmentIndex(0),
            segment: AudioSegment {
                samples: segment1_samples,
            },
        });

        buffer.add_segment(DecodedSegment {
            index: SegmentIndex(1),
            segment: AudioSegment {
                samples: segment2_samples,
            },
        });

        // Read from position near the end of first segment, spanning into second
        let start_pos = SEGMENT_SIZE - 50; // 50 samples before the segment boundary
        let mut output = vec![0.0; 100]; // Read 100 samples (50 from each segment)

        let read = buffer.get_samples(start_pos, &mut output);

        // Should read all requested samples
        assert_eq!(
            read, 100,
            "Should read all requested samples across segments"
        );

        // Verify first 50 samples (from first segment)
        (0..50).for_each(|i| {
            let expected = (start_pos + i) as f32;
            assert_eq!(
                output[i], expected,
                "Sample {} should be {} from first segment",
                i, expected
            );
        });

        // Verify next 50 samples (from second segment)
        for i in 0..50 {
            let expected = (SEGMENT_SIZE + i) as f32;
            assert_eq!(
                output[i + 50],
                expected,
                "Sample {} should be {} from second segment",
                i + 50,
                expected
            );
        }
    }

    #[test]
    fn test_get_samples_with_missing_segment() {
        // Create a new buffer
        let mut buffer = SegmentedBuffer::new();

        // Create a segment with all 1.0 values
        let segment_samples = [1.0; SEGMENT_SIZE];

        // Add segment at position 0 and position 2 (skipping position 1)
        buffer.add_segment(DecodedSegment {
            index: SegmentIndex(0),
            segment: AudioSegment {
                samples: segment_samples,
            },
        });

        buffer.add_segment(DecodedSegment {
            index: SegmentIndex(2),
            segment: AudioSegment {
                samples: segment_samples,
            },
        });

        // Try to read from position in segment 0 (should work)
        let mut output = vec![0.0; 100];
        let read = buffer.get_samples(50, &mut output);
        assert_eq!(read, 100, "Should read from available segment");

        // Try to read from position in missing segment 1
        output.fill(0.0); // Reset output buffer
        let read = buffer.get_samples(SEGMENT_SIZE + 50, &mut output);
        assert_eq!(read, 0, "Should read 0 samples when segment is missing");

        // Try to read from position in segment 2 (should work)
        output.fill(0.0); // Reset output buffer
        let read = buffer.get_samples(2 * SEGMENT_SIZE + 50, &mut output);
        assert_eq!(read, 100, "Should read from available segment");
    }

    #[test]
    fn test_get_samples_partial_buffer() {
        // Create a new buffer
        let mut buffer = SegmentedBuffer::new();

        // Create a segment
        let mut segment_samples = [0.0; SEGMENT_SIZE];
        (0..SEGMENT_SIZE).for_each(|i| {
            segment_samples[i] = i as f32;
        });

        // Add the segment
        buffer.add_segment(DecodedSegment {
            index: SegmentIndex(0),
            segment: AudioSegment {
                samples: segment_samples,
            },
        });

        // Try to read more samples than available
        let mut output = vec![0.0; SEGMENT_SIZE + 100];
        let read = buffer.get_samples(0, &mut output);

        // Should only read available samples
        assert_eq!(read, SEGMENT_SIZE, "Should only read available samples");

        // Verify all samples were read correctly
        (0..SEGMENT_SIZE).for_each(|i| {
            assert_eq!(output[i], i as f32, "Sample at position {} should match", i);
        });
    }
}
