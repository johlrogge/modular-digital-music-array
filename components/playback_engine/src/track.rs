use crate::error::PlaybackError;
use audio_decoder::Source;
use audio_resampler::Resampler;
#[cfg(test)]
use audio_types::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};

use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc;

use std::sync::Arc;

use ringbuf::HeapProducer;
#[cfg(test)]
use ringbuf::HeapRb;

/// Playback state for a track.
///
/// Represented as `AtomicU8` so it can be shared safely across threads without a mutex.
/// Two booleans (`playing`, `finished`) would create 4 combinations of which only 3 are
/// valid — the enum makes illegal states unrepresentable.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackState {
    Stopped = 0,
    Playing = 1,
    Finished = 2,
}

impl TrackState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Playing,
            2 => Self::Finished,
            _ => Self::Stopped,
        }
    }
}

pub struct Track {
    state: Arc<AtomicU8>,
    command_tx: mpsc::SyncSender<TrackCommand>,
    decoder_thread: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    /// Current playback position in milliseconds (updated by decoder thread).
    position_ms: Arc<AtomicU64>,
    /// Total track duration in milliseconds (0 if unknown).
    duration_ms: u64,
}

pub enum TrackCommand {
    FillFrom(usize),
    Shutdown,
}

/// Threshold: decode ahead until we have this many resampled samples pending.
/// At 192 kHz stereo this is ~170 ms of audio.
const DECODE_AHEAD: usize = 65_536;

#[allow(clippy::too_many_arguments)]
fn decoder_thread_fn<S: Source + Send + Sync + 'static>(
    source: S,
    mut output: HeapProducer<f32>,
    command_rx: mpsc::Receiver<TrackCommand>,
    state: Arc<AtomicU8>,
    source_rate: u32,
    target_rate: u32,
    position_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let channels = source.audio_channels() as usize;

    let mut resampler: Option<Resampler> = if source_rate != target_rate {
        match Resampler::new(source_rate, target_rate, channels) {
            Ok(r) => {
                tracing::info!(
                    "Resampling {}Hz → {}Hz ({}ch)",
                    source_rate,
                    target_rate,
                    channels
                );
                Some(r)
            }
            Err(e) => {
                tracing::error!("Failed to create resampler, playing at source rate: {e}");
                None
            }
        }
    } else {
        tracing::info!(
            "Source rate matches target ({}Hz), no resampling needed",
            source_rate
        );
        None
    };

    // Resampled samples waiting to be written to the ring buffer
    let mut pending: VecDeque<f32> = VecDeque::new();
    let mut eof = false;

    loop {
        // Check stop flag first — set by Drop before join
        if stop.load(Ordering::Acquire) {
            tracing::info!("Decoder thread stopping (stop flag set)");
            return;
        }

        // Handle commands (seek, shutdown) — checked every iteration
        while let Ok(command) = command_rx.try_recv() {
            match command {
                TrackCommand::FillFrom(position) => {
                    tracing::debug!("Seek to sample {position}");
                    if let Err(e) = source.seek(position) {
                        tracing::error!("Seek error: {e}");
                    }
                    pending.clear();
                    eof = false;
                    // Update position_ms after seek.
                    // .max(1) upholds the invariant that audio_channels() >= 1, avoiding
                    // division by zero if a malformed stream reports 0 channels.
                    let ch = source.audio_channels().max(1) as u64;
                    let frames = position as u64 / ch;
                    let ms = frames * 1000 / source_rate as u64;
                    position_ms.store(ms, Ordering::Release);
                    // Seek resets to Stopped — caller must call play() again if desired
                    state.store(TrackState::Stopped as u8, Ordering::Release);
                    // Reset resampler state by recreating it
                    if source_rate != target_rate {
                        resampler = Resampler::new(source_rate, target_rate, channels).ok();
                    }
                }
                TrackCommand::Shutdown => {
                    tracing::info!("Decoder thread shutting down");
                    return;
                }
            }
        }

        // Decode more audio when the pending buffer runs low
        if !eof && pending.len() < DECODE_AHEAD {
            match source.decode_next_frame() {
                Ok(segments) if segments.is_empty() => {
                    tracing::debug!("Decoder reached EOF");
                    eof = true;
                }
                Ok(segments) => {
                    for seg in segments {
                        let valid = &seg.segment.samples[..seg.valid_samples];
                        let samples = if let Some(ref mut r) = resampler {
                            match r.process_segment(valid) {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!("Resampler error: {e}");
                                    continue;
                                }
                            }
                        } else {
                            valid.to_vec()
                        };
                        pending.extend(samples);
                    }
                    // Update position_ms from the source's current sample position.
                    // current_position() counts all interleaved samples; divide by channels to get frames.
                    // .max(1) upholds the invariant that audio_channels() >= 1, avoiding
                    // division by zero if a malformed stream reports 0 channels.
                    let ch = source.audio_channels().max(1) as u64;
                    let sample_pos = source.current_position() as u64;
                    let frames = sample_pos / ch;
                    let ms = frames * 1000 / source_rate as u64;
                    position_ms.store(ms, Ordering::Release);
                }
                Err(e) => {
                    tracing::error!("Decode error: {e}");
                }
            }
        }

        match TrackState::from_u8(state.load(Ordering::Acquire)) {
            TrackState::Stopped => {
                // Paused: keep decoding ahead for instant resume, but don't write
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            TrackState::Playing if !pending.is_empty() => {
                // Write as much as the ring buffer will accept
                let (front, _) = pending.as_slices();
                let written = output.push_slice(front);
                pending.drain(..written);

                if written == 0 {
                    // Ring buffer full — yield so the mixer can consume
                    std::thread::sleep(std::time::Duration::from_micros(500));
                }
            }
            TrackState::Playing if eof => {
                // All decoded samples written — track is done
                state.store(TrackState::Finished as u8, Ordering::Release);
                tracing::info!("Track finished (EOF + pending drained)");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            TrackState::Playing => {
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
            TrackState::Finished => {
                // Nothing to do — wait for a seek command to reset or shutdown
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

impl Track {
    pub fn new<S: Source + Send + Sync + 'static>(
        source: S,
        output_producer: HeapProducer<f32>,
        source_rate: u32,
        target_rate: u32,
    ) -> Result<Self, PlaybackError> {
        let state = Arc::new(AtomicU8::new(TrackState::Stopped as u8));
        let position_ms = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let duration_ms = source.duration_ms().unwrap_or(0);

        let (command_tx, command_rx) = mpsc::sync_channel(32);

        let state_clone = state.clone();
        let position_ms_clone = position_ms.clone();
        let stop_clone = stop.clone();
        let decoder_thread = std::thread::spawn(move || {
            decoder_thread_fn(
                source,
                output_producer,
                command_rx,
                state_clone,
                source_rate,
                target_rate,
                position_ms_clone,
                stop_clone,
            );
        });

        Ok(Self {
            state,
            command_tx,
            decoder_thread: Some(decoder_thread),
            stop,
            position_ms,
            duration_ms,
        })
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        if let Err(e) = self.command_tx.try_send(TrackCommand::FillFrom(position)) {
            tracing::error!("Failed to send fill command after seek: {}", e);
        }
        Ok(())
    }

    pub fn play(&mut self) {
        self.state
            .store(TrackState::Playing as u8, Ordering::Release);
        tracing::debug!("Track set to playing state");
    }

    pub fn stop(&mut self) {
        self.state
            .store(TrackState::Stopped as u8, Ordering::Release);
    }

    pub fn is_playing(&self) -> bool {
        TrackState::from_u8(self.state.load(Ordering::Acquire)) == TrackState::Playing
    }

    pub fn is_finished(&self) -> bool {
        TrackState::from_u8(self.state.load(Ordering::Acquire)) == TrackState::Finished
    }

    /// Current playback position in milliseconds (from decoder progress).
    pub fn position_ms(&self) -> u64 {
        self.position_ms.load(Ordering::Acquire)
    }

    /// Total track duration in milliseconds (0 if unknown).
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        tracing::info!("Track drop beginning");

        // 1. Set stop flag so the loop exits even if the command channel is full
        self.stop.store(true, Ordering::Release);

        // 2. Best-effort shutdown command — ignore channel-full errors
        let _ = self.command_tx.try_send(TrackCommand::Shutdown);
        tracing::info!("Stop flag set and shutdown command attempted");

        // 3. Join the decoder thread — deterministic shutdown
        if let Some(handle) = self.decoder_thread.take() {
            tracing::info!("Joining decoder thread");
            handle.join().ok();
        }

        tracing::info!("Track drop completed");
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub struct TestSource {
    position: AtomicUsize, // Track which frame we're on
    samples: Vec<Vec<DecodedSegment>>,
    current_sample_position: AtomicUsize, // Track current sample position
}

#[cfg(test)]
#[allow(dead_code)]
impl TestSource {
    pub fn new_from_samples(samples: Vec<f32>) -> Self {
        let segments = Self::create_segments_from_samples(samples);
        Self {
            position: AtomicUsize::new(0),
            samples: vec![segments], // Wrap in vector to simulate frames
            current_sample_position: AtomicUsize::new(0), // Initialize to 0
        }
    }

    // Create decoded segments from a flat vector of samples
    fn create_segments_from_samples(samples: Vec<f32>) -> Vec<DecodedSegment> {
        let mut segments = Vec::new();
        let mut start_pos = 0;

        // Process complete segments (of SEGMENT_SIZE)
        for chunk_idx in 0..(samples.len() / SEGMENT_SIZE) {
            let segment_index = SegmentIndex::from_sample_position(start_pos);

            // Create segment data
            let mut segment_samples = [0.0; SEGMENT_SIZE];
            let start = chunk_idx * SEGMENT_SIZE;
            let end = start + SEGMENT_SIZE;

            segment_samples.copy_from_slice(&samples[start..end]);

            // Add segment
            segments.push(DecodedSegment {
                index: segment_index,
                segment: AudioSegment {
                    samples: segment_samples,
                },
                valid_samples: SEGMENT_SIZE,
            });

            start_pos += SEGMENT_SIZE;
        }

        // Handle any remaining samples (partial segment)
        let remaining = samples.len() % SEGMENT_SIZE;
        if remaining > 0 {
            let segment_index = SegmentIndex::from_sample_position(start_pos);

            // Create segment data
            let mut segment_samples = [0.0; SEGMENT_SIZE];
            let start = samples.len() - remaining;

            // Copy remaining samples and leave the rest as zeros
            segment_samples[..remaining].copy_from_slice(&samples[start..]);

            // Add segment
            segments.push(DecodedSegment {
                index: segment_index,
                segment: AudioSegment {
                    samples: segment_samples,
                },
                valid_samples: remaining,
            });
        }

        segments
    }
    fn adjust_segment_indices(&self, segments: Vec<DecodedSegment>) -> Vec<DecodedSegment> {
        let seek_pos = self.current_sample_position.load(Ordering::Relaxed);
        if seek_pos == 0 {
            return segments; // No adjustment needed
        }

        // Create base segment index from seek position
        let base_index = SegmentIndex::from_sample_position(seek_pos);

        // Adjust each segment's index
        segments
            .into_iter()
            .enumerate()
            .map(|(i, mut segment)| {
                segment.index = SegmentIndex(base_index.0 + i);
                segment
            })
            .collect()
    }
    // Convenience method to generate various test patterns
    pub fn new_with_pattern(pattern: &str, seconds: f32) -> Self {
        let sample_rate = 48000;
        let channels = 2;
        let total_samples = (seconds * sample_rate as f32 * channels as f32) as usize;

        let samples = match pattern {
            "sine" => {
                // Generate sine wave at 440Hz
                let mut data = Vec::with_capacity(total_samples);
                let frequency = 440.0;

                for i in 0..total_samples {
                    let t = i as f32 / (sample_rate as f32 * channels as f32);
                    let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
                    data.push(sample);
                }
                data
            }
            "ascending" => {
                // Generate ascending ramp from -0.9 to 0.9
                let mut data = Vec::with_capacity(total_samples);
                for i in 0..total_samples {
                    let sample = -0.9 + (1.8 * i as f32 / total_samples as f32);
                    data.push(sample);
                }
                data
            }
            "alternating" => {
                // Generate alternating pattern (high, zero, low)
                let mut data = Vec::with_capacity(total_samples);
                for i in 0..total_samples {
                    let sample = match i % 3 {
                        0 => 0.9,
                        1 => 0.0,
                        _ => -0.9,
                    };
                    data.push(sample);
                }
                data
            }
            "silence" => {
                // All zeros
                vec![0.0; total_samples]
            }
            "impulses" => {
                // Periodic impulses
                let mut data = Vec::with_capacity(total_samples);
                for i in 0..total_samples {
                    let sample = if i % 100 == 0 { 0.9 } else { 0.0 };
                    data.push(sample);
                }
                data
            }
            _ => {
                // Default to silence if pattern unknown
                vec![0.0; total_samples]
            }
        };

        Self::new_from_samples(samples)
    }
}

#[cfg(test)]
impl Source for TestSource {
    fn decode_next_frame(&self) -> Result<Vec<DecodedSegment>, audio_decoder::DecoderError> {
        // Get current position
        let pos = self.position.load(Ordering::Relaxed);

        // Check if we have any more frames
        if pos >= self.samples.len() {
            return Ok(Vec::new()); // EOF
        }

        // Get the current frame's segments
        let segments = self.samples[pos].clone();

        // Calculate how many samples this represents
        let sample_count: usize = segments.iter().map(|s| s.segment.samples.len()).sum();

        // Adjust segment indices based on seek position
        let adjusted_segments = self.adjust_segment_indices(segments);

        // Move to next frame
        self.position.store(pos + 1, Ordering::Relaxed);

        // Update current sample position
        let current_pos = self.current_sample_position.load(Ordering::Relaxed);
        self.current_sample_position
            .store(current_pos + sample_count, Ordering::Relaxed);

        Ok(adjusted_segments)
    }

    fn seek(&self, position: usize) -> Result<(), audio_decoder::DecoderError> {
        // Store the target sample position
        self.current_sample_position
            .store(position, Ordering::Relaxed);

        // Reset frame position to beginning
        self.position.store(0, Ordering::Relaxed);

        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        48000
    }

    fn audio_channels(&self) -> u16 {
        2
    }

    fn current_position(&self) -> usize {
        self.current_sample_position.load(Ordering::Relaxed)
    }

    fn duration_ms(&self) -> Option<u64> {
        // TestSource duration is not meaningful; return None
        None
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl Track {
    pub(crate) fn new_test() -> Result<Self, PlaybackError> {
        // Generate 1 second of 440Hz test tone
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let mut samples = Vec::with_capacity(sample_rate);

        for i in 0..sample_rate {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.1;
            samples.push(sample);
        }
        let buffer = HeapRb::new(1024 * 8);
        let (prod, _cons) = buffer.split();
        // Tests use same source and target rate to skip resampling
        Self::new(TestSource::new_from_samples(samples), prod, 48_000, 48_000)
    }

    // Add this method for tests
    pub(crate) fn ensure_ready_for_test(&mut self) -> Result<(), PlaybackError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;

    /// Verify that Track::new does not require a tokio runtime.
    /// The decoder must run on a dedicated std::thread, not a tokio task.
    #[test]
    fn track_new_does_not_require_tokio_runtime() {
        let sample_rate = 48_000u32;
        let samples: Vec<f32> = (0..sample_rate as usize)
            .map(|i| (i as f32 / sample_rate as f32).sin() * 0.1)
            .collect();
        let source = TestSource::new_from_samples(samples);
        let buffer = HeapRb::new(1024 * 8);
        let (prod, _cons) = buffer.split();
        // This must compile and succeed without a tokio runtime on the current thread
        let _track = Track::new(source, prod, sample_rate, sample_rate)
            .expect("Track::new should succeed without tokio runtime");
    }

    /// Verify that dropping a Track joins the decoder thread deterministically.
    #[test]
    fn track_drop_joins_decoder_thread() {
        let sample_rate = 48_000u32;
        let samples: Vec<f32> = vec![0.0f32; 1024];
        let source = TestSource::new_from_samples(samples);
        let buffer = HeapRb::new(1024 * 8);
        let (prod, _cons) = buffer.split();
        let track =
            Track::new(source, prod, sample_rate, sample_rate).expect("Track::new should succeed");
        // Dropping the track should not panic or deadlock
        drop(track);
    }
}
