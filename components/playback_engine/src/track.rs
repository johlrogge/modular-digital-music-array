use parking_lot::RwLock;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
    sample_rate: u32,
    channels: usize,
}

impl Track {
    pub fn new(path: &Path) -> Result<Self, PlaybackError> {
        if !path.exists() {
            return Err(PlaybackError::TrackNotFound(path.to_owned()));
        }

        let src = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(src), Default::default());

        let mut hint = Hint::new();
        hint.with_extension("flac");

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

        // Get track parameters
        let channels = params
            .channels
            .unwrap_or(Channels::FRONT_LEFT | Channels::FRONT_RIGHT)
            .count();
        let sample_rate = params.sample_rate.unwrap_or(44100);

        // Calculate buffer size for N minutes of audio
        let samples_per_channel = sample_rate as usize * 60 * MINUTES_TO_BUFFER;
        let total_samples = samples_per_channel * channels;

        tracing::info!(
            "Allocating buffer for {} minutes of audio ({} samples, {} channels @ {}Hz)",
            MINUTES_TO_BUFFER,
            total_samples,
            channels,
            sample_rate
        );

        // Pre-decode audio into memory
        let mut playback_buffer = Vec::with_capacity(total_samples);
        let mut total_frames = 0;

        while let Ok(packet) = probed.format.next_packet() {
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let frames = decoded.frames();
                    let mut sample_buf =
                        SampleBuffer::<f32>::new(frames as u64, decoded.spec().clone());
                    sample_buf.copy_interleaved_ref(decoded);

                    // Copy samples to our main buffer
                    playback_buffer.extend_from_slice(sample_buf.samples());
                    total_frames += frames;

                    tracing::debug!(
                        "Decoded {} frames ({} samples), total frames: {}",
                        frames,
                        sample_buf.samples().len(),
                        total_frames
                    );

                    // Break if we've filled our buffer
                    if playback_buffer.len() >= total_samples {
                        tracing::info!("Reached buffer capacity, stopping decode");
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("Error decoding packet: {}", e);
                    break;
                }
            }
        }

        tracing::info!(
            "Loaded {} samples ({:.1} seconds of audio)",
            playback_buffer.len(),
            playback_buffer.len() as f32 / (channels as f32 * sample_rate as f32)
        );

        Ok(Self {
            playback_buffer: Arc::new(playback_buffer),
            buffer_position: Arc::new(AtomicUsize::new(0)),
            playing: Arc::new(RwLock::new(false)),
            volume: Arc::new(RwLock::new(1.0)),
            sample_rate,
            channels,
        })
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
