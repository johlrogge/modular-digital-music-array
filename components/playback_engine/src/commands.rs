use crate::{source::Source, track::Track};
use playback_primitives::Deck;

pub enum AudioCommand {
    /// Add a new track to a channel
    AddTrack {
        /// The channel to add the track to
        channel: Deck,
        /// The track to add
        track: Track,
    },
    /// Remove a track from a channel
    RemoveTrack(Deck),
}

impl AudioCommand {
    /// Create a new AddTrack command
    pub fn add_track(channel: Deck, track: Track) -> Self {
        Self::AddTrack { channel, track }
    }

    /// Create a new RemoveTrack command
    pub fn remove_track(channel: Deck) -> Self {
        Self::RemoveTrack(channel)
    }
}
