// components/playback_engine/src/track.rs
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::PlaybackError;

// Initial buffer size (2 seconds of audio)
const INITIAL_BUFFER_SECONDS: f32 = 0.25; // Quarter second initial buffer

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

pub struct Track {
    buffer: Arc<RwLock<Vec<f32>>>,
    buffer_position: Arc<RwLock<usize>>,
    playing: Arc<RwLock<bool>>,
    volume: Arc<RwLock<f32>>,
    sample_rate: usize,
    channels: usize,
}

impl Track {
    pub fn new(path: &Path) -> Result<(Self, LoadMetrics), PlaybackError> {
        let start_time = Instant::now();
        let mut metrics = LoadMetrics {
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
            total_time: Duration::ZERO,
            decoded_frames: 0,
            buffer_size: 0,
            read_calls: 0,
            bytes_read: 0,
        };

        // Open and create decoder
        let file_start = Instant::now();
        let src = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(src), Default::default());
        metrics.file_open_time = file_start.elapsed();

        let mut hint = Hint::new();
        hint.with_extension("flac");

        let decoder_start = Instant::now();
        let mut probed = symphonia::default::get_probe()
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

        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;
        metrics.decoder_creation_time = decoder_start.elapsed();

        // Get track parameters
        let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100) as usize;

        // Allocate initial buffer for 2 seconds of audio
        let alloc_start = Instant::now();
        let initial_samples =
            (sample_rate as f32 * channels as f32 * INITIAL_BUFFER_SECONDS) as usize;
        let mut buffer = Vec::with_capacity(initial_samples);
        metrics.buffer_allocation_time = alloc_start.elapsed();
        metrics.buffer_size = initial_samples;

        // Decode initial buffer
        let decode_start = Instant::now();
        let mut total_frames = 0;

        while buffer.len() < initial_samples {
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

            let decode_start = Instant::now();
            let decoded = decoder
                .decode(&packet)
                .map_err(|e| PlaybackError::Decoder(e.to_string()))?;
            metrics.decoding_stats.packet_decode_time += decode_start.elapsed();

            let copy_start = Instant::now();
            let frames = decoded.frames();
            let mut sample_buf = SampleBuffer::<f32>::new(frames as u64, *decoded.spec());
            sample_buf.copy_interleaved_ref(decoded);
            buffer.extend_from_slice(sample_buf.samples());
            metrics.decoding_stats.sample_copy_time += copy_start.elapsed();

            total_frames += frames;
        }

        metrics.decoding_time = decode_start.elapsed();
        metrics.decoded_frames = total_frames;
        metrics.total_time = start_time.elapsed();

        Ok((
            Self {
                buffer: Arc::new(RwLock::new(buffer)),
                buffer_position: Arc::new(RwLock::new(0)),
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

        let position = *self.buffer_position.read();
        let track_buffer = self.buffer.read();
        let available = track_buffer.len().saturating_sub(position);

        if available == 0 {
            *self.playing.write() = false;
            return Ok(0);
        }

        let len = std::cmp::min(buffer.len(), available);
        let volume = *self.volume.read();

        // Copy samples and apply volume
        for i in 0..len {
            buffer[i] = track_buffer[position + i] * volume;
        }

        *self.buffer_position.write() = position + len;

        Ok(len)
    }

    pub fn play(&mut self) {
        *self.playing.write() = true;
        *self.buffer_position.write() = 0;
    }

    pub fn stop(&mut self) {
        *self.playing.write() = false;
    }

    pub fn is_playing(&self) -> bool {
        *self.playing.read()
    }

    pub fn set_volume(&mut self, db: f32) {
        let linear = 10.0f32.powf(db / 20.0);
        *self.volume.write() = linear;
    }

    pub fn get_volume(&self) -> f32 {
        *self.volume.read()
    }
}

#[cfg(test)]
impl Track {
    pub(crate) fn new_test() -> Self {
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let samples_per_cycle = sample_rate as f32 / frequency;
        let total_samples = sample_rate; // 1 second of audio

        let mut buffer = Vec::with_capacity(total_samples);
        for i in 0..total_samples {
            let sample = if (i as f32 / samples_per_cycle).floor() % 2.0 == 0.0 {
                0.1
            } else {
                -0.1
            };
            buffer.push(sample);
        }

        Self {
            buffer: Arc::new(RwLock::new(buffer)),
            buffer_position: Arc::new(RwLock::new(0)),
            playing: Arc::new(RwLock::new(false)),
            volume: Arc::new(RwLock::new(1.0)),
            sample_rate,
            channels: 2,
        }
    }
}
