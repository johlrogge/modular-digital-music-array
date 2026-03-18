use thiserror::Error;

#[derive(Error, Debug)]
pub enum AudioOutputError {
    #[error("Audio device error: {0}")]
    AudioDevice(String),
}

pub mod pipewire_devices;
pub mod pipewire_output;

pub use pipewire_devices::AudioSink;
pub use pipewire_output::PipewireOutput;
