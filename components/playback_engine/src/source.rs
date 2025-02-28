use crate::error::PlaybackError;
use parking_lot::{Mutex, RwLock};
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use tokio::sync::Mutex as AsyncMutex;

pub trait Source: Send + Sync {
    // Existing methods...
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError>;
    fn sample_rate(&self) -> u32;
    fn audio_channels(&self) -> u16;
    fn len(&self) -> usize;

    // Change seek to take an immutable reference
    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // Default implementation (no-op)
        Ok(())
    }
}

/// Represents a decoded audio packet with its position information
struct DecodedPacket {
    samples: Vec<f32>,
    position: usize, // Sample position in the overall stream
}

pub struct FlacSource {
    // Buffer management
    packets: Arc<RwLock<Vec<DecodedPacket>>>,
    loaded_packets: Arc<AtomicUsize>,
    total_samples: Arc<AtomicUsize>,
    sample_rate: u32,
    audio_channels: u16,

    // Change these to use Mutex for interior mutability
    format_reader: Arc<Mutex<Box<dyn FormatReader>>>,
    decoder: Arc<Mutex<Box<dyn symphonia::core::codecs::Decoder>>>,

    loading_task: Option<tokio::task::JoinHandle<()>>,
    seek_mutex: Arc<AsyncMutex<()>>,
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

        // Create decoder for initial loading
        let decoder_init = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        // Create shared resources
        let packets = Arc::new(RwLock::new(Vec::new()));
        let total_samples = Arc::new(AtomicUsize::new(0));
        let loaded_packets = Arc::new(AtomicUsize::new(0));
        let format_reader = Arc::new(Mutex::new(probed.format));
        let decoder = Arc::new(Mutex::new(decoder_init));
        let seek_mutex = Arc::new(AsyncMutex::new(()));

        let mut source = Self {
            packets: packets.clone(),
            loaded_packets: loaded_packets.clone(),
            sample_rate,
            audio_channels,
            loading_task: None,
            total_samples: total_samples.clone(),
            format_reader: format_reader.clone(),
            decoder: decoder.clone(),
            seek_mutex: seek_mutex.clone(),
        };

        // Load initial packets
        source.load_initial_packets()?;

        // Create a new file handle for background loading
        let file_bg = std::fs::File::open(&path)?;
        let mss_bg = MediaSourceStream::new(Box::new(file_bg), Default::default());
        let format_bg = symphonia::default::get_probe()
            .format(
                &hint,
                mss_bg,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?
            .format;

        // Create decoder for background loading
        let decoder_bg = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        // Shared resources for background loading
        let packets_bg = packets.clone();
        let total_samples_bg = total_samples.clone();
        let loaded_packets_bg = loaded_packets.clone();
        let seek_mutex_bg = seek_mutex.clone();

        // Start background loading
        let loading_task = tokio::spawn(async move {
            // Skip the packets we already loaded
            let loaded = loaded_packets_bg.load(std::sync::atomic::Ordering::Acquire);

            // Initialize background format reader and decoder
            let mut format_reader = format_bg;
            let mut decoder = decoder_bg;

            // Skip to where initial loading left off
            for _ in 0..loaded {
                let _ = format_reader.next_packet();
            }

            // Background loading loop
            loop {
                // Check if a seek is in progress
                let _guard = tokio::time::timeout(
                    tokio::time::Duration::from_millis(1),
                    seek_mutex_bg.lock(),
                )
                .await;

                if _guard.is_err() {
                    // A seek is in progress, wait and retry
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                }

                // Process next packet
                match format_reader.next_packet() {
                    Ok(packet) => {
                        let decoded = match decoder.decode(&packet) {
                            Ok(decoded) => decoded,
                            Err(_) => continue,
                        };

                        let mut sample_buf =
                            SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
                        sample_buf.copy_interleaved_ref(decoded);

                        let current_total =
                            total_samples_bg.load(std::sync::atomic::Ordering::Relaxed);
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
                        loaded_packets_bg.fetch_add(1, std::sync::atomic::Ordering::Release);
                    }
                    Err(_) => {
                        // Likely end of stream, sleep to avoid busy-wait
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });

        source.loading_task = Some(loading_task);
        Ok(source)
    }

    fn load_initial_packets(&mut self) -> Result<(), PlaybackError> {
        const INITIAL_BUFFER_SECS: f32 = 0.5;
        let target_samples =
            (INITIAL_BUFFER_SECS * self.sample_rate as f32 * self.audio_channels as f32) as usize;
        let mut current_samples = 0;

        let mut format_reader = self.format_reader.lock();
        let mut decoder = self.decoder.lock();

        while current_samples < target_samples {
            if let Ok(packet) = format_reader.next_packet() {
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

    pub async fn async_seek(&self, target_position: usize) -> Result<(), PlaybackError> {
        // Acquire seek mutex to prevent background loading during seek
        let _seek_guard = self.seek_mutex.lock().await;

        // Convert sample position to time value
        let sample_rate_f64 = self.sample_rate as f64;
        let channels_f64 = self.audio_channels as f64;
        let time_seconds = (target_position as f64) / (sample_rate_f64 * channels_f64);

        // Convert to Symphonia's Time format
        let seconds = time_seconds.floor() as u64;
        let frac = time_seconds - seconds as f64;
        let time = Time::new(seconds, frac);

        // Define the seek target
        let seek_to = SeekTo::Time {
            time,
            track_id: None, // Use default track
        };

        // Perform the seek - note the argument order (mode, to)
        let mut format_reader = self.format_reader.lock();
        let seek_result = format_reader.seek(SeekMode::Accurate, seek_to);

        // Check if seek was successful
        if let Err(e) = seek_result {
            return Err(PlaybackError::Decoder(format!("Seek error: {}", e)));
        }

        // Reset the packet buffer
        {
            let mut packets_guard = self.packets.write();
            packets_guard.clear();
        }

        self.loaded_packets
            .store(0, std::sync::atomic::Ordering::Release);
        self.total_samples
            .store(0, std::sync::atomic::Ordering::Release);

        // Process packets until we reach or exceed the target position
        let mut current_position = 0;
        let mut decoder = self.decoder.lock();

        // Decode packets until we get past the target position
        while current_position <= target_position {
            if let Ok(packet) = format_reader.next_packet() {
                let decoded = match decoder.decode(&packet) {
                    Ok(decoded) => decoded,
                    Err(e) => {
                        return Err(PlaybackError::Decoder(format!(
                            "Decode error during seek: {}",
                            e
                        )))
                    }
                };

                let mut sample_buf =
                    SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
                sample_buf.copy_interleaved_ref(decoded);

                let samples = sample_buf.samples().to_vec();
                let samples_len = samples.len();

                let decoded_packet = DecodedPacket {
                    samples,
                    position: current_position,
                };

                // Add to buffer
                {
                    let mut packets_guard = self.packets.write();
                    packets_guard.push(decoded_packet);
                }

                current_position += samples_len;
                self.total_samples
                    .store(current_position, std::sync::atomic::Ordering::Release);
                self.loaded_packets
                    .fetch_add(1, std::sync::atomic::Ordering::Release);

                // If we've loaded enough samples past the target position, break
                if current_position
                    > target_position + (self.sample_rate as usize * self.audio_channels as usize)
                {
                    break;
                }
            } else {
                // End of stream reached
                if current_position < target_position {
                    return Err(PlaybackError::Decoder(format!(
                        "Seek position {} beyond end of stream ({})",
                        target_position, current_position
                    )));
                }
                break;
            }
        }

        // Successfully seeked
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

    // Implement the trait's seek method to call our async implementation
    fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        // Create a runtime to block on the async seek
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| PlaybackError::Decoder(format!("Failed to create runtime: {}", e)))?;

        // Block on the async seek
        runtime.block_on(self.async_seek(position))
    }
}

impl Drop for FlacSource {
    fn drop(&mut self) {
        // Cancel the background loading task if it exists
        if let Some(task) = self.loading_task.take() {
            task.abort();
        }
    }
}
