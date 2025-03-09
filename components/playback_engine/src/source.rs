use crate::error::PlaybackError;
use parking_lot::{Mutex, RwLock};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use tokio::sync::Mutex as AsyncMutex;

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
    fn total_samples(&self) -> Option<usize>; // May not be known until file is fully decoded
}

/// Represents a decoded audio packet with its position information
struct DecodedPacket {
    samples: Vec<f32>,
    position: usize, // Sample position in the overall stream
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
        // Early return if we've already reached EOF
        if self.is_eof.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();
        let mut segments_created = 0;

        // Try to decode until we have the requested number of segments
        // or reach the end of the file
        while segments_created < max_segments {
            // Current position tells us what segment index we're at
            let current_position = self.current_position.load(Ordering::Relaxed);
            let current_segment_index = SegmentIndex::from_sample_position(current_position);
            let offset_in_segment = current_position % SEGMENT_SIZE;

            // Process any existing buffered samples first
            {
                let mut sample_buffer = self.sample_buffer.lock();

                if !sample_buffer.is_empty() {
                    // Create a segment from the buffered data
                    let mut segment = AudioSegment {
                        samples: [0.0; SEGMENT_SIZE],
                    };

                    // Zero-fill the segment first (for partial segments)
                    for i in 0..SEGMENT_SIZE {
                        segment.samples[i] = 0.0;
                    }

                    // Calculate how many samples we can use from the buffer
                    let samples_needed = SEGMENT_SIZE - offset_in_segment;
                    let samples_available = sample_buffer.len();
                    let samples_to_use = std::cmp::min(samples_needed, samples_available);

                    // Copy samples from buffer
                    for i in 0..samples_to_use {
                        segment.samples[offset_in_segment + i] = sample_buffer[i];
                    }

                    // Update buffer
                    if samples_to_use < samples_available {
                        // We used part of the buffer, keep the rest
                        *sample_buffer = sample_buffer[samples_to_use..].to_vec();
                    } else {
                        // We used all of the buffer
                        sample_buffer.clear();
                    }

                    // If we filled the segment completely
                    if offset_in_segment + samples_to_use == SEGMENT_SIZE {
                        result.push(DecodedSegment {
                            index: current_segment_index,
                            segment,
                        });

                        segments_created += 1;
                        self.current_position
                            .fetch_add(samples_to_use, Ordering::Release);

                        // If we've created enough segments, we're done
                        if segments_created == max_segments {
                            break;
                        }
                    } else {
                        // Segment is not complete - we just added what we had
                        self.current_position
                            .fetch_add(samples_to_use, Ordering::Release);
                    }
                }
            }

            // We need to decode more data
            // Decode next packet from format reader
            let mut decoder_state = self.decoder_state.lock();

            // Try to get the next packet
            let packet = match decoder_state.format_reader.next_packet() {
                Ok(packet) => packet,
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    // This is normal at end of file
                    // Any partial segment data will be handled on the next call
                    self.is_eof.store(true, Ordering::Release);
                    break;
                }
                Err(e) => {
                    return Err(PlaybackError::Decoder(format!(
                        "Error getting next packet: {}",
                        e
                    )));
                }
            };

            // Decode the packet
            let decoded = match decoder_state.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(e) => {
                    return Err(PlaybackError::Decoder(format!(
                        "Error decoding packet: {}",
                        e
                    )));
                }
            };

            // Convert to interleaved f32 samples
            let mut sample_buf = SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
            sample_buf.copy_interleaved_ref(decoded);
            let samples = sample_buf.samples();

            // Add samples to buffer
            {
                let mut sample_buffer = self.sample_buffer.lock();
                sample_buffer.extend_from_slice(samples);

                // Process complete segments
                while sample_buffer.len() >= SEGMENT_SIZE && segments_created < max_segments {
                    let current_position = self.current_position.load(Ordering::Relaxed);
                    let current_segment_index =
                        SegmentIndex::from_sample_position(current_position);

                    let mut segment = AudioSegment {
                        samples: [0.0; SEGMENT_SIZE],
                    };

                    // Copy samples from buffer
                    segment
                        .samples
                        .copy_from_slice(&sample_buffer[0..SEGMENT_SIZE]);

                    // Update buffer
                    if sample_buffer.len() > SEGMENT_SIZE {
                        *sample_buffer = sample_buffer[SEGMENT_SIZE..].to_vec();
                    } else {
                        sample_buffer.clear();
                    }

                    result.push(DecodedSegment {
                        index: current_segment_index,
                        segment,
                    });

                    segments_created += 1;
                    self.current_position
                        .fetch_add(SEGMENT_SIZE, Ordering::Release);
                }
            }
        }

        // If we've created no segments and reached EOF, return empty Vec
        if result.is_empty() && self.is_eof.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }

        Ok(result)
    }

    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // Reset EOF flag
        self.is_eof.store(false, Ordering::Release);

        // Clear sample buffer
        {
            let mut sample_buffer = self.sample_buffer.lock();
            sample_buffer.clear();
        }

        // Convert sample position to time value
        let time = self.position_to_time(position);

        // Perform the seek
        {
            let mut state = self.decoder_state.lock();

            // Define seek target
            let seek_to = SeekTo::Time {
                time,
                track_id: None, // Use default track
            };

            // Perform the seek
            state
                .format_reader
                .seek(SeekMode::Accurate, seek_to)
                .map_err(|e| PlaybackError::Decoder(format!("Seek error: {}", e)))?;

            // After seeking, we may need to recreate the decoder
            // because the internal state might be invalidated
            let track = state
                .format_reader
                .default_track()
                .ok_or_else(|| PlaybackError::Decoder("No default track found".into()))?;

            state.decoder = symphonia::default::get_codecs()
                .make(&track.codec_params, &DecoderOptions::default())
                .map_err(|e| PlaybackError::Decoder(e.to_string()))?;
        }

        // Update current position
        self.current_position.store(position, Ordering::Release);

        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn audio_channels(&self) -> u16 {
        self.audio_channels
    }

    fn total_samples(&self) -> Option<usize> {
        // We might not know the total until we've decoded the whole file
        // This would typically be extracted from metadata if available
        None
    }
}
