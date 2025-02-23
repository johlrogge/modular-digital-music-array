use crate::track::Track;
use playback_primitives::Deck;

/// Manages track assignments to channels and their synchronization
use parking_lot::RwLock;
use std::sync::Arc;

use std::collections::HashMap;

#[derive(Clone)]
pub struct Channels {
    tracks: Arc<RwLock<HashMap<Deck, Arc<RwLock<Track>>>>>,
}

impl Channels {
    pub fn new() -> Self {
        Self {
            tracks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn assign(&self, deck: Deck, track: Track) {
        let mut tracks = self.tracks.write();
        tracks.insert(deck, Arc::new(RwLock::new(track)));
    }

    pub fn get_track(&self, deck: Deck) -> Option<Arc<RwLock<Track>>> {
        let tracks = self.tracks.read();
        tracks.get(&deck).cloned()
    }

    pub fn clear(&self, deck: Deck) {
        let mut tracks = self.tracks.write();
        tracks.remove(&deck);
    }

    pub(crate) fn read(
        &self,
    ) -> parking_lot::RwLockReadGuard<'_, HashMap<Deck, Arc<RwLock<Track>>>> {
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
        assert!(tracks.contains_key(&Deck::A));
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
