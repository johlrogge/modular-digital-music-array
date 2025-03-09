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
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError>;
    fn sample_rate(&self) -> u32;
    fn audio_channels(&self) -> u16;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Change seek to take an immutable reference
    async fn seek(&self, _position: usize) -> Result<(), PlaybackError> {
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
    // Keep these fields
    packets: Arc<RwLock<Vec<DecodedPacket>>>,
    total_samples: Arc<AtomicUsize>,
    sample_rate: u32,
    audio_channels: u16,
    format_reader: Arc<Mutex<Box<dyn FormatReader>>>,
}

type DecoderResult = Result<
    (
        Box<dyn FormatReader>,
        Box<dyn symphonia::core::codecs::Decoder>,
        u32,
        u16,
    ),
    PlaybackError,
>;

impl FlacSource {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, PlaybackError> {
        // Initialize the decoder and format reader
        let (format_reader, _decoder, sample_rate, audio_channels) =
            Self::init_decoder(path.as_ref())?;

        let format_reader = Arc::new(Mutex::new(format_reader));
        let packets = Arc::new(RwLock::new(Vec::new()));
        let total_samples = Arc::new(AtomicUsize::new(0));

        // Create new instance without the loading task
        let source = Self {
            packets,
            total_samples,
            sample_rate,
            audio_channels,
            format_reader,
        };

        // Decode a few initial packets to have some data ready
        for _ in 0..3 {
            // Decode just a few packets for initial buffer
            source.decode_next_packet()?;
        }

        Ok(source)
    }

    // Add a method to decode the next packet on demand
    fn decode_next_packet(&self) -> Result<(), PlaybackError> {
        let mut format_reader = self.format_reader.lock();

        // Try to get the next packet
        let packet = match format_reader.next_packet() {
            Ok(packet) => packet,
            Err(e) => {
                return Err(PlaybackError::Decoder(format!(
                    "Error getting next packet: {}",
                    e
                )))
            }
        };

        // Create a decoder for this packet
        let track = format_reader
            .default_track()
            .ok_or_else(|| PlaybackError::Decoder("No default track found".into()))?;

        let mut decoder = symphonia::default::get_codecs()
            .make(
                &track.codec_params,
                &symphonia::core::codecs::DecoderOptions::default(),
            )
            .map_err(|e| PlaybackError::Decoder(e.to_string()))?;

        // Decode the packet
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(e) => {
                return Err(PlaybackError::Decoder(format!(
                    "Error decoding packet: {}",
                    e
                )))
            }
        };

        // Convert to interleaved f32 samples
        let mut sample_buf = SampleBuffer::<f32>::new(decoded.frames() as u64, *decoded.spec());
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples().to_vec();

        // Get the current position for this packet
        let position = self
            .total_samples
            .load(std::sync::atomic::Ordering::Relaxed);

        // Create a decoded packet
        let decoded_packet = DecodedPacket {
            samples: samples.clone(),
            position,
        };

        // Update total samples
        self.total_samples
            .fetch_add(samples.len(), std::sync::atomic::Ordering::Release);

        // Store the decoded packet
        {
            let mut packets_guard = self.packets.write();
            packets_guard.push(decoded_packet);
        }

        Ok(())
    }

    fn init_decoder(path: &Path) -> DecoderResult {
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

    fn sync_seek(&self, position: usize) -> Result<(), PlaybackError> {
        // We need to create a synchronous version of the seek functionality
        // This will likely involve:

        // 1. Acquire a lock on format_reader
        let mut format_reader = self.format_reader.lock();

        // 2. Convert sample position to time value
        let time = self.position_to_time(position);

        // 3. Define seek target
        let seek_to = symphonia::core::formats::SeekTo::Time {
            time,
            track_id: None, // Use default track
        };

        // 4. Perform the seek
        format_reader
            .seek(symphonia::core::formats::SeekMode::Accurate, seek_to)
            .map_err(|e| PlaybackError::Decoder(format!("Seek error: {}", e)))?;

        // 5. Clear the packet buffer
        {
            let mut packets_guard = self.packets.write();
            packets_guard.clear();
        }

        self.total_samples
            .store(0, std::sync::atomic::Ordering::Release);

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
}

impl Source for FlacSource {
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        let total = self
            .total_samples
            .load(std::sync::atomic::Ordering::Relaxed);
        if position >= total {
            // Try to decode more data if we're at the end of what we've already decoded
            self.decode_next_packet()?;

            // Check again after decoding
            let total = self
                .total_samples
                .load(std::sync::atomic::Ordering::Relaxed);
            if position >= total {
                return Ok(0); // Still at the end, no more data
            }
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
            None => {
                // No packet found, try to decode more
                drop(packets_guard); // Drop the read lock before attempting to decode
                self.decode_next_packet()?;

                // Retry after decoding
                return self.read_samples(position, buffer);
            }
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

    async fn seek(&self, position: usize) -> Result<(), PlaybackError> {
        self.sync_seek(position)
    }
}
