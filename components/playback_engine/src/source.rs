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
}

impl Source for FlacSource {
    fn decode_segments(&self, max_segments: usize) -> Result<Vec<DecodedSegment>, PlaybackError> {
        todo!("implement decode segments")
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
