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
    pub fn seek(&mut self, position: usize) {
        let max_position = self.source.len();
        self.position = position.min(max_position);

        tracing::info!(
            "Track seeking to position={}/{}, playing={}, volume={}",
            self.position,
            max_position,
            self.playing,
            self.volume
        );
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn length(&self) -> usize {
        self.source.len()
    }

    pub fn get_next_samples(&mut self, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            return Ok(0);
        }

        // Read samples directly into the provided buffer
        let read = self.source.read_samples(self.position, buffer)?;

        if read == 0 {
            self.playing = false;
            return Ok(0);
        }

        // Apply volume
        for sample in &mut buffer[..read] {
            *sample *= self.volume;
        }

        self.position += read;
        Ok(read)
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
    pub fn shutdown(&mut self) {
        // Get a mutable reference to the underlying source
        // This is a bit tricky since we're using Arc<dyn Source>
        // We might need to modify the Source trait to add a shutdown method

        // For now, we'll rely on Drop to handle cleanup
        // But in a more complete implementation, we'd want something like:
        // if let Some(flac_source) = self.source.downcast_mut::<FlacSource>() {
        //     flac_source.shutdown();
        // }
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
            fn read_samples(
                &self,
                position: usize,
                buffer: &mut [f32],
            ) -> Result<usize, PlaybackError> {
                if position >= self.samples.len() {
                    return Ok(0);
                }
                let available = self.samples.len() - position;
                let count = buffer.len().min(available);

                buffer[..count].copy_from_slice(&self.samples[position..position + count]);
                Ok(count)
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
