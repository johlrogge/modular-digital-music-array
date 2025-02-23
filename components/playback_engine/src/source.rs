// In src/source.rs
use crate::error::PlaybackError;
use parking_lot::RwLock;
use std::sync::Arc;
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use vfs::VfsPath;

pub trait Source: Send + Sync {
    fn view_samples(&self, position: usize, len: usize) -> Result<&[f32], PlaybackError>;
    fn sample_rate(&self) -> u32;
    fn audio_channels(&self) -> u16;
    fn len(&self) -> usize;
}

pub struct FlacSource {
    path: VfsPath,
    sample_rate: u32,
    audio_channels: u16,
    total_samples: usize,
    // Internal state for loading/caching - implementation detail
    samples: Arc<RwLock<Vec<f32>>>,
}

impl FlacSource {
    pub fn new(path: VfsPath) -> Result<Self, PlaybackError> {
        // Just open and read metadata
        let mut hint = Hint::new();
        hint.with_extension("flac");

        let file = path.open_file()?;
        let mss = symphonia::core::io::MediaSourceStream::new(Box::new(file), Default::default());

        let probed = symphonia::default::get_probe()
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

        Ok(Self {
            path,
            sample_rate: track.codec_params.sample_rate.unwrap_or(44100),
            audio_channels: track.codec_params.channels.map(|c| c.count()).unwrap_or(2) as u16,
            total_samples: track.codec_params.n_frames.map(|f| f as usize).unwrap_or(0),
            samples: Arc::new(RwLock::new(Vec::new())),
        })
    }
}
