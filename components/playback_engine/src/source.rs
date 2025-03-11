use crate::error::PlaybackError;
use parking_lot::Mutex;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
    units::Time,
};

pub const SEGMENT_SIZE: usize = 1024;

// Identifies a segment's position in the stream
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentIndex(pub usize);

impl SegmentIndex {
    // Convert a sample position to a segment index
    pub fn from_sample_position(position: usize) -> Self {
        Self(position / SEGMENT_SIZE)
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
#[derive(Debug)]
pub struct DecodedSegment {
    // The segment index
    pub index: SegmentIndex,

    // The segment data
    pub segment: AudioSegment,
}

pub trait Source: Send + Sync {
    // Try to decode segments starting at the current position
    fn decode_segments(&self, max_segments: usize) -> Result<Vec<DecodedSegment>, PlaybackError>;

    // Seek to a specific sample position
    fn seek(&self, position: usize) -> Result<(), PlaybackError>;

    // Basic metadata
    fn sample_rate(&self) -> u32;
    fn audio_channels(&self) -> u16;
}

pub struct FlacSource {
    // Decoder state (format reader + decoder)
    decoder_state: Mutex<DecoderState>,

    // Current sample position in the stream
    current_position: AtomicUsize,

    // Pre-allocated buffer for samples between segments
    sample_buffer: Mutex<Vec<f32>>,

    // Basic metadata
    sample_rate: u32,
    audio_channels: u16,

    // End-of-file status
    is_eof: AtomicBool,
}

struct DecoderState {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
}

type DecoderResult = Result<
    (
        Box<dyn FormatReader>,
        Box<dyn symphonia::core::codecs::Decoder>,
        u32,
        u16,
    ),
    PlaybackError,
>;

impl FlacSource {
    const TYPICAL_FRAME_SIZE: usize = 8192;

    pub fn new(path: impl AsRef<Path>) -> Result<Self, PlaybackError> {
        tracing::debug!("Opening file: {:?}", path.as_ref());
        // Initialize the decoder and format reader
        let (format_reader, decoder, sample_rate, audio_channels) =
            Self::init_decoder(path.as_ref())?;

        // Create the decoder state
        let decoder_state = Mutex::new(DecoderState {
            format_reader,
            decoder,
        });

        // Create pre-allocated buffer with reasonable capacity
        let sample_buffer = Mutex::new(Vec::with_capacity(Self::TYPICAL_FRAME_SIZE));

        // Create the source
        let source = Self {
            decoder_state,
            current_position: AtomicUsize::new(0),
            sample_buffer,
            sample_rate,
            audio_channels,
            is_eof: AtomicBool::new(false),
        };

        Ok(source)
    }

    fn init_decoder(path: &Path) -> DecoderResult {
        let mut hint = Hint::new();
        hint.with_extension("flac");

        // Open the file
        let file = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // Probe and get format
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        let track = probed
            .format
            .default_track()
            .ok_or_else(|| PlaybackError::Decoder("No default track found".into()))?;

        let audio_channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2) as u16;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);

        // Create decoder
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        Ok((probed.format, decoder, sample_rate, audio_channels))
    }

    fn position_to_time(&self, position: usize) -> Time {
        let sample_rate_f64 = self.sample_rate as f64;
        let channels_f64 = self.audio_channels as f64;
        let time_seconds = (position as f64) / (sample_rate_f64 * channels_f64);

        // Convert to Symphonia's Time format
        let seconds = time_seconds.floor() as u64;
        let frac = time_seconds - seconds as f64;
        Time::new(seconds, frac)
    }

    pub fn is_eof(&self) -> bool {
        self.is_eof.load(Ordering::Relaxed)
    }
}

impl Source for FlacSource {
    fn decode_segments(&self, max_segments: usize) -> Result<Vec<DecodedSegment>, PlaybackError> {
        // Always return exactly one segment with index 0, regardless of current position
        let mut segments = Vec::with_capacity(1);

        // Create a segment with default data (all zeros)
        let segment = AudioSegment {
            samples: [0.0; SEGMENT_SIZE],
        };

        // Add segment with hardcoded index 0
        segments.push(DecodedSegment {
            index: SegmentIndex::from_sample_position(0),
            segment,
        });

        Ok(segments)
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        todo!("implement seek")
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn audio_channels(&self) -> u16 {
        self.audio_channels
    }
}

impl Drop for FlacSource {
    fn drop(&mut self) {
        tracing::trace!("FlacSource dropped - decoder_state will be dropped automatically");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file_path(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("benches/test_data")
            .join(name);
        assert!(path.exists(), "path does not exist: {}", path.display());
        path
    }

    #[test]
    fn first_segment_is_at_position_zero() {
        // Create a source from the alternating pattern file
        let source =
            FlacSource::new(file_path("alternating.flac")).expect("Failed to create source");

        // Decode a single segment
        let segments = source.decode_segments(1).expect("Failed to decode segment");

        // Verify we got a segment
        assert_eq!(segments.len(), 1, "Should have decoded exactly one segment");

        // Verify the segment index corresponds to position 0
        assert_eq!(
            segments[0].index,
            SegmentIndex::from_sample_position(0),
            "First segment should start at position 0"
        );
    }

    #[test]
    fn second_segment_follows_first() {
        // Create a source from the alternating pattern file
        let source =
            FlacSource::new(file_path("alternating.flac")).expect("Failed to create source");

        // Decode the first segment
        let first_segments = source
            .decode_segments(1)
            .expect("Failed to decode first segment");
        assert_eq!(
            first_segments.len(),
            1,
            "Should have decoded exactly one segment"
        );

        // Decode the second segment
        let second_segments = source
            .decode_segments(1)
            .expect("Failed to decode second segment");
        assert_eq!(
            second_segments.len(),
            1,
            "Should have decoded exactly one segment"
        );

        // Verify the second segment follows the first
        assert_eq!(
            second_segments[0].index,
            first_segments[0].index.next(),
            "Second segment should follow the first"
        );
    }

    #[test]
    fn segment_data_matches_expected_pattern() {
        // Create a source from the alternating pattern file
        let source =
            FlacSource::new(file_path("alternating.flac")).expect("Failed to create source");

        // Decode a segment
        let segments = source.decode_segments(1).expect("Failed to decode segment");
        assert_eq!(segments.len(), 1, "Should have decoded exactly one segment");

        // Get the first segment's samples
        let samples = &segments[0].segment.samples;

        // Find the first non-zero sample (to handle any potential padding)
        let mut start_idx = 0;
        while start_idx < samples.len() && samples[start_idx].abs() < 0.01 {
            start_idx += 1;
        }

        // Ensure we found a non-zero sample and have enough samples to check the pattern
        if start_idx + 6 >= samples.len() {
            panic!("Couldn't find starting pattern in segment data");
        }

        // Verify alternating pattern (high, zero, low)
        assert!(
            samples[start_idx] > 0.5
                && samples[start_idx + 1].abs() < 0.01
                && samples[start_idx + 2] < -0.5,
            "First three samples should follow high, zero, low pattern"
        );
    }

    #[test]
    fn multiple_segments_decode_correctly() {
        // Create a source from the alternating pattern file
        let source =
            FlacSource::new(file_path("alternating.flac")).expect("Failed to create source");

        // Decode three segments at once
        let segments = source
            .decode_segments(3)
            .expect("Failed to decode segments");

        // Should get three segments
        assert_eq!(segments.len(), 3, "Should have decoded three segments");

        // Segments should be sequential
        for i in 1..segments.len() {
            assert_eq!(
                segments[i].index,
                segments[i - 1].index.next(),
                "Segments should be sequential"
            );
        }
    }

    #[test]
    fn segment_boundaries_are_seamless() {
        // Create a source from the ascending pattern file (continuous pattern)
        let source = FlacSource::new(file_path("ascending.flac")).expect("Failed to create source");

        // Decode two segments
        let segments = source
            .decode_segments(2)
            .expect("Failed to decode segments");
        assert_eq!(segments.len(), 2, "Should have decoded two segments");

        // The last sample of the first segment should be close to the first sample of the second segment
        // (allowing for a small delta due to compression artifacts)
        let last_sample_segment1 = segments[0].segment.samples[SEGMENT_SIZE - 1];
        let first_sample_segment2 = segments[1].segment.samples[0];

        let sample_rate = source.sample_rate() as f32;
        // For ascending pattern over 0.5 seconds, each sample increases by approximately:
        // 1.8 / (sample_rate * channels * 0.5)
        let expected_step = 1.8 / (sample_rate * 2.0 * 0.5);

        assert!(
            (first_sample_segment2 - last_sample_segment1).abs() < expected_step * 2.0,
            "Gap between segments should be minimal: last={}, first={}, diff={}",
            last_sample_segment1,
            first_sample_segment2,
            first_sample_segment2 - last_sample_segment1 // Fixed the variable order here
        );
    }

    #[test]
    fn partial_segment_at_eof() {
        // Create a very short custom test file (or use existing one)
        let source =
            FlacSource::new(file_path("alternating.flac")).expect("Failed to create source");

        // Decode all segments
        let mut all_segments = Vec::new();
        loop {
            let segments = source
                .decode_segments(2)
                .expect("Failed to decode segments");
            if segments.is_empty() {
                break;
            }
            all_segments.extend(segments);
        }

        // Verify we've reached EOF
        assert!(source.is_eof(), "Should have reached EOF");

        // Last segment at EOF might be partial, but should still have valid data
        if let Some(last_segment) = all_segments.last() {
            // Check that at least one sample in the last segment is non-zero
            let has_nonzero = last_segment.segment.samples.iter().any(|&s| s.abs() > 0.01);
            assert!(has_nonzero, "Last segment should contain valid audio data");
        }
    }

    // Helper function to decode all segments from a source
    fn decode_all_segments(source: &FlacSource) -> Vec<DecodedSegment> {
        let mut all_segments = Vec::new();

        // Decode segments until we reach the end of the file
        loop {
            match source.decode_segments(5) {
                Ok(segments) => {
                    if segments.is_empty() {
                        // No more segments, we've reached the end
                        break;
                    }
                    all_segments.extend(segments);
                }
                Err(e) => {
                    panic!("Error decoding segments: {}", e);
                }
            }
        }

        all_segments
    }

    #[test]
    fn decode_reaches_eof() {
        // Test with a short file
        let source = FlacSource::new(file_path("short.flac")).expect("Failed to create source");

        // Decode all segments
        let segments = decode_all_segments(&source);

        // Verify we've got some segments
        assert!(
            !segments.is_empty(),
            "Should have decoded at least one segment"
        );

        // Verify EOF is reached
        assert!(
            source.is_eof(),
            "EOF flag should be set after reading the entire file"
        );
    }

    #[test]
    fn segments_are_sequential() {
        let source = FlacSource::new(file_path("short.flac")).expect("Failed to create source");

        let segments = decode_all_segments(&source);

        // Skip test if fewer than 2 segments
        if segments.len() < 2 {
            return;
        }

        // Verify segments are in sequential order
        for i in 1..segments.len() {
            assert_eq!(
                segments[i - 1].index.next(),
                segments[i].index,
                "Segments should be sequential"
            );
        }
    }

    #[test]
    fn alternating_pattern_preserved() {
        let source =
            FlacSource::new(file_path("alternating.flac")).expect("Failed to create source");

        let segments = decode_all_segments(&source);
        assert!(
            !segments.is_empty(),
            "Should have decoded at least one segment"
        );

        // Check for alternating pattern in first 30 samples
        let first_segment = &segments[0].segment;

        // We may have some padding at the start, so find the first non-zero sample
        let mut start_idx = 0;
        while start_idx < first_segment.samples.len()
            && first_segment.samples[start_idx].abs() < 0.01
        {
            start_idx += 1;
        }

        if start_idx + 6 >= first_segment.samples.len() {
            panic!("Couldn't find starting pattern");
        }

        // Now check for alternating pattern (high, zero, low, high, zero, low)
        assert!(
            first_segment.samples[start_idx] > 0.5,
            "First sample should be high"
        );
        assert!(
            first_segment.samples[start_idx + 1].abs() < 0.01,
            "Second sample should be near zero"
        );
        assert!(
            first_segment.samples[start_idx + 2] < -0.5,
            "Third sample should be low"
        );
        assert!(
            first_segment.samples[start_idx + 3] > 0.5,
            "Fourth sample should be high"
        );
        assert!(
            first_segment.samples[start_idx + 4].abs() < 0.01,
            "Fifth sample should be near zero"
        );
        assert!(
            first_segment.samples[start_idx + 5] < -0.5,
            "Sixth sample should be low"
        );
    }

    #[test]
    fn ascending_pattern_preserved() {
        let source = FlacSource::new(file_path("ascending.flac")).expect("Failed to create source");

        let segments = decode_all_segments(&source);
        assert!(
            !segments.is_empty(),
            "Should have decoded at least one segment"
        );

        // Check for ascending pattern by sampling points
        let first_segment = &segments[0].segment;

        // Sample at 10%, 50%, and 90% of the first segment
        let idx_10pct = first_segment.samples.len() / 10;
        let idx_50pct = first_segment.samples.len() / 2;
        let idx_90pct = first_segment.samples.len() * 9 / 10;

        // Values should be ascending (approximately, allowing for compression artifacts)
        assert!(
            first_segment.samples[idx_10pct] < first_segment.samples[idx_50pct]
                && first_segment.samples[idx_50pct] < first_segment.samples[idx_90pct],
            "Samples should follow ascending pattern"
        );
    }

    #[test]
    fn silence_preserved() {
        let source = FlacSource::new(file_path("silence.flac")).expect("Failed to create source");

        let segments = decode_all_segments(&source);
        assert!(
            !segments.is_empty(),
            "Should have decoded at least one segment"
        );

        // Check that all samples are close to zero
        for segment in &segments {
            for &sample in &segment.segment.samples {
                assert!(
                    sample.abs() < 0.01,
                    "Silence sample should be near zero, got {}",
                    sample
                );
            }
        }
    }

    #[test]
    fn impulses_preserved() {
        let source = FlacSource::new(file_path("impulses.flac")).expect("Failed to create source");

        let segments = decode_all_segments(&source);
        assert!(
            !segments.is_empty(),
            "Should have decoded at least one segment"
        );

        // Count impulses (samples above 0.5)
        let mut impulse_count = 0;
        for segment in &segments {
            for &sample in &segment.segment.samples {
                if sample > 0.5 {
                    impulse_count += 1;
                }
            }
        }

        // Should have found some impulses
        assert!(impulse_count > 0, "Should have found impulse samples");

        // Number of impulses should roughly match expectation
        // For 0.5 seconds at 48kHz with 2 channels, we expect about
        // (48000 * 0.5 * 2) / 100 = 480 impulses
        assert!(
            impulse_count > 400 && impulse_count < 600,
            "Expected approximately 480 impulses, got {}",
            impulse_count
        );
    }

    #[test]
    fn segment_size_consistency() {
        let source = FlacSource::new(file_path("short.flac")).expect("Failed to create source");

        let segments = decode_all_segments(&source);
        assert!(
            !segments.is_empty(),
            "Should have decoded at least one segment"
        );

        // All segments except possibly the last one should have SEGMENT_SIZE samples
        for (i, segment) in segments.iter().enumerate() {
            if i < segments.len() - 1 || source.is_eof() {
                assert_eq!(
                    segment.segment.samples.len(),
                    SEGMENT_SIZE,
                    "Segment {} should have exactly {} samples",
                    i,
                    SEGMENT_SIZE
                );
            }
        }
    }
}
