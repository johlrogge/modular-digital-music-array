use crate::channels::Channels;
use crate::error::PlaybackError;

pub struct Mixer {
    mix_buffer: Vec<f32>,
}

impl Mixer {
    pub fn new(buffer_size: usize) -> Self {
        Self {
            mix_buffer: vec![0.0; buffer_size],
        }
    }

    // in components/playback_engine/src/mixer.rs
    pub fn mix(
        &mut self,
        channels: &Channels,
        output: &mut [f32],
        samples_per_callback: usize,
    ) -> Result<(), PlaybackError> {
        // Clear output buffer
        output[..samples_per_callback].fill(0.0);

        // Get all tracks and their lock guards
        let tracks = channels.read();
        tracing::debug!("Mixer: found {} tracks", tracks.len());
        // Mix each active track
        for (channel, track) in tracks.iter() {
            let track_lock = track.clone();
            let mut track = track_lock.write();

            if track.is_playing() {
                tracing::debug!("Channel {:?} is playing", channel);

                // Get samples from this track
                self.mix_buffer[..samples_per_callback].fill(0.0);
                match track.get_next_samples(&mut self.mix_buffer[..samples_per_callback]) {
                    Ok(len) if len > 0 => {
                        let volume = track.get_volume();
                        tracing::debug!(
                            "Got {} samples from channel {:?}, volume: {}",
                            len,
                            channel,
                            volume
                        );

                        // Check for non-zero samples
                        let has_audio = self.mix_buffer[..len].iter().any(|&s| s.abs() > 1e-6);
                        tracing::debug!("Channel {:?} has audio: {}", channel, has_audio);

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
            } else {
                tracing::debug!("Channel {:?} is not playing", channel);
            }
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
        assert!(output.iter().all(|&x| (-1.0..=1.0).contains(&x)));
    }
}
