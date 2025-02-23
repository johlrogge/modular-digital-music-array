// in mixer.rs
use crate::error::PlaybackError;
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

    pub fn mix(
        &mut self,
        decks: &RwLock<HashMap<Deck, Arc<RwLock<Track>>>>,
        output: &mut [f32],
        samples_per_callback: usize,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Get all tracks and their lock guards
        let tracks = decks.read();
        tracing::info!("Mixer: found {} tracks", tracks.len());

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
                        for i in 0..len {
                            output[i] += self.mix_buffer[i] * volume;
                        }
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
    use crate::track::Track;
    use std::collections::HashMap;

    #[test]
    fn test_mix_empty_decks() {
        let decks = RwLock::new(HashMap::new());
        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&decks, &mut output, 1024).unwrap();

        // Output should be silence
        assert!(output.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_mix_single_deck() {
        let decks = RwLock::new(HashMap::new());

        // Setup a deck with a test track
        let mut track = Track::new_test();
        track.play(); // Start playback
        decks.write().insert(Deck::A, Arc::new(RwLock::new(track)));

        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&decks, &mut output, 1024).unwrap();

        // Output should contain samples
        assert!(!output.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_mix_prevents_clipping() {
        let decks = RwLock::new(HashMap::new());

        // Setup a deck with a test track
        let mut track = Track::new_test();
        track.play();
        decks.write().insert(Deck::A, Arc::new(RwLock::new(track)));

        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&decks, &mut output, 1024).unwrap();

        // No samples should exceed [-1.0, 1.0]
        assert!(output.iter().all(|&x| x >= -1.0 && x <= 1.0));
    }
}
