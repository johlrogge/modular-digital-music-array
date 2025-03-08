use std::mem::MaybeUninit;
use std::sync::Arc;

use ringbuf::{Consumer, Producer, SharedRb};

use crate::error::PlaybackError;
use crate::source::Source;

use tokio::sync::mpsc;

pub enum TrackCommand {
    FillFrom(usize), // Fill the buffer starting from this position
    Shutdown,
}

type SampleConsumer = Consumer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>;
type SampleProducer = Producer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>;
pub struct Track<S: Source + Send + Sync + 'static> {
    source: Arc<S>,
    position: usize,
    playing: bool,
    volume: f32,
    consumer: SampleConsumer,
    command_tx: mpsc::Sender<TrackCommand>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl<S: Source + Send + Sync> Track<S> {
    const BUFFER_SIZE: usize = 4096; // Small buffer size to start with

    // Make this method generic over the Source type
    pub async fn new(source: S) -> Result<Self, PlaybackError> {
        let source = Arc::new(source);

        // Create ring buffer
        let rb = SharedRb::<f32, Vec<MaybeUninit<f32>>>::new(Self::BUFFER_SIZE);
        let (producer, consumer) = rb.split();

        // Create command channel
        let (command_tx, command_rx) = mpsc::channel(32);

        // Create track instance
        let track = Track {
            source: source.clone(),
            position: 0,
            playing: false,
            volume: 1.0,
            consumer,
            command_tx: command_tx.clone(),
            task_handle: None,
        };

        // Start background task
        let task_handle = tokio::spawn(async move {
            Self::buffer_management_task(source, producer, command_rx).await;
        });

        // Store the task handle
        let mut track = track;
        track.task_handle = Some(task_handle);

        // Send initial fill command and wait for buffer to start filling
        track
            .command_tx
            .send(TrackCommand::FillFrom(0))
            .await
            .expect("failed to send track command");

        // Wait for initial buffer to fill with timeout
        let timeout = tokio::time::Duration::from_millis(100);
        let start = tokio::time::Instant::now();

        while track.consumer.len() < track.consumer.capacity() / 4 {
            if tokio::time::Instant::now() - start > timeout {
                tracing::warn!("Timeout waiting for initial buffer to fill");
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }

        Ok(track)
    }

    async fn buffer_management_task(
        source: Arc<S>,
        mut producer: SampleProducer,
        mut command_rx: mpsc::Receiver<TrackCommand>,
    ) {
        let mut temp_buffer = vec![0.0; 1024];

        while let Some(command) = command_rx.recv().await {
            match command {
                TrackCommand::FillFrom(position) => {
                    // Fill the buffer from this position
                    let mut current_pos = position;
                    while producer.len() < producer.capacity() / 2 {
                        let read = source
                            .read_samples(current_pos, &mut temp_buffer)
                            .unwrap_or(0);
                        if read == 0 {
                            break;
                        }

                        for &sample in temp_buffer.iter().take(read) {
                            if producer.push(sample).is_err() {
                                break;
                            }
                        }

                        current_pos += read;
                    }
                }
                TrackCommand::Shutdown => {
                    break;
                }
            }
        }
    }

    pub fn play(&mut self) {
        // Start filling the buffer first
        if let Err(e) = self.prefill_buffer() {
            tracing::warn!("Failed to prefill buffer: {}", e);
        }

        self.await_ready();

        // Now mark as playing
        self.playing = true;

        tracing::info!(
            "Track playback started: playing={}, position={}, volume={}, buffer_fill={}%",
            self.playing,
            self.position,
            self.volume,
            self.consumer.len() * 100 / self.consumer.capacity()
        );
    }

    /// Wait for buffer to fill with a timeout
    fn await_ready(&mut self) {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(100);

        while !self.is_ready() {
            if start.elapsed() > timeout {
                tracing::warn!("Buffer fill timeout - starting playback with partial buffer");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub async fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        let max_position = self.source.len();
        let target_position = position.min(max_position);

        self.source.seek(target_position).await?;
        self.position = target_position;

        self.drain_buffer();

        // Tell the background task to fill from the new position
        if let Err(e) = self
            .command_tx
            .try_send(TrackCommand::FillFrom(self.position))
        {
            tracing::warn!("Failed to send fill command after seek: {}", e);
        }

        tracing::info!(
            "Track seeked to position={}/{}, playing={}, volume={}",
            self.position,
            max_position,
            self.playing,
            self.volume
        );

        Ok(())
    }

    fn drain_buffer(&mut self) {
        while self.consumer.pop().is_some() {}
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn length(&self) -> usize {
        self.source.len()
    }

    /// A track is considered ready if has at least 25% of the buffer filled
    pub fn is_ready(&self) -> bool {
        self.consumer.len() >= self.consumer.capacity() / 4
    }

    pub fn get_next_samples(&mut self, output: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            return Ok(0);
        }

        // Read from the buffer
        let mut read_from_buffer = 0;
        for out in output.iter_mut() {
            if let Some(sample) = self.consumer.pop() {
                *out = sample;
                read_from_buffer += 1;
            } else {
                break;
            }
        }

        // If we read something from the buffer, apply volume and update position
        if read_from_buffer > 0 {
            // Apply volume
            for sample in &mut output[..read_from_buffer] {
                *sample *= self.volume;
            }

            self.position += read_from_buffer;

            // Try to fill the buffer for next time if it's getting low
            if self.consumer.len() < self.consumer.capacity() / 4 {
                self.fill_buffer()?;
            }

            return Ok(read_from_buffer);
        }

        // Buffer underrun detected!
        tracing::warn!(
            "Buffer underrun detected at position={}/{}",
            self.position,
            self.source.len()
        );

        // Try to fill the buffer
        self.fill_buffer()?;

        // Check if we're at the end of the track
        if self.position >= self.source.len() {
            self.playing = false;
            tracing::info!("End of track reached");
        } else {
            // If we're not at the end, this is a true underrun
            // We could output silence or try waiting a bit
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        Ok(0)
    }

    fn prefill_buffer(&mut self) -> Result<(), PlaybackError> {
        // Send a Fill command to start filling the buffer
        if let Err(e) = self
            .command_tx
            .try_send(TrackCommand::FillFrom(self.position))
        {
            // Handle error if needed
            tracing::warn!("Failed to send fill command: {}", e);
        }

        // We don't know how much was filled, but the background task is now working on it
        Ok(())
    }
    // Simple method to fill the buffer
    fn fill_buffer(&mut self) -> Result<usize, PlaybackError> {
        // Instead of directly accessing producer, send a Fill command
        // Use try_send to avoid blocking on a full channel
        if let Err(e) = self
            .command_tx
            .try_send(TrackCommand::FillFrom(self.position))
        {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    // Channel is full, buffer is probably being filled already
                    // Just continue
                    return Ok(0);
                }
                mpsc::error::TrySendError::Closed(_) => {
                    // Channel is closed, this is an error
                    return Err(PlaybackError::AudioDevice(
                        "Buffer management task stopped".into(),
                    ));
                }
            }
        }

        // We don't know exactly how many samples were written,
        // but we can assume the buffer is being filled
        Ok(self.consumer.capacity() / 4) // Return an estimate
    }
    pub fn stop(&mut self) {
        self.playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_volume(&mut self, db: f32) {
        // Convert dB to linear amplitude
        self.volume = 10.0f32.powf(db / 20.0);
    }

    pub fn get_volume(&self) -> f32 {
        self.volume
    }
}

impl<S: Source + Send + Sync + 'static> Drop for Track<S> {
    fn drop(&mut self) {
        // Send shutdown command
        // Use try_send to avoid blocking if the receiver is gone
        let _ = self.command_tx.try_send(TrackCommand::Shutdown);

        // Abort task if it's still running
        if let Some(task) = self.task_handle.take() {
            // Use try_cancel which is safer than abort
            // This properly handles test contexts where no runtime exists
            task.abort();
        }
    }
}

#[cfg(test)]
pub struct TestSource {
    samples: Vec<f32>,
}

#[cfg(test)]
impl Source for TestSource {
    fn read_samples(&self, position: usize, buffer: &mut [f32]) -> Result<usize, PlaybackError> {
        if position >= self.samples.len() {
            return Ok(0);
        }
        let available = self.samples.len() - position;
        let count = buffer.len().min(available);

        buffer[..count].copy_from_slice(&self.samples[position..position + count]);
        Ok(count)
    }

    fn sample_rate(&self) -> u32 {
        48000
    }

    fn audio_channels(&self) -> u16 {
        2
    }

    fn len(&self) -> usize {
        self.samples.len()
    }
}

#[cfg(test)]
impl Track<TestSource> {
    pub(crate) async fn new_test() -> Result<Self, PlaybackError> {
        // Create a simple 1-second sine wave source

        // Generate 1 second of 440Hz test tone
        let sample_rate = 48000;
        let frequency = 440.0; // A4 note
        let mut samples = Vec::with_capacity(sample_rate);

        for i in 0..sample_rate {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.1;
            samples.push(sample);
        }

        Self::new(TestSource { samples }).await
    }
}
