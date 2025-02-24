// In src/source.rs
use crate::error::PlaybackError;
use std::path::Path;
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub trait Source: Send + Sync {
    // Change to copy samples into provided buffer
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError>;
    fn sample_rate(&self) -> u32;
    fn audio_channels(&self) -> u16;
    fn len(&self) -> usize;
}

/// Represents a decoded audio packet with its position information
struct DecodedPacket {
    samples: Vec<f32>,
    position: usize, // Sample position in the overall stream
}

pub struct FlacSource {
    packets: Arc<parking_lot::RwLock<Vec<DecodedPacket>>>,
    loaded_packets: Arc<std::sync::atomic::AtomicUsize>,
    sample_rate: u32,
    audio_channels: u16,
    loading_task: Option<tokio::task::JoinHandle<()>>,
    total_samples: Arc<std::sync::atomic::AtomicUsize>,
}

impl FlacSource {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, PlaybackError> {
        let mut hint = Hint::new();
        hint.with_extension("flac");

        // Get track info from initial probe
        let file = std::fs::File::open(&path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
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

        let audio_channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2) as u16;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
        let codec_params = track.codec_params.clone();

        let packets = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let total_samples = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loaded_packets = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut source = Self {
            packets: packets.clone(),
            loaded_packets: loaded_packets.clone(),
            sample_rate,
            audio_channels,
            loading_task: None,
            total_samples: total_samples.clone(),
        };

        // Load initial packets with first format reader
        let decoder_init = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;
        source.load_initial_packets(probed.format, decoder_init)?;

        // Create new format reader for background loading
        let file_bg = std::fs::File::open(&path)?;
        let mss_bg = MediaSourceStream::new(Box::new(file_bg), Default::default());
        let mut format_bg = symphonia::default::get_probe()
            .format(
                &hint,
                mss_bg,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?
            .format;

        let mut decoder_bg = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        let packets_bg = packets;
        let total_samples_bg = total_samples;
        let loading_task = tokio::spawn(async move {
            // Skip the packets we already loaded
            let loaded = loaded_packets.load(std::sync::atomic::Ordering::Acquire);
            for _ in 0..loaded {
                let _ = format_bg.next_packet();
            }

            while let Ok(packet) = format_bg.next_packet() {
                let decoded = match decoder_bg.decode(&packet) {
                    Ok(decoded) => decoded,
                    Err(_) => continue,
                };

                let mut sample_buf =
                    SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
                sample_buf.copy_interleaved_ref(decoded);

                let current_total = total_samples_bg.load(std::sync::atomic::Ordering::Relaxed);
                let decoded_packet = DecodedPacket {
                    samples: sample_buf.samples().to_vec(),
                    position: current_total,
                };

                let new_total = current_total + decoded_packet.samples.len();
                total_samples_bg.store(new_total, std::sync::atomic::Ordering::Release);

                {
                    let mut packets_guard = packets_bg.write();
                    packets_guard.push(decoded_packet);
                }
                loaded_packets.fetch_add(1, std::sync::atomic::Ordering::Release);
            }
        });

        source.loading_task = Some(loading_task);
        Ok(source)
    }

    fn load_initial_packets(
        &mut self,
        mut format: Box<dyn symphonia::core::formats::FormatReader>,
        mut decoder: Box<dyn symphonia::core::codecs::Decoder>,
    ) -> Result<(), PlaybackError> {
        const INITIAL_BUFFER_SECS: f32 = 0.5;
        let target_samples = (INITIAL_BUFFER_SECS * self.sample_rate as f32) as usize;
        let mut current_samples = 0;

        while current_samples < target_samples {
            if let Ok(packet) = format.next_packet() {
                let decoded = decoder
                    .decode(&packet)
                    .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

                let mut sample_buf =
                    SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
                sample_buf.copy_interleaved_ref(decoded);

                let decoded_packet = DecodedPacket {
                    samples: sample_buf.samples().to_vec(),
                    position: current_samples,
                };

                current_samples += decoded_packet.samples.len();
                self.total_samples
                    .store(current_samples, std::sync::atomic::Ordering::Release);

                let mut packets_guard = self.packets.write();
                packets_guard.push(decoded_packet);
                self.loaded_packets
                    .fetch_add(1, std::sync::atomic::Ordering::Release);
            } else {
                break; // End of stream
            }
        }

        Ok(())
    }
}

impl Source for FlacSource {
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        let total = self
            .total_samples
            .load(std::sync::atomic::Ordering::Relaxed);
        if position >= total {
            return Ok(0);
        }

        let packets_guard = self.packets.read();

        // Find the packet containing our position
        let mut packet_idx = None;
        for (idx, packet) in packets_guard.iter().enumerate() {
            if packet.position + packet.samples.len() > position {
                packet_idx = Some(idx);
                break;
            }
        }

        let packet_idx = match packet_idx {
            Some(idx) => idx,
            None => return Ok(0),
        };

        // Get samples from the packet
        let packet = &packets_guard[packet_idx];
        let packet_pos = position - packet.position;
        let available = packet.samples.len() - packet_pos;
        let sample_count = buffer.len().min(available);

        // Copy samples to the provided buffer
        buffer[..sample_count]
            .copy_from_slice(&packet.samples[packet_pos..packet_pos + sample_count]);

        Ok(sample_count)
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn audio_channels(&self) -> u16 {
        self.audio_channels
    }

    fn len(&self) -> usize {
        self.total_samples
            .load(std::sync::atomic::Ordering::Relaxed)
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

    #[tokio::test]
    async fn test_flac_metadata() -> Result<(), PlaybackError> {
        let path = create_test_flac()?;
        let source = FlacSource::new(path)?;

        assert_eq!(source.sample_rate(), 48000);
        assert_eq!(source.audio_channels(), 2);
        assert!(source.len() > 0);

        Ok(())
    }
    #[tokio::test]
    async fn test_packet_reading() -> Result<(), PlaybackError> {
        let path = create_test_flac()?;
        let source = FlacSource::new(path)?;
        let mut buffer = vec![0.0f32; 1024];

        // Test reading from start
        let read = source.read_samples(0, &mut buffer)?;
        assert!(read > 0);

        // Test reading across packet boundary
        let total = source
            .total_samples
            .load(std::sync::atomic::Ordering::Relaxed);
        let packets = source.packets.read();
        let first_packet_len = packets[0].samples.len();
        drop(packets); // Release the lock before next read

        let mut cross_buffer = vec![0.0f32; 20];
        let read = source.read_samples(first_packet_len - 10, &mut cross_buffer)?;
        assert!(read > 0);

        // Test reading beyond end
        let mut end_buffer = vec![0.0f32; 1024];
        let read = source.read_samples(total, &mut end_buffer)?;
        assert_eq!(read, 0);

        Ok(())
    }
}
