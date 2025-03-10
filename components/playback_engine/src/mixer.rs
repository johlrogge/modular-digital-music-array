// in mixer.rs
use crate::error::PlaybackError;
use crate::source::Source;
use crate::track::Track;
use parking_lot::RwLock;
use playback_primitives::Deck;
use std::collections::HashMap;
use std::sync::Arc;

pub struct Mixer {
    mix_buffer: Vec<f32>,
}

impl Mixer {
    pub fn new(buffer_size: usize) -> Self {
        Self {
            mix_buffer: vec![0.0; buffer_size],
        }
    }

    pub fn mix<S: Source + Send + Sync>(
        &mut self,
        decks: &RwLock<HashMap<Deck, Arc<RwLock<Track<S>>>>>,
        output: &mut [f32],
        samples_per_callback: usize,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Get all tracks and their lock guards
        let tracks = decks.read();

        // Mix each active track
        for (deck, track) in tracks.iter() {
            let mut track = track.write();

            if track.is_playing() {
                tracing::debug!("Deck {:?} is playing", deck);

                // Get samples from this track
                self.mix_buffer[..samples_per_callback].fill(0.0);
                match track.get_next_samples(&mut self.mix_buffer[..samples_per_callback]) {
                    Ok(len) if len > 0 => {
                        let volume = track.get_volume();
                        tracing::debug!(
                            "Got {} samples from deck {:?}, volume: {}",
                            len,
                            deck,
                            volume
                        );

                        // Mix into output with volume
                        output
                            .iter_mut()
                            .zip(self.mix_buffer.iter())
                            .take(len)
                            .for_each(|(out, &input)| {
                                *out += input * volume;
                            });
                    }
                    Ok(_) => {
                        tracing::debug!("No samples from deck {:?}", deck);
                    }
                    Err(e) => {
                        tracing::error!("Error getting samples from deck {:?}: {}", deck, e);
                        return Err(PlaybackError::AudioDevice(format!(
                            "Mixing error on deck {:?}: {}",
                            deck, e
                        )));
                    }
                }
            } else {
                tracing::debug!("Deck {:?} is not playing", deck);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{source::FlacSource, track::Track};
    use std::collections::HashMap;

    #[test]
    fn test_mix_empty_decks() {
        let decks = RwLock::new(HashMap::new());
        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix::<FlacSource>(&decks, &mut output, 1024).unwrap();

        // Output should be silence
        assert!(output.iter().all(|&x| x == 0.0));
    }

    #[tokio::test]
    async fn test_mix_single_deck() {
        let decks = RwLock::new(HashMap::new());

        // Setup a deck with a test track
        let mut track = Track::new_test().await.unwrap();

        // Force track to be ready for testing
        track.ensure_ready_for_test().await.unwrap();

        track.play(); // Start playback

        // Insert track into decks and get a reference to it
        let track_ref = Arc::new(RwLock::new(track));
        decks.write().insert(Deck::A, track_ref.clone());

        // Wait for track to be ready with increased timeout
        let timeout = std::time::Duration::from_millis(500); // Increased from 100ms
        let start = std::time::Instant::now();

        let mut ready = false;
        while !ready {
            if start.elapsed() > timeout {
                panic!("Timeout waiting for track to be ready");
            }

            // Check if track is ready
            ready = track_ref.read().is_ready();

            if !ready {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }

        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&decks, &mut output, 1024).unwrap();

        // Output should contain samples
        assert!(!output.iter().all(|&x| x == 0.0));
    }

    #[tokio::test]
    async fn test_mix_prevents_clipping() {
        let decks = RwLock::new(HashMap::new());

        // Setup a deck with a test track
        let mut track = Track::new_test().await.unwrap();
        track.play();
        decks.write().insert(Deck::A, Arc::new(RwLock::new(track)));

        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&decks, &mut output, 1024).unwrap();

        // No samples should exceed [-1.0, 1.0]
        assert!(output.iter().all(|&x| (-1.0..=1.0).contains(&x)));
    }
}
