use crate::track::Track;
use parking_lot::RwLock;
use playback_primitives::Deck;
use std::sync::Arc;

/// Manages track assignments to channels and their synchronization
#[derive(Clone)]
pub struct Channels {
    tracks: Arc<RwLock<Vec<(Deck, Arc<RwLock<Track>>)>>>,
}

impl Channels {
    pub fn new() -> Self {
        Self {
            tracks: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn assign(&self, channel: Deck, track: Track) {
        let mut tracks = self.tracks.write();
        tracks.retain(|(ch, _)| *ch != channel);
        tracks.push((channel, Arc::new(RwLock::new(track))));
        tracing::info!("Assigned track to channel {:?}", channel);
    }

    pub fn clear(&self, channel: Deck) {
        let mut tracks = self.tracks.write();
        tracks.retain(|(ch, _)| *ch != channel);
        tracing::info!("Cleared channel {:?}", channel);
    }

    pub fn get_track(&self, channel: Deck) -> Option<Arc<RwLock<Track>>> {
        self.tracks
            .read()
            .iter()
            .find(|(ch, _)| *ch == channel)
            .map(|(_, track)| Arc::clone(track))
    }

    pub(crate) fn read(&self) -> parking_lot::RwLockReadGuard<'_, Vec<(Deck, Arc<RwLock<Track>>)>> {
        self.tracks.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assign_track() {
        let channels = Channels::new();
        let track = Track::new_test();

        channels.assign(Deck::A, track);

        let tracks = channels.read();
        assert_eq!(tracks.len(), 1);
        assert!(matches!(tracks[0].0, Deck::A));
    }

    #[test]
    fn test_clear_channel() {
        let channels = Channels::new();
        let track = Track::new_test();

        channels.assign(Deck::A, track);
        channels.clear(Deck::A);

        let tracks = channels.read();
        assert_eq!(tracks.len(), 0);
    }

    #[test]
    fn test_get_track() {
        let channels = Channels::new();
        let track = Track::new_test();

        channels.assign(Deck::A, track);

        assert!(channels.get_track(Deck::A).is_some());
        assert!(channels.get_track(Deck::B).is_none());
    }
}
