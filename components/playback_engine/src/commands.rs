use crate::{source::Source, track::Track};
use playback_primitives::Deck;

pub enum AudioCommand<S: Source + Send + Sync> {
    /// Add a new track to a channel
    AddTrack {
        /// The channel to add the track to
        channel: Deck,
        /// The track to add
        track: Track<S>,
    },
    /// Remove a track from a channel
    RemoveTrack(Deck),
}

impl<S: Source + Send + Sync> AudioCommand<S> {
    /// Create a new AddTrack command
    pub fn add_track(channel: Deck, track: Track<S>) -> Self {
        Self::AddTrack { channel, track }
    }

    /// Create a new RemoveTrack command
    pub fn remove_track(channel: Deck) -> Self {
        Self::RemoveTrack(channel)
    }
}
