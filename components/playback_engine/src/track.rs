// components/playback_engine/src/track.rs
use std::path::Path;
use std::sync::Arc;

use crate::error::PlaybackError;
use crate::source::{FlacSource, Source};

#[derive(Clone)]
pub struct Track {
    source: Arc<dyn Source>, // Changed to Arc since Source needs to be shared when Track is cloned
    position: usize,
    playing: bool,
    volume: f32,
}

impl Track {
    pub fn new(path: &Path) -> Result<Track, PlaybackError> {
        let source = Arc::new(FlacSource::new(path)?);

        Ok(Track {
            source,
            position: 0,
            playing: false,
            volume: 1.0,
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
        tracing::info!(
            "Track playback started: playing={}, position={}, volume={}",
            self.playing,
            self.position,
            self.volume
        );
    }

    pub fn get_next_samples(&mut self, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            tracing::debug!("Track not playing, returning 0 samples");
            return Ok(0);
        }

        // Get samples from source
        let samples = self.source.view_samples(self.position, buffer.len())?;
        let len = samples.len();

        if len == 0 {
            self.playing = false;
            tracing::info!("Track reached end");
            return Ok(0);
        }

        // Log the first buffer we get
        if self.position == 0 {
            let non_zero = samples.iter().filter(|&&s| s.abs() > 1e-6).count();
            tracing::info!("First buffer: {} samples, {} non-zero", len, non_zero);
        }

        // Copy samples and apply volume
        for i in 0..len {
            buffer[i] = samples[i] * self.volume;
        }

        self.position += len;
        Ok(len)
    }
    pub fn stop(&mut self) {
        self.playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_volume(&mut self, db: f32) {
        // Convert dB to linear amplitude
        self.volume = 10.0f32.powf(db / 20.0);
    }

    pub fn get_volume(&self) -> f32 {
        self.volume
    }
}

#[cfg(test)]
impl Track {
    pub(crate) fn new_test() -> Self {
        // Create a simple 1-second sine wave source
        struct TestSource {
            samples: Vec<f32>,
        }

        impl Source for TestSource {
            fn view_samples(&self, position: usize, len: usize) -> Result<&[f32], PlaybackError> {
                if position >= self.samples.len() {
                    return Ok(&[]);
                }
                let end = position.saturating_add(len).min(self.samples.len());
                Ok(&self.samples[position..end])
            }

            fn sample_rate(&self) -> u32 {
                48000
            }
            fn audio_channels(&self) -> u16 {
                2
            }
            fn len(&self) -> usize {
                self.samples.len()
            }
        }

        // Generate 1 second of 440Hz test tone
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let mut samples = Vec::with_capacity(sample_rate);

        for i in 0..sample_rate {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.1;
            samples.push(sample);
        }

        Self {
            source: Arc::new(TestSource { samples }),
            position: 0,
            playing: false,
            volume: 1.0,
        }
    }
}
