use crate::channels::Channels;
use crate::error::PlaybackError;

/// Handles real-time mixing of audio from multiple channels
pub struct Mixer {
    mix_buffer: Vec<f32>,
}

impl Mixer {
    pub fn new(buffer_size: usize) -> Self {
        Self {
            mix_buffer: vec![0.0; buffer_size],
        }
    }

    /// Mix audio from all active channels into the output buffer
    pub fn mix(
        &mut self,
        channels: &Channels,
        output: &mut [f32],
        samples_per_callback: usize,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Get channel state
        let channel_state = channels.read();

        // Mix each active channel
        for (channel, track) in channel_state.iter() {
            let mut track = track.write();

            if track.is_playing() {
                // Get samples from this track
                self.mix_buffer[..samples_per_callback].fill(0.0);
                match track.get_next_samples(&mut self.mix_buffer[..samples_per_callback]) {
                    Ok(len) if len > 0 => {
                        tracing::debug!("Got {} samples from channel {:?}", len, channel);
                        let volume = track.get_volume();

                        // Mix into output with volume
                        for i in 0..len {
                            output[i] += self.mix_buffer[i] * volume;
                        }
                    }
                    Ok(_) => {
                        tracing::debug!("No samples from channel {:?}", channel);
                    }
                    Err(e) => {
                        tracing::error!("Error getting samples from channel {:?}: {}", channel, e);
                        return Err(PlaybackError::AudioDevice(format!(
                            "Mixing error on channel {:?}: {}",
                            channel, e
                        )));
                    }
                }
            }
        }

        // Apply master limiter to prevent clipping
        for sample in &mut output[..samples_per_callback] {
            *sample = sample.clamp(-1.0, 1.0);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::Track;
    use playback_primitives::Deck;

    #[test]
    fn test_mix_empty_channels() {
        let channels = Channels::new();
        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&channels, &mut output, 1024).unwrap();

        // Output should be silence
        assert!(output.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_mix_single_channel() {
        let channels = Channels::new();
        let mut track = Track::new_test();
        track.play(); // Start playback
        channels.assign(Deck::A, track);

        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&channels, &mut output, 1024).unwrap();

        // Output should contain samples
        assert!(!output.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn test_mix_prevents_clipping() {
        let channels = Channels::new();
        let mut track = Track::new_test();
        track.play();
        channels.assign(Deck::A, track);

        let mut mixer = Mixer::new(1024);
        let mut output = vec![0.0; 1024];

        mixer.mix(&channels, &mut output, 1024).unwrap();

        // No samples should exceed [-1.0, 1.0]
        assert!(output.iter().all(|&x| x >= -1.0 && x <= 1.0));
    }
}
