use crate::track::Track;
use playback_primitives::Channel;

pub enum AudioCommand {
    /// Add a new track to a channel
    AddTrack {
        /// The channel to add the track to
        channel: Channel,
        /// The track to add
        track: Track,
    },
    /// Remove a track from a channel
    RemoveTrack(Channel),
}

impl AudioCommand {
    /// Create a new AddTrack command
    pub fn add_track(channel: Channel, track: Track) -> Self {
        Self::AddTrack { channel, track }
    }

    /// Create a new RemoveTrack command
    pub fn remove_track(channel: Channel) -> Self {
        Self::RemoveTrack(channel)
    }
}
