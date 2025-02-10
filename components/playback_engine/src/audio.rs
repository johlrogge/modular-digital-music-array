use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream};
use crossbeam::channel::{unbounded, Sender};
use parking_lot::RwLock;
use std::sync::Arc;

use crate::error::PlaybackError;
use crate::track::Track;
use playback_primitives::Channel;

type Tracks = Arc<RwLock<Vec<(Channel, Arc<RwLock<Track>>)>>>;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2; // Stereo
const PLAYBACK_BUFFER_SIZE: usize = 480; // 10ms at 48kHz
const DECODE_BUFFER_SIZE: usize = 960; // Double for resampling headroom

pub struct AudioOutput {
    _stream: Stream,
    _device: Device,
    tracks: Tracks,
    command_tx: Sender<AudioCommand>,
}

enum AudioCommand {
    AddTrack { channel: Channel, track: Track },
    RemoveTrack(Channel),
}

impl AudioOutput {
    pub fn new() -> Result<Self, PlaybackError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::AudioDevice("No output device found".into()))?;

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(PLAYBACK_BUFFER_SIZE as u32),
        };

        let (command_tx, command_rx) = unbounded();

        let tracks: Arc<RwLock<Vec<(Channel, Arc<RwLock<Track>>)>>> =
            Arc::new(RwLock::new(Vec::new()));
        let tracks_ref = Arc::clone(&tracks);
        let tracks_for_commands = Arc::clone(&tracks);
        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                let mut tracks = tracks_for_commands.write();
                match command {
                    AudioCommand::AddTrack { channel, track } => {
                        // Remove any existing track on this channel
                        tracks.retain(|(ch, _)| *ch != channel);
                        // Add the new track
                        tracks.push((channel, Arc::new(RwLock::new(track))));
                        tracing::info!("Added track to channel {:?}", channel);
                    }
                    AudioCommand::RemoveTrack(channel) => {
                        tracks.retain(|(ch, _)| *ch != channel);
                        tracing::info!("Removed track from channel {:?}", channel);
                    }
                }
            }
            tracing::info!("Command processing stopped");
        });

        // Mix buffer sized for decode buffer
        let mix_buffer = vec![0f32; DECODE_BUFFER_SIZE];
        let mix_buffer = Arc::new(RwLock::new(mix_buffer));
        let mix_buffer_ref = Arc::clone(&mix_buffer);

        let audio_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Clear output buffer
            data.fill(0.0);

            // Calculate how many samples we should provide for this callback
            // data.len() already accounts for stereo (it's the total number of samples needed)
            let samples_per_callback = data.len();

            tracing::debug!("Audio callback requesting {} samples", samples_per_callback);

            // Mix all playing tracks
            let tracks = tracks_ref.read();
            let mut mix_buffer = mix_buffer_ref.write();
            mix_buffer.fill(0.0);

            for (channel, track) in tracks.iter() {
                let mut track = track.write();
                if track.is_playing() {
                    match track.get_next_samples(&mut mix_buffer[..samples_per_callback]) {
                        Ok(len) if len > 0 => {
                            tracing::debug!("Got {} samples from channel {:?}", len, channel);
                            let volume = track.get_volume();
                            // Copy exact number of samples we got
                            for i in 0..len {
                                data[i] += mix_buffer[i] * volume;
                            }
                        }
                        Ok(_) => {
                            tracing::debug!("No samples from channel {:?}", channel);
                        }
                        Err(e) => {
                            tracing::error!(
                                "Error getting samples from channel {:?}: {}",
                                channel,
                                e
                            );
                        }
                    }
                }
            }

            // Apply limiter to prevent clipping
            for sample in data.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
        };

        // Build output stream with minimal latency
        let stream = device.build_output_stream(
            &config,
            audio_callback,
            move |err| eprintln!("Audio stream error: {}", err),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            _stream: stream,
            _device: device,
            tracks,
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

    pub fn tracks(&self) -> &Tracks {
        &self.tracks
    }
}
