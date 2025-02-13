use parking_lot::RwLock;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::PlaybackError;

const MINUTES_TO_BUFFER: usize = 3; // Store 3 minutes of audio
pub struct Track {
    playback_buffer: Arc<Vec<f32>>,
    buffer_position: Arc<AtomicUsize>,
    playing: Arc<RwLock<bool>>,
    volume: Arc<RwLock<f32>>,
    sample_rate: usize,
    channels: usize,
}

use std::io::{self, Read, Seek, SeekFrom};
use symphonia::core::io::MediaSource;

pub struct ReadMetrics {
    pub read_calls: usize,
    pub bytes_read: usize,
}

struct MetricsReader<R> {
    inner: R,
    read_calls: Arc<AtomicUsize>,
    bytes_read: Arc<AtomicUsize>,
}

impl<R: Read + Seek + Send + Sync> MediaSource for MetricsReader<R> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        None // We'll let symphonia handle this
    }
}

impl<R: Read> Read for MetricsReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_calls.fetch_add(1, Ordering::Relaxed);
        let bytes = self.inner.read(buf)?;
        self.bytes_read.fetch_add(bytes, Ordering::Relaxed);
        Ok(bytes)
    }
}

impl<R: Seek> Seek for MetricsReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<R> MetricsReader<R> {
    fn new(inner: R) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let read_calls = Arc::new(AtomicUsize::new(0));
        let bytes_read = Arc::new(AtomicUsize::new(0));

        (
            Self {
                inner,
                read_calls: read_calls.clone(),
                bytes_read: bytes_read.clone(),
            },
            read_calls,
            bytes_read,
        )
    }
}

#[derive(Debug)]
pub struct DecodingStats {
    pub packet_read_time: Duration,
    pub packet_decode_time: Duration,
    pub sample_copy_time: Duration,
    pub packets_processed: usize,
    pub largest_packet: usize,
    pub smallest_packet: usize,
    pub total_packet_bytes: usize,
}

#[derive(Debug)]
pub struct LoadMetrics {
    pub file_open_time: Duration,
    pub decoder_creation_time: Duration,
    pub buffer_allocation_time: Duration,
    pub decoding_time: Duration,
    pub decoding_stats: DecodingStats,
    pub total_time: Duration,
    pub decoded_frames: usize,
    pub buffer_size: usize,
    pub read_calls: usize,
    pub bytes_read: usize,
}

impl LoadMetrics {
    pub fn new(start_time: Instant) -> Self {
        Self {
            file_open_time: Duration::ZERO,
            decoder_creation_time: Duration::ZERO,
            buffer_allocation_time: Duration::ZERO,
            decoding_time: Duration::ZERO,
            decoding_stats: DecodingStats {
                packet_read_time: Duration::ZERO,
                packet_decode_time: Duration::ZERO,
                sample_copy_time: Duration::ZERO,
                packets_processed: 0,
                largest_packet: 0,
                smallest_packet: usize::MAX,
                total_packet_bytes: 0,
            },
            total_time: start_time.elapsed(),
            decoded_frames: 0,
            buffer_size: 0,
            read_calls: 0,
            bytes_read: 0,
        }
    }
}

impl Track {
    pub fn new(path: &Path) -> Result<(Self, LoadMetrics), PlaybackError> {
        let start_time = Instant::now();
        let mut metrics = LoadMetrics::new(start_time);

        if !path.exists() {
            return Err(PlaybackError::TrackNotFound(path.to_owned()));
        }

        // Measure file opening
        let file_start = Instant::now();
        let src = std::fs::File::open(path)?;
        let buffered_reader = std::io::BufReader::with_capacity(128 * 1024, src);
        let (metrics_reader, read_calls, bytes_read) = MetricsReader::new(buffered_reader);
        let mss = MediaSourceStream::new(Box::new(metrics_reader), Default::default());
        metrics.file_open_time = file_start.elapsed();

        let mut hint = Hint::new();
        hint.with_extension("flac");

        // Measure decoder creation
        let decoder_start = Instant::now();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();

        let mut probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        let track_id = probed
            .format
            .default_track()
            .ok_or_else(|| PlaybackError::Decoder("No default track found".into()))?
            .id;

        let params = probed
            .format
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .ok_or_else(|| PlaybackError::Decoder("Track not found".into()))?
            .codec_params
            .clone();

        let mut decoder = symphonia::default::get_codecs()
            .make(&params, &Default::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;
        metrics.decoder_creation_time = decoder_start.elapsed();

        // Get track parameters
        let channels = params
            .channels
            .unwrap_or(Channels::FRONT_LEFT | Channels::FRONT_RIGHT)
            .count();
        let sample_rate = params.sample_rate.unwrap_or(44100) as usize;

        // Measure buffer allocation
        let buffer_start = Instant::now();
        let samples_per_channel = sample_rate * 60 * MINUTES_TO_BUFFER;
        let total_samples = samples_per_channel * channels;
        let mut playback_buffer = Vec::with_capacity(total_samples);
        metrics.buffer_allocation_time = buffer_start.elapsed();
        metrics.buffer_size = total_samples;

        // Measure decoding
        let decode_start = Instant::now();
        let mut total_frames = 0;

        loop {
            // Measure packet reading
            let packet_read_start = Instant::now();
            let packet = match probed.format.next_packet() {
                Ok(packet) => {
                    metrics.decoding_stats.packet_read_time += packet_read_start.elapsed();
                    metrics.decoding_stats.packets_processed += 1;
                    let packet_size = packet.data.len();
                    metrics.decoding_stats.total_packet_bytes += packet_size;
                    metrics.decoding_stats.largest_packet =
                        metrics.decoding_stats.largest_packet.max(packet_size);
                    metrics.decoding_stats.smallest_packet =
                        metrics.decoding_stats.smallest_packet.min(packet_size);
                    packet
                }
                Err(_) => break,
            };

            // Measure packet decoding
            let decode_start = Instant::now();
            let decoded = match decoder.decode(&packet) {
                Ok(decoded) => {
                    metrics.decoding_stats.packet_decode_time += decode_start.elapsed();
                    decoded
                }
                Err(e) => {
                    tracing::warn!("Error decoding packet: {}", e);
                    break;
                }
            };

            // Measure sample copying
            let copy_start = Instant::now();
            let frames = decoded.frames();
            let mut sample_buf = SampleBuffer::<f32>::new(frames as u64, *decoded.spec());
            sample_buf.copy_interleaved_ref(decoded);
            playback_buffer.extend_from_slice(sample_buf.samples());
            metrics.decoding_stats.sample_copy_time += copy_start.elapsed();

            total_frames += frames;

            if playback_buffer.len() >= total_samples {
                break;
            }
        }

        metrics.decoding_time = decode_start.elapsed();
        metrics.decoded_frames = total_frames;

        // Collect IO metrics
        metrics.read_calls = read_calls.load(Ordering::Relaxed);
        metrics.bytes_read = bytes_read.load(Ordering::Relaxed);
        metrics.total_time = start_time.elapsed();

        Ok((
            Self {
                playback_buffer: Arc::new(playback_buffer),
                buffer_position: Arc::new(AtomicUsize::new(0)),
                playing: Arc::new(RwLock::new(false)),
                volume: Arc::new(RwLock::new(1.0)),
                sample_rate,
                channels,
            },
            metrics,
        ))
    }

    pub fn get_next_samples(&mut self, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.is_playing() {
            return Ok(0);
        }

        let position = self.buffer_position.load(Ordering::Relaxed);
        let available = self.playback_buffer.len().saturating_sub(position);

        if available == 0 {
            tracing::info!("Reached end of buffered audio");
            *self.playing.write() = false;
            return Ok(0);
        }

        let len = std::cmp::min(buffer.len(), available);

        // Copy samples and apply volume
        let volume = self.get_volume();
        for i in 0..len {
            buffer[i] = self.playback_buffer[position + i] * volume;
        }

        // Update position
        self.buffer_position
            .store(position + len, Ordering::Relaxed);

        Ok(len)
    }

    pub fn play(&mut self) {
        *self.playing.write() = true;
        self.buffer_position.store(0, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        *self.playing.write() = false;
    }

    pub fn is_playing(&self) -> bool {
        *self.playing.read()
    }

    pub fn set_volume(&mut self, db: f32) {
        // Convert dB to linear amplitude
        let linear = 10.0f32.powf(db / 20.0);
        *self.volume.write() = linear;
    }

    pub fn get_volume(&self) -> f32 {
        *self.volume.read()
    }
}
#[cfg(test)]
impl Track {
    /// Creates a test track with a simple test signal for unit testing
    pub(crate) fn new_test() -> Self {
        // Create a simple square wave for testing
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let samples_per_cycle = sample_rate as f32 / frequency;
        let total_samples = sample_rate; // 1 second of audio

        let mut buffer = Vec::with_capacity(total_samples);
        for i in 0..total_samples {
            // Create a square wave that alternates between 0.1 and -0.1
            let sample = if (i as f32 / samples_per_cycle).floor() % 2.0 == 0.0 {
                0.1
            } else {
                -0.1
            };
            buffer.push(sample);
        }

        Self {
            playback_buffer: Arc::new(buffer),
            buffer_position: Arc::new(AtomicUsize::new(0)),
            playing: Arc::new(RwLock::new(false)),
            volume: Arc::new(RwLock::new(1.0)),
            sample_rate,
            channels: 2,
        }
    }
}

#[cfg(test)]
mod track_tests {
    use super::*;

    #[test]
    fn test_new_test_creates_valid_track() {
        let track = Track::new_test();
        assert_eq!(track.sample_rate, 48000);
        assert_eq!(track.channels, 2);
        assert_eq!(track.get_volume(), 1.0);
        assert!(!track.is_playing());
    }

    #[test]
    fn test_new_test_provides_non_zero_samples() {
        let mut track = Track::new_test();
        let mut buffer = vec![0.0; 1024];

        track.play();
        let samples_read = track.get_next_samples(&mut buffer).unwrap();

        assert_eq!(samples_read, 1024);
        // Verify we got non-zero samples
        assert!(!buffer[..samples_read].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_new_test_signal_alternates() {
        let mut track = Track::new_test();
        let mut buffer = vec![0.0; 1024];

        track.play();
        let samples_read = track.get_next_samples(&mut buffer).unwrap();

        // Check that we have both positive and negative samples
        let has_positive = buffer[..samples_read].iter().any(|&x| x > 0.0);
        let has_negative = buffer[..samples_read].iter().any(|&x| x < 0.0);
        assert!(
            has_positive && has_negative,
            "Test signal should alternate between positive and negative"
        );
    }
}
