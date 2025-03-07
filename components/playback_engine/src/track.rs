use std::borrow::BorrowMut;
use std::mem::MaybeUninit;
use std::path::Path;
use std::sync::Arc;

use ringbuf::{Consumer, HeapRb, Producer, Rb, SharedRb};

use crate::error::PlaybackError;
use crate::source::{FlacSource, Source};

use tokio::sync::mpsc;

enum TrackCommand {
    Fill(usize), // Fill the buffer from this position
    Seek(usize), // Seek to this position
    Stop,        // Stop the task
}

pub struct Track<S: Source + Send + Sync + 'static> {
    source: Arc<S>,
    position: usize,
    playing: bool,
    volume: f32,
    consumer: Consumer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>,
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

        // Send initial fill command
        track.command_tx.send(TrackCommand::Fill(0)).await;

        Ok(track)
    }

    async fn buffer_management_task(
        source: Arc<S>,
        mut producer: Producer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>,
        mut command_rx: mpsc::Receiver<TrackCommand>,
    ) {
        let mut temp_buffer = vec![0.0; 1024];

        while let Some(command) = command_rx.recv().await {
            match command {
                TrackCommand::Fill(position) => {
                    // Fill the buffer
                    let mut current_pos = position;
                    while producer.len() < producer.capacity() / 2 {
                        let read = source
                            .read_samples(current_pos, &mut temp_buffer)
                            .unwrap_or(0);
                        if read == 0 {
                            break;
                        }

                        for i in 0..read {
                            if producer.push(temp_buffer[i]).is_err() {
                                break;
                            }
                        }

                        current_pos += read;
                    }
                }
                TrackCommand::Seek(pos) => {
                    // Clear the buffer and refill
                    //producer.rb().borrow_mut().clear();

                    // Start filling from the new position
                    // let _ =
                    //     Self::buffer_management_task(source.clone(), producer, command_rx).await;
                    break;
                }
                TrackCommand::Stop => {
                    break;
                }
            }
        }
    }

    // This method will be called when we have a Tokio runtime available
    pub fn start_background_task(
        &mut self,
        command_rx: mpsc::Receiver<TrackCommand>,
    ) -> tokio::task::JoinHandle<()> {
        // The implementation will go here, but for now just a placeholder
        tokio::spawn(async move {
            // Background task logic will go here
        })
    }

    pub fn play(&mut self) {
        self.playing = true;

        // Pre-fill buffer when playback starts
        if let Err(e) = self.prefill_buffer() {
            tracing::warn!("Failed to prefill buffer: {}", e);
        }

        tracing::info!(
            "Track playback started: playing={}, position={}, volume={}",
            self.playing,
            self.position,
            self.volume
        );
    }

    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        let max_position = self.source.len();
        let target_position = position.min(max_position);

        // Try to perform a real seek via the source's seek method
        if let Err(e) = self.source.seek(position) {
            tracing::warn!("Source seek failed: {}", e);
            // Continue anyway - we'll update the position counter
        }

        // Update position counter
        self.position = target_position;

        // Reset buffer by sending a Seek command
        // This will clear the buffer and refill it from the new position
        if let Err(e) = self.command_tx.try_send(TrackCommand::Seek(self.position)) {
            // If sending fails, log warning but continue
            tracing::warn!("Failed to send seek command: {}", e);
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

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn length(&self) -> usize {
        self.source.len()
    }

    pub fn get_next_samples(&mut self, output: &mut [f32]) -> Result<usize, PlaybackError> {
        if !self.playing {
            return Ok(0);
        }

        // Read from the buffer
        let mut read_from_buffer = 0;
        for i in 0..output.len() {
            if let Some(sample) = self.consumer.pop() {
                output[i] = sample;
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

        // If buffer is empty, try to fill it and try reading again
        self.fill_buffer()?;

        // If we're still playing but didn't get any samples, we might be at the end of the track
        if self.position >= self.source.len() {
            self.playing = false;
        }

        Ok(0)
    }
    fn prefill_buffer(&mut self) -> Result<(), PlaybackError> {
        // Send a Fill command to start filling the buffer
        if let Err(e) = self.command_tx.try_send(TrackCommand::Fill(self.position)) {
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
        if let Err(e) = self.command_tx.try_send(TrackCommand::Fill(self.position)) {
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
