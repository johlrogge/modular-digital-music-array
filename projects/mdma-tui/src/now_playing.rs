use event_protocol::PlaybackEvent;
use mdma_client::ContentHash;

/// Represents the current playback state as a proper enum,
/// eliminating the illegal state of (None, is_paused=true).
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackStatus {
    Stopped,
    Playing {
        track: ContentHash,
        position_ms: u64,
        duration_ms: u64,
    },
    Paused {
        track: ContentHash,
        position_ms: u64,
        duration_ms: u64,
    },
}

/// State reducer over PlaybackEvents.
///
/// NOT a poller — call `apply` each time an event arrives from the subscriber
/// thread. Returns true if the change warrants a redraw.
pub struct NowPlaying {
    pub status: PlaybackStatus,
    pub queue_length: usize,
    pub title: Option<String>,
    pub artist: Option<String>,
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self::new()
    }
}

impl NowPlaying {
    pub fn new() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            queue_length: 0,
            title: None,
            artist: None,
        }
    }

    pub fn set_track_metadata(&mut self, title: Option<String>, artist: Option<String>) {
        self.title = title;
        self.artist = artist;
    }

    /// Apply a playback event, returning `true` if a redraw is warranted.
    pub fn apply(&mut self, event: &PlaybackEvent) -> bool {
        match event {
            PlaybackEvent::TrackStarted { hash } => {
                self.status = PlaybackStatus::Playing {
                    track: hash.clone(),
                    position_ms: 0,
                    duration_ms: 0,
                };
                true
            }
            PlaybackEvent::TrackEnded { .. } | PlaybackEvent::TrackStopped { .. } => {
                self.status = PlaybackStatus::Stopped;
                self.title = None;
                self.artist = None;
                true
            }
            PlaybackEvent::TrackPaused { .. } => {
                if let PlaybackStatus::Playing {
                    track,
                    position_ms,
                    duration_ms,
                } = self.status.clone()
                {
                    self.status = PlaybackStatus::Paused {
                        track,
                        position_ms,
                        duration_ms,
                    };
                }
                true
            }
            PlaybackEvent::TrackResumed { .. } => {
                if let PlaybackStatus::Paused {
                    track,
                    position_ms,
                    duration_ms,
                } = self.status.clone()
                {
                    self.status = PlaybackStatus::Playing {
                        track,
                        position_ms,
                        duration_ms,
                    };
                }
                true
            }
            PlaybackEvent::PositionUpdate {
                position_ms,
                duration_ms,
                ..
            } => {
                match &mut self.status {
                    PlaybackStatus::Playing {
                        position_ms: p,
                        duration_ms: d,
                        ..
                    }
                    | PlaybackStatus::Paused {
                        position_ms: p,
                        duration_ms: d,
                        ..
                    } => {
                        *p = *position_ms;
                        *d = *duration_ms;
                    }
                    PlaybackStatus::Stopped => {}
                }
                // No redraw needed on every position tick — avoids excessive rendering.
                false
            }
            PlaybackEvent::QueueChanged { length } => {
                self.queue_length = *length;
                true
            }
            PlaybackEvent::SessionStarted { .. } | PlaybackEvent::SessionEnded { .. } => false,
        }
    }

    /// Progress as a fraction in [0.0, 1.0], safe against division by zero.
    #[allow(dead_code)]
    pub fn progress_ratio(&self) -> f64 {
        let (position_ms, duration_ms) = match &self.status {
            PlaybackStatus::Playing {
                position_ms,
                duration_ms,
                ..
            }
            | PlaybackStatus::Paused {
                position_ms,
                duration_ms,
                ..
            } => (*position_ms, *duration_ms),
            PlaybackStatus::Stopped => return 0.0,
        };
        if duration_ms == 0 {
            return 0.0;
        }
        (position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    #[test]
    fn track_started_sets_playing() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        let redraw = np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        assert_eq!(
            np.status,
            PlaybackStatus::Playing {
                track: hash,
                position_ms: 0,
                duration_ms: 0,
            }
        );
        assert!(redraw);
    }

    #[test]
    fn track_ended_sets_stopped() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        let redraw = np.apply(&PlaybackEvent::TrackEnded { hash });
        assert_eq!(np.status, PlaybackStatus::Stopped);
        assert!(redraw);
    }

    #[test]
    fn track_stopped_sets_stopped() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        let redraw = np.apply(&PlaybackEvent::TrackStopped { hash });
        assert_eq!(np.status, PlaybackStatus::Stopped);
        assert!(redraw);
    }

    #[test]
    fn paused_transitions_to_paused() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        let redraw = np.apply(&PlaybackEvent::TrackPaused { hash: hash.clone() });
        assert!(matches!(&np.status, PlaybackStatus::Paused { track, .. } if track == &hash));
        assert!(redraw);
    }

    #[test]
    fn resumed_transitions_to_playing() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        np.apply(&PlaybackEvent::TrackPaused { hash: hash.clone() });
        let redraw = np.apply(&PlaybackEvent::TrackResumed { hash: hash.clone() });
        assert!(matches!(&np.status, PlaybackStatus::Playing { track, .. } if track == &hash));
        assert!(redraw);
    }

    #[test]
    fn position_update_returns_false() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        let redraw = np.apply(&PlaybackEvent::PositionUpdate {
            hash,
            position_ms: 5000,
            duration_ms: 200_000,
        });
        assert!(matches!(
            &np.status,
            PlaybackStatus::Playing {
                position_ms: 5000,
                duration_ms: 200_000,
                ..
            }
        ));
        assert!(!redraw);
    }

    #[test]
    fn set_track_metadata_stores_fields() {
        let mut np = NowPlaying::new();
        np.set_track_metadata(Some("Init".into()), Some("Carbon Based Lifeforms".into()));
        assert_eq!(np.title.as_deref(), Some("Init"));
        assert_eq!(np.artist.as_deref(), Some("Carbon Based Lifeforms"));
    }

    #[test]
    fn track_ended_clears_metadata() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.set_track_metadata(Some("Init".into()), Some("CBL".into()));
        np.apply(&PlaybackEvent::TrackEnded { hash });
        assert!(np.title.is_none());
        assert!(np.artist.is_none());
    }

    #[test]
    fn track_stopped_clears_metadata() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.set_track_metadata(Some("Init".into()), Some("CBL".into()));
        np.apply(&PlaybackEvent::TrackStopped { hash });
        assert!(np.title.is_none());
        assert!(np.artist.is_none());
    }

    #[test]
    fn progress_ratio_zero_when_stopped() {
        let np = NowPlaying::new();
        assert_eq!(np.progress_ratio(), 0.0);
    }

    #[test]
    fn progress_ratio_zero_when_no_duration() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash });
        assert_eq!(np.progress_ratio(), 0.0);
    }

    #[test]
    fn progress_ratio_midpoint() {
        let mut np = NowPlaying::new();
        let hash = make_hash("sha256:abc");
        np.apply(&PlaybackEvent::TrackStarted { hash: hash.clone() });
        np.apply(&PlaybackEvent::PositionUpdate {
            hash,
            position_ms: 100,
            duration_ms: 200,
        });
        assert!((np.progress_ratio() - 0.5).abs() < f64::EPSILON);
    }
}
