use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use crossbeam::channel::{unbounded, Sender};
use std::sync::Arc;

use crate::channels::Channels;
use crate::commands::AudioCommand;
use crate::error::PlaybackError;
use crate::mixer::Mixer;
use crate::track::Track;
use playback_primitives::Deck;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2; // Stereo
const PLAYBACK_BUFFER_SIZE: usize = 480; // 10ms at 48kHz

pub struct AudioOutput {
    _stream: Stream,
    _device: Device,
    channels: Channels,
    command_tx: Sender<AudioCommand>,
}

impl AudioOutput {
    pub fn new() -> Result<Self, PlaybackError> {
        let (device, config) = Self::initialize_audio_device()?;

        let channels = Channels::new();
        let command_tx = Self::initialize_command_processing(channels.clone());

        let mixer = Arc::new(parking_lot::RwLock::new(Mixer::new(PLAYBACK_BUFFER_SIZE)));
        let audio_callback = Self::create_audio_callback(channels.clone(), mixer);

        let stream = Self::create_audio_stream(&device, &config, audio_callback)?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            _device: device,
            channels,
            command_tx,
        })
    }

    fn initialize_audio_device() -> Result<(cpal::Device, cpal::StreamConfig), PlaybackError> {
        let host = cpal::default_host();
        tracing::info!("Audio host: {}", host.id().name());

        let device = host
            .default_output_device()
            .ok_or_else(|| PlaybackError::AudioDevice("No output device found".into()))?;
        tracing::info!("Output device: {}", device.name().unwrap_or_default());

        let config = cpal::StreamConfig {
            channels: CHANNELS,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(PLAYBACK_BUFFER_SIZE as u32),
        };
        tracing::info!(
            "Audio config: {}Hz, {} channels, buffer size: {}",
            SAMPLE_RATE,
            CHANNELS,
            PLAYBACK_BUFFER_SIZE
        );

        Ok((device, config))
    }

    fn create_audio_callback(
        channels: Channels, // This takes ownership of a clone
        mixer: Arc<parking_lot::RwLock<Mixer>>,
    ) -> impl FnMut(&mut [f32], &cpal::OutputCallbackInfo) + Send + 'static {
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let samples_per_callback = data.len();
            let mut mixer = mixer.write();

            if let Err(e) = mixer.mix(&channels, data, samples_per_callback) {
                tracing::error!("Error in audio callback: {}", e);
            }

            // Check for actual audio data periodically
            static LAST_CHECK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            if now > LAST_CHECK.load(std::sync::atomic::Ordering::Relaxed) {
                let has_audio = data.iter().any(|&s| s.abs() > 1e-6);
                tracing::info!(
                    "Audio callback status - Buffer size: {}, Has audio: {}",
                    samples_per_callback,
                    has_audio
                );
                LAST_CHECK.store(now, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    fn initialize_command_processing(channels: Channels) -> Sender<AudioCommand> {
        let (command_tx, command_rx) = unbounded();

        std::thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                match command {
                    AudioCommand::AddTrack { channel, track } => {
                        tracing::info!("Adding track to channel {:?}", channel);
                        channels.assign(channel, track);
                    }
                    AudioCommand::RemoveTrack(channel) => {
                        tracing::info!("Removing track from channel {:?}", channel);
                        channels.clear(channel);
                    }
                }
            }
            tracing::info!("Command processing stopped");
        });

        command_tx
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

        Ok(stream)
    }

    pub fn add_track(&self, channel: Deck, track: Track) -> Result<(), PlaybackError> {
        self.command_tx
            .send(AudioCommand::add_track(channel, track))
            .map_err(|_| PlaybackError::AudioDevice("Failed to send add track command".into()))
    }

    pub fn remove_track(&self, channel: Deck) -> Result<(), PlaybackError> {
        self.command_tx
            .send(AudioCommand::remove_track(channel))
            .map_err(|_| PlaybackError::AudioDevice("Failed to send remove track command".into()))
    }

    pub fn channels(&self) -> &Channels {
        &self.channels
    }
}
