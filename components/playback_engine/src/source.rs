// In src/source.rs
use crate::error::PlaybackError;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub trait Source: Send + Sync {
    fn view_samples(&self, position: usize, len: usize) -> Result<&[f32], PlaybackError>;
    fn sample_rate(&self) -> u32;
    fn audio_channels(&self) -> u16;
    fn len(&self) -> usize;
}

pub struct FlacSource {
    samples: Vec<f32>,
    sample_rate: u32,
    audio_channels: u16,
}

// in components/playback_engine/src/source.rs
impl FlacSource {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, PlaybackError> {
        let mut hint = Hint::new();
        hint.with_extension("flac");

        // Open the file
        let file = std::fs::File::open(&path)?;
        tracing::info!("Opened FLAC file: {:?}", path.as_ref());

        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // Probe and get format reader
        let mut probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        tracing::info!("Probed audio format successfully");

        // Get the default track
        let track = probed
            .format
            .default_track()
            .ok_or_else(|| PlaybackError::Decoder("No default track found".into()))?;

        // Get track info
        let audio_channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2) as u16;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);

        tracing::info!(
            "Track info - channels: {}, sample rate: {}",
            audio_channels,
            sample_rate
        );

        // Create decoder
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        // Decode all samples
        let mut samples = Vec::new();
        let mut frame_count = 0;
        while let Ok(packet) = probed.format.next_packet() {
            let decoded = decoder
                .decode(&packet)
                .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

            let mut sample_buf = SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
            sample_buf.copy_interleaved_ref(decoded);
            samples.extend_from_slice(sample_buf.samples());
            frame_count += 1;
        }

        tracing::info!(
            "Decoded {} frames, total samples: {}",
            frame_count,
            samples.len()
        );

        // Verify we have some non-zero samples
        let non_zero = samples.iter().filter(|&&s| s.abs() > 1e-6).count();
        tracing::info!(
            "Non-zero samples: {} ({:.2}%)",
            non_zero,
            100.0 * non_zero as f32 / samples.len() as f32
        );

        Ok(Self {
            samples,
            sample_rate,
            audio_channels,
        })
    }
}

impl Source for FlacSource {
    fn view_samples(&self, position: usize, len: usize) -> Result<&[f32], PlaybackError> {
        if position >= self.samples.len() {
            tracing::info!("End of track reached at position {}", position);
            return Ok(&[]);
        }
        let end = position.saturating_add(len).min(self.samples.len());
        let samples = &self.samples[position..end];

        // Log periodically about the samples we're returning
        if position % 48000 == 0 {
            // Log roughly every second
            let non_zero = samples.iter().filter(|&&s| s.abs() > 1e-6).count();
            tracing::info!(
                "Returning {} samples starting at position {}, non-zero: {}",
                samples.len(),
                position,
                non_zero
            );
        }

        Ok(samples)
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn audio_channels(&self) -> u16 {
        self.audio_channels
    }

    fn len(&self) -> usize {
        self.samples.len()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;
    use std::process::Command;

    fn generate_test_signal(duration_secs: f32, sample_rate: u32, audio_channels: u16) -> Vec<f32> {
        let num_samples = (duration_secs * sample_rate as f32) as usize * audio_channels as usize;
        let mut samples = Vec::with_capacity(num_samples);

        // Generate a mix of frequencies (A4, A5, A6)
        let frequencies = [440.0, 880.0, 1760.0];

        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let sample = frequencies
                .iter()
                .enumerate()
                .map(|(idx, &freq)| {
                    let amplitude = 0.25 / (idx + 1) as f32;
                    amplitude * (2.0 * PI * freq * t).sin()
                })
                .sum::<f32>();

            samples.push(sample);
        }

        samples
    }

    fn create_test_flac() -> Result<tempfile::TempPath, std::io::Error> {
        // First create a WAV file
        let wav_file = tempfile::NamedTempFile::new()?;

        // Create FLAC file with extension
        let flac_file = tempfile::Builder::new().suffix(".flac").tempfile()?;

        // Generate test audio data
        let sample_rate = 48000;
        let audio_channels = 2;
        let samples = generate_test_signal(0.1, sample_rate, audio_channels); // 100ms of audio

        // Write WAV
        let spec = hound::WavSpec {
            channels: audio_channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let mut writer = hound::WavWriter::create(wav_file.path(), spec).unwrap();
        for sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();

        // Convert WAV to FLAC using ffmpeg
        let status = Command::new("ffmpeg")
            .arg("-y") // Overwrite output files
            .arg("-i")
            .arg(wav_file.path())
            .arg("-c:a")
            .arg("flac")
            .arg(flac_file.path())
            .status()?;

        if !status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "ffmpeg conversion failed",
            ));
        }

        Ok(flac_file.into_temp_path())
    }

    #[test]
    fn test_flac_metadata() -> Result<(), PlaybackError> {
        let path = create_test_flac()?;
        let source = FlacSource::new(path)?;

        assert_eq!(source.sample_rate(), 48000);
        assert_eq!(source.audio_channels(), 2);
        assert!(source.len() > 0);

        Ok(())
    }
}
