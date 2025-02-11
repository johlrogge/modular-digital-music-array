use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
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
        let (device, config) = Self::initialize_audio_device()?;

        let tracks = Arc::new(RwLock::new(Vec::new()));
        let command_tx = Self::initialize_command_processing(tracks.clone());
        let audio_callback = Self::create_audio_callback(tracks.clone());
        let stream = Self::create_audio_stream(&device, &config, audio_callback)?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            _device: device,
            tracks,
            command_tx,
        })
    }

    fn initialize_audio_device() -> Result<(cpal::Device, cpal::StreamConfig), PlaybackError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::AudioDevice("No output device found".into()))?;

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(PLAYBACK_BUFFER_SIZE as u32),
        };

        Ok((device, config))
    }

    fn initialize_command_processing(tracks: Tracks) -> Sender<AudioCommand> {
        let (command_tx, command_rx) = unbounded();

        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                let mut tracks = tracks.write();
                match command {
                    AudioCommand::AddTrack { channel, track } => {
                        tracks.retain(|(ch, _)| *ch != channel);
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

        command_tx
    }

    fn create_audio_callback(
        tracks: Tracks,
    ) -> impl FnMut(&mut [f32], &cpal::OutputCallbackInfo) + Send + 'static {
        let mix_buffer = vec![0f32; DECODE_BUFFER_SIZE];
        let mix_buffer = Arc::new(RwLock::new(mix_buffer));
        let mix_buffer_ref = Arc::clone(&mix_buffer);

        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            data.fill(0.0);
            let samples_per_callback = data.len();
            tracing::debug!("Audio callback requesting {} samples", samples_per_callback);

            let tracks = tracks.read();
            let mut mix_buffer = mix_buffer_ref.write();
            mix_buffer.fill(0.0);

            for (channel, track) in tracks.iter() {
                let mut track = track.write();
                if track.is_playing() {
                    match track.get_next_samples(&mut mix_buffer[..samples_per_callback]) {
                        Ok(len) if len > 0 => {
                            tracing::debug!("Got {} samples from channel {:?}", len, channel);
                            let volume = track.get_volume();
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

            for sample in data.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
        }
    }

    fn create_audio_stream(
        device: &Device,
        config: &StreamConfig,
        audio_callback: impl FnMut(&mut [f32], &cpal::OutputCallbackInfo) + Send + 'static,
    ) -> Result<Stream, PlaybackError> {
        let stream = device.build_output_stream(
            config,
            audio_callback,
            move |err| eprintln!("Audio stream error: {}", err),
            None,
        )?;

        stream.play()?;

        Ok(stream)
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
