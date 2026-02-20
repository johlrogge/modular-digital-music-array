use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::error::PlaybackError;
use crate::source::SEGMENT_SIZE;

/// Upsamples interleaved stereo audio to a fixed target rate using a sinc resampler.
///
/// Input chunks must be exactly SEGMENT_SIZE interleaved samples.
/// Output size varies based on the resampling ratio.
pub struct Resampler {
    inner: SincFixedIn<f32>,
    channels: usize,
    /// frames per input chunk (= SEGMENT_SIZE / channels)
    chunk_frames: usize,
    input_bufs: Vec<Vec<f32>>,
}

impl Resampler {
    pub fn new(source_rate: u32, target_rate: u32, channels: usize) -> Result<Self, PlaybackError> {
        let chunk_frames = SEGMENT_SIZE / channels;
        let ratio = target_rate as f64 / source_rate as f64;

        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            oversampling_factor: 256,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::BlackmanHarris2,
        };

        let inner = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_frames, channels)
            .map_err(|e| PlaybackError::Resampler(e.to_string()))?;

        tracing::debug!(
            "Resampler created: {}Hz → {}Hz, channels={}, chunk_frames={}",
            source_rate,
            target_rate,
            channels,
            chunk_frames
        );

        Ok(Self {
            inner,
            channels,
            chunk_frames,
            input_bufs: vec![vec![0.0f32; chunk_frames]; channels],
        })
    }

    /// Resample one segment of interleaved samples.
    ///
    /// Input must be exactly SEGMENT_SIZE samples (chunk_frames * channels).
    /// Returns resampled interleaved samples; length depends on the ratio.
    pub fn process_segment(&mut self, interleaved: &[f32]) -> Result<Vec<f32>, PlaybackError> {
        // Deinterleave: LRLRLR... → per-channel buffers
        for (frame_idx, frame) in interleaved
            .chunks(self.channels)
            .enumerate()
            .take(self.chunk_frames)
        {
            for (ch, &sample) in frame.iter().enumerate() {
                self.input_bufs[ch][frame_idx] = sample;
            }
        }

        // Borrow fields separately to satisfy the borrow checker
        let inner = &mut self.inner;
        let input_bufs = &self.input_bufs;

        let out = inner
            .process(input_bufs, None)
            .map_err(|e| PlaybackError::Resampler(e.to_string()))?;

        // Reinterleave: per-channel → LRLRLR...
        let n_out_frames = out[0].len();
        let mut result = Vec::with_capacity(n_out_frames * self.channels);
        for i in 0..n_out_frames {
            for ch in 0..self.channels {
                result.push(out[ch][i]);
            }
        }

        Ok(result)
    }
}
