pub const SEGMENT_SIZE: usize = 1024;

// Identifies a segment's position in the stream
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentIndex(pub usize);

impl SegmentIndex {
    // Convert a sample position to a segment index
    pub fn from_sample_position(position: usize) -> Self {
        let index = position / SEGMENT_SIZE;
        Self(index)
    }

    // Get the sample position at the start of this segment
    pub fn start_position(&self) -> usize {
        self.0 * SEGMENT_SIZE
    }

    // Get the next segment index
    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }
}

// An audio segment with exactly SEGMENT_SIZE samples
// Last segment is zero-padded if needed
#[derive(Clone, Debug)]
pub struct AudioSegment {
    pub samples: [f32; SEGMENT_SIZE],
}

// A decoded segment with its position information
#[derive(Debug, Clone)]
pub struct DecodedSegment {
    // The segment index
    pub index: SegmentIndex,

    // The segment data
    pub segment: AudioSegment,

    // How many samples in `segment.samples` are real audio (not zero-padding).
    // Callers must only read `segment.samples[..valid_samples]`.
    pub valid_samples: usize,
}

impl DecodedSegment {
    pub fn is_empty(&self) -> bool {
        self.segment.samples.iter().all(|s| *s == 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_index_from_sample_position_zero() {
        let idx = SegmentIndex::from_sample_position(0);
        assert_eq!(idx.0, 0);
    }

    #[test]
    fn segment_index_from_sample_position_within_first_segment() {
        let idx = SegmentIndex::from_sample_position(512);
        assert_eq!(idx.0, 0);
    }

    #[test]
    fn segment_index_from_sample_position_at_segment_boundary() {
        let idx = SegmentIndex::from_sample_position(SEGMENT_SIZE);
        assert_eq!(idx.0, 1);
    }

    #[test]
    fn segment_index_start_position() {
        let idx = SegmentIndex(3);
        assert_eq!(idx.start_position(), 3 * SEGMENT_SIZE);
    }

    #[test]
    fn segment_index_next() {
        let idx = SegmentIndex(5);
        assert_eq!(idx.next(), SegmentIndex(6));
    }

    #[test]
    fn decoded_segment_is_empty_when_all_zeros() {
        let segment = DecodedSegment {
            index: SegmentIndex(0),
            segment: AudioSegment {
                samples: [0.0; SEGMENT_SIZE],
            },
            valid_samples: SEGMENT_SIZE,
        };
        assert!(segment.is_empty());
    }

    #[test]
    fn decoded_segment_is_not_empty_when_has_nonzero_sample() {
        let mut samples = [0.0f32; SEGMENT_SIZE];
        samples[42] = 1.0;
        let segment = DecodedSegment {
            index: SegmentIndex(0),
            segment: AudioSegment { samples },
            valid_samples: SEGMENT_SIZE,
        };
        assert!(!segment.is_empty());
    }
}
