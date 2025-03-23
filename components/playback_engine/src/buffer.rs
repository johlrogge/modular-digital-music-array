// src/buffer.rs
use crate::source::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};

pub struct SegmentedBuffer {
    // Vec of optional AudioSegments, stored in order
    segments: Vec<Option<AudioSegment>>,
    // Index of the first segment in the buffer (head)
    head_index: SegmentIndex,
    // Index where the next segment should be added (tail)
    tail_index: SegmentIndex,
    // Maximum number of segments to keep
    capacity: usize,
}

impl SegmentedBuffer {
    pub fn new() -> Self {
        let capacity = 500;

        Self {
            segments: Vec::with_capacity(capacity),
            head_index: SegmentIndex(0),
            tail_index: SegmentIndex(0),
            capacity,
        }
    }

    pub fn add_segments(&mut self, segments: Vec<DecodedSegment>) {
        for segment in segments {
            self.add_segment(segment);
        }
    }

    pub fn add_segment(&mut self, segment: DecodedSegment) {
        if self.segments.is_empty() {
            // First segment, initialize buffer
            self.head_index = segment.index;
            self.tail_index = SegmentIndex(segment.index.0 + 1);
            self.segments.push(Some(segment.segment));
            return;
        }

        // Calculate where this segment should go relative to head
        let relative_index = segment.index.0 as isize - self.head_index.0 as isize;

        if relative_index < 0 {
            // This segment comes before our head
            if -relative_index as usize > self.capacity / 2 {
                // If it's too far back, reset the buffer
                self.segments.clear();
                self.head_index = segment.index;
                self.tail_index = SegmentIndex(segment.index.0 + 1);
                self.segments.push(Some(segment.segment));
            } else {
                // Prepend with Nones and then add our segment
                let missing_segments = -relative_index as usize;
                let mut new_segments = vec![None; missing_segments];
                new_segments[0] = Some(segment.segment);
                new_segments.append(&mut self.segments);
                self.segments = new_segments;
                self.head_index = segment.index;

                // Trim if we've exceeded capacity
                if self.segments.len() > self.capacity {
                    self.segments.truncate(self.capacity);
                    self.tail_index = SegmentIndex(self.head_index.0 + self.segments.len());
                }
            }
        } else if relative_index as usize >= self.segments.len() {
            // This segment comes after our current tail
            let current_size = self.segments.len();
            let target_position = relative_index as usize;

            if target_position - current_size > self.capacity / 2 {
                // If it's too far ahead, reset the buffer
                self.segments.clear();
                self.head_index = segment.index;
                self.tail_index = SegmentIndex(segment.index.0 + 1);
                self.segments.push(Some(segment.segment));
            } else {
                // Extend with Nones and then add our segment
                let new_size = target_position + 1;
                self.segments.resize_with(new_size, || None);
                self.segments[target_position] = Some(segment.segment);
                self.tail_index = SegmentIndex(self.head_index.0 + new_size);

                // Trim from the beginning if we've exceeded capacity
                if self.segments.len() > self.capacity {
                    let excess = self.segments.len() - self.capacity;
                    self.segments.drain(0..excess);
                    self.head_index = SegmentIndex(self.head_index.0 + excess);
                }
            }
        } else {
            // This segment is within our current range, just replace it
            self.segments[relative_index as usize] = Some(segment.segment);
        }
    }

    pub fn get_samples(&self, position: usize, output: &mut [f32]) -> usize {
        if self.segments.is_empty() {
            return 0;
        }

        let start_segment_index = SegmentIndex::from_sample_position(position);
        let offset_in_segment = position - start_segment_index.start_position();

        // Calculate relative position to our head
        let relative_index = start_segment_index.0 as isize - self.head_index.0 as isize;

        if relative_index < 0 || relative_index >= self.segments.len() as isize {
            // Position is outside our buffered range
            return 0;
        }

        let mut samples_written = 0;
        let mut current_segment_idx = relative_index as usize;

        while samples_written < output.len() && current_segment_idx < self.segments.len() {
            if let Some(segment) = &self.segments[current_segment_idx] {
                // Calculate offset within segment
                let segment_offset = if current_segment_idx == relative_index as usize {
                    offset_in_segment
                } else {
                    0
                };

                // Calculate samples to copy
                let samples_available = SEGMENT_SIZE - segment_offset;
                let samples_to_copy =
                    std::cmp::min(samples_available, output.len() - samples_written);

                // Copy samples
                output[samples_written..samples_written + samples_to_copy].copy_from_slice(
                    &segment.samples[segment_offset..segment_offset + samples_to_copy],
                );

                samples_written += samples_to_copy;
            } else {
                // Found a gap (None), can't continue
                break;
            }

            current_segment_idx += 1;
        }

        samples_written
    }

    pub fn is_segment_loaded(&self, index: SegmentIndex) -> bool {
        if self.segments.is_empty() {
            return false;
        }

        let relative_index = index.0 as isize - self.head_index.0 as isize;
        if relative_index < 0 || relative_index >= self.segments.len() as isize {
            return false;
        }

        self.segments[relative_index as usize].is_some()
    }

    pub fn is_ready_at(&self, position: usize) -> bool {
        let segment_index = SegmentIndex::from_sample_position(position);
        self.is_segment_loaded(segment_index)
    }

    pub fn clear(&mut self) {
        self.segments.clear();
        // Reset indices when clearing
        self.head_index = SegmentIndex(0);
        self.tail_index = SegmentIndex(0);
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
