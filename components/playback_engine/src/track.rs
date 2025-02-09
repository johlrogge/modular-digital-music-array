use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use symphonia::core::audio::{Channels, SampleBuffer, SignalSpec};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::PlaybackError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    A,
    B,
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::A => write!(f, "A"),
            Channel::B => write!(f, "B"),
        }
    }
}

pub struct Track {
    decoder: Arc<RwLock<Box<dyn symphonia::core::codecs::Decoder>>>,
    format: Arc<RwLock<Box<dyn FormatReader>>>,
    buffer: Arc<RwLock<SampleBuffer<f32>>>,
    playing: Arc<RwLock<bool>>,
    volume: Arc<RwLock<f32>>,
}

impl Track {
    pub fn new(path: &Path) -> Result<Self, PlaybackError> {
        if !path.exists() {
            return Err(PlaybackError::TrackNotFound(path.to_owned()));
        }

        // Open media source
        let src = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(src), Default::default());

        // Create probe hint
        let mut hint = Hint::new();
        hint.with_extension("flac");

        // Probe format
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        // Get default track
        let track = probed
            .format
            .default_track()
            .ok_or_else(|| PlaybackError::Decoder("No default track found".into()))?;

        // Get decoder
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        // Create sample buffer with proper signal spec
        let spec = SignalSpec::new(
            track.codec_params.sample_rate.unwrap_or(44100),
            track
                .codec_params
                .channels
                .unwrap_or(Channels::FRONT_LEFT | Channels::FRONT_RIGHT),
        );

        Ok(Self {
            decoder: Arc::new(RwLock::new(decoder)), // decoder is already a Box<dyn Decoder>
            format: Arc::new(RwLock::new(probed.format)),
            buffer: Arc::new(RwLock::new(SampleBuffer::new(1024, spec))),
            playing: Arc::new(RwLock::new(false)),
            volume: Arc::new(RwLock::new(1.0)),
        })
    }

    pub fn play(&mut self) {
        *self.playing.write() = true;
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

    pub fn get_next_samples(&mut self, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.is_playing() {
            return Ok(0);
        }

        let mut format = self.format.write();
        let mut decoder = self.decoder.write();
        let mut sample_buf = self.buffer.write();
        let volume = self.get_volume();

        let packet = format
            .next_packet()
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        let decoded = decoder
            .decode(&packet)
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();
        let len = std::cmp::min(buffer.len(), samples.len());

        for i in 0..len {
            buffer[i] = samples[i] * volume;
        }

        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_track_volume() {
        let path = PathBuf::from("test.flac");
        assert!(Track::new(&path).is_err()); // Should fail as file doesn't exist
    }
}
