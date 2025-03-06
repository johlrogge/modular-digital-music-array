use crate::error::PlaybackError;
use parking_lot::{Mutex, RwLock};
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo};
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
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Change seek to take an immutable reference
    fn seek(&self, _position: usize) -> Result<(), PlaybackError> {
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

    loading_task: Option<tokio::task::JoinHandle<()>>,
    seek_mutex: Arc<AsyncMutex<()>>,
}

impl FlacSource {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, PlaybackError> {
        // Create shared resources
        let packets = Arc::new(RwLock::new(Vec::new()));
        let total_samples = Arc::new(AtomicUsize::new(0));
        let loaded_packets = Arc::new(AtomicUsize::new(0));
        let seek_mutex = Arc::new(AsyncMutex::new(()));

        // Initialize the decoder and format reader
        let (format_reader, _decoder, sample_rate, audio_channels) =
            Self::init_decoder(path.as_ref())?;

        let format_reader = Arc::new(Mutex::new(format_reader));

        // Create new instance without the loading task
        let mut source = Self {
            packets: packets.clone(),
            loaded_packets: loaded_packets.clone(),
            total_samples: total_samples.clone(),
            sample_rate,
            audio_channels,
            format_reader: format_reader.clone(),
            loading_task: None,
            seek_mutex: seek_mutex.clone(),
        };

        // Start the background loading task
        let loading_task = source.create_background_loader(path.as_ref())?;

        // Assign the loading task properly
        source.loading_task = Some(loading_task);

        // Wait for initial buffer to fill
        source.wait_for_initial_buffer()?;

        Ok(source)
    }

    fn create_background_loader(
        &self,
        path: &Path,
    ) -> Result<tokio::task::JoinHandle<()>, PlaybackError> {
        // Create a new decoder and format reader for the background task
        let (format_reader, decoder, _, _) = Self::init_decoder(path)?;

        // Create shared references for the background task
        let packets = self.packets.clone();
        let total_samples = self.total_samples.clone();
        let loaded_packets = self.loaded_packets.clone();
        let seek_mutex = self.seek_mutex.clone();
        let sample_rate = self.sample_rate;
        let audio_channels = self.audio_channels;

        // Start background loading
        let loading_task = tokio::spawn(async move {
            // Load at least enough for initial playback
            const INITIAL_BUFFER_SECS: f32 = 0.5;
            let initial_target_samples =
                (INITIAL_BUFFER_SECS * sample_rate as f32 * audio_channels as f32) as usize;

            let mut current_position = 0;
            let mut format_reader = format_reader;
            let mut decoder = decoder;

            // Flag to track if we've loaded the initial buffer
            let mut initial_buffer_filled = false;

            // Background loading loop
            loop {
                // Check if a seek is in progress
                let _guard =
                    tokio::time::timeout(tokio::time::Duration::from_millis(1), seek_mutex.lock())
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

                        let samples = sample_buf.samples().to_vec();
                        let samples_len = samples.len();

                        let decoded_packet = DecodedPacket {
                            samples,
                            position: current_position,
                        };

                        {
                            let mut packets_guard = packets.write();
                            packets_guard.push(decoded_packet);
                        }

                        current_position += samples_len;
                        total_samples.store(current_position, std::sync::atomic::Ordering::Release);
                        loaded_packets.fetch_add(1, std::sync::atomic::Ordering::Release);

                        // Check if we've filled the initial buffer
                        if !initial_buffer_filled && current_position >= initial_target_samples {
                            initial_buffer_filled = true;
                            // We can slow down a bit after the initial fill
                            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                        }
                    }
                    Err(_) => {
                        // Likely end of stream, sleep to avoid busy-wait
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });

        Ok(loading_task)
    }

    fn wait_for_initial_buffer(&self) -> Result<(), PlaybackError> {
        const INITIAL_BUFFER_SECS: f32 = 0.5;
        let target_samples =
            (INITIAL_BUFFER_SECS * self.sample_rate as f32 * self.audio_channels as f32) as usize;

        // Create a timeout for waiting
        let timeout = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();

        // Wait until we have enough samples or timeout
        while self
            .total_samples
            .load(std::sync::atomic::Ordering::Acquire)
            < target_samples
        {
            if start.elapsed() > timeout {
                return Err(PlaybackError::Decoder(
                    "Timeout waiting for initial buffer".into(),
                ));
            }

            // Short sleep to avoid busy waiting
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        Ok(())
    }
    fn init_decoder(
        path: &Path,
    ) -> Result<
        (
            Box<dyn FormatReader>,
            Box<dyn symphonia::core::codecs::Decoder>,
            u32,
            u16,
        ),
        PlaybackError,
    > {
        let mut hint = Hint::new();
        hint.with_extension("flac");

        // Open the file
        let file = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // Probe and get format
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

        // Create decoder
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        Ok((probed.format, decoder, sample_rate, audio_channels))
    }

    pub async fn async_seek(&self, target_position: usize) -> Result<(), PlaybackError> {
        // Acquire seek mutex to prevent background loading during seek
        let _seek_guard = self.seek_mutex.lock().await;

        // Convert sample position to time value
        let time = self.position_to_time(target_position);

        // Perform the seek
        let seeked_to = self.perform_seek(time).await?;

        // Reset the buffer and fill it with new data from the seek position
        self.reset_buffer_after_seek(seeked_to).await?;

        Ok(())
    }

    fn position_to_time(&self, position: usize) -> Time {
        let sample_rate_f64 = self.sample_rate as f64;
        let channels_f64 = self.audio_channels as f64;
        let time_seconds = (position as f64) / (sample_rate_f64 * channels_f64);

        // Convert to Symphonia's Time format
        let seconds = time_seconds.floor() as u64;
        let frac = time_seconds - seconds as f64;
        Time::new(seconds, frac)
    }

    async fn perform_seek(&self, time: Time) -> Result<SeekedTo, PlaybackError> {
        // Define the seek target
        let seek_to = SeekTo::Time {
            time,
            track_id: None, // Use default track
        };

        // Perform the seek
        let mut format_reader = self.format_reader.lock();
        format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| PlaybackError::Decoder(format!("Seek error: {}", e)))
    }

    async fn reset_buffer_after_seek(&self, _seeked_to: SeekedTo) -> Result<(), PlaybackError> {
        // Clear the packet buffer
        {
            let mut packets_guard = self.packets.write();
            packets_guard.clear();
        }

        self.loaded_packets
            .store(0, std::sync::atomic::Ordering::Release);
        self.total_samples
            .store(0, std::sync::atomic::Ordering::Release);

        // Wait for the background loader to fill the buffer
        // The background loader will automatically start loading from the new position
        self.wait_for_initial_buffer()?;

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
