use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use crossbeam::channel::{bounded, Sender};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

use crate::error::PlaybackError;

pub struct AudioOutput {
    _stream: Stream,
    _device: Device,
    _config: StreamConfig,
    command_tx: Sender<AudioCommand>,
}

enum AudioCommand {
    AddTrack {
        channel: crate::Channel,
        track: crate::Track,
    },
    RemoveTrack(crate::Channel),
}

type Tracks = Arc<RwLock<Vec<(crate::Channel, Arc<RwLock<crate::Track>>)>>>;

impl AudioOutput {
    pub fn new() -> Result<Self, PlaybackError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::AudioDevice("No output device found".into()))?;

        let config = device
            .default_output_config()
            .map_err(|e| PlaybackError::AudioDevice(e.to_string()))?
            .config();

        // Create command channel
        let (command_tx, command_rx) = bounded::<AudioCommand>(32);

        // Create mixing buffer
        let mix_buffer = vec![0f32; config.channels as usize * 1024];
        let mix_buffer = Arc::new(RwLock::new(mix_buffer));

        // Track management
        let tracks: Tracks = Arc::new(RwLock::new(Vec::new()));

        let tracks_ref = Arc::clone(&tracks);
        let mix_buffer_ref = Arc::clone(&mix_buffer);

        // Build audio callback
        let audio_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Process any pending commands
            while let Ok(cmd) = command_rx.try_recv() {
                let mut tracks = tracks_ref.write();
                match cmd {
                    AudioCommand::AddTrack { channel, track } => {
                        tracks.push((channel, Arc::new(RwLock::new(track))));
                    }
                    AudioCommand::RemoveTrack(channel) => {
                        tracks.retain(|(ch, _)| *ch != channel);
                    }
                }
            }

            // Clear output buffer
            for sample in data.iter_mut() {
                *sample = 0.0;
            }

            // Mix all playing tracks
            let tracks = tracks_ref.read();
            for (_, track) in tracks.iter() {
                let mut track = track.write();
                if track.is_playing() {
                    let mut mix_buffer = mix_buffer_ref.write();
                    if let Ok(len) = track.get_next_samples(&mut mix_buffer) {
                        // Mix into output buffer with volume
                        let volume = track.get_volume();
                        for i in 0..len.min(data.len()) {
                            data[i] += mix_buffer[i] * volume;
                        }
                    }
                }
            }

            // Apply master limiter to prevent clipping
            for sample in data.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
        };

        // Error callback
        let error_callback = move |err: cpal::StreamError| {
            eprintln!("Audio stream error: {}", err);
        };

        // Build output stream with buffer duration of 50ms
        let stream = device
            .build_output_stream(
                &config,
                audio_callback,
                error_callback,
                Some(Duration::from_millis(50)),
            )
            .map_err(|e| PlaybackError::AudioDevice(e.to_string()))?;

        // Start the stream
        stream
            .play()
            .map_err(|e| PlaybackError::AudioDevice(e.to_string()))?;

        Ok(Self {
            _stream: stream,
            _device: device,
            _config: config,
            command_tx,
        })
    }

    pub fn add_track(
        &self,
        channel: crate::Channel,
        track: crate::Track,
    ) -> Result<(), PlaybackError> {
        self.command_tx
            .send(AudioCommand::AddTrack { channel, track })
            .map_err(|_| PlaybackError::AudioDevice("Failed to send add track command".into()))
    }

    pub fn remove_track(&self, channel: crate::Channel) -> Result<(), PlaybackError> {
        self.command_tx
            .send(AudioCommand::RemoveTrack(channel))
            .map_err(|_| PlaybackError::AudioDevice("Failed to send remove track command".into()))
    }

    pub fn get_sample_rate(&self) -> u32 {
        self._config.sample_rate.0
    }

    pub fn get_channels(&self) -> u16 {
        self._config.channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_output_creation() {
        let audio = AudioOutput::new();
        assert!(audio.is_ok(), "Failed to create audio output");
    }

    #[test]
    fn test_sample_rate_query() {
        if let Ok(audio) = AudioOutput::new() {
            assert!(audio.get_sample_rate() > 0, "Invalid sample rate");
            assert!(audio.get_channels() > 0, "Invalid channel count");
        }
    }
}
