use event_protocol::PlaybackEvent;
use mdma_client::{ContentHash, PositionMs, Transport, TransportStatus};

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

impl PlaybackStatus {
    /// The track shown, if any.
    fn track(&self) -> Option<&ContentHash> {
        match self {
            PlaybackStatus::Stopped => None,
            PlaybackStatus::Playing { track, .. } | PlaybackStatus::Paused { track, .. } => {
                Some(track)
            }
        }
    }
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

    /// Take the state from what the node says it is (`GetTransport`): the
    /// source of truth when the audible track changes without a `TrackStarted`
    /// (a preview starting or ending, ADR-031). Returns `true`: redraw.
    pub fn apply_transport(&mut self, status: &TransportStatus) -> bool {
        self.status = match status {
            TransportStatus::Idle => PlaybackStatus::Stopped,
            TransportStatus::Loaded {
                transport,
                now,
                position_ms,
                duration_ms,
                ..
            } => {
                let track = now.clone();
                let position_ms = position_ms.map_or(0, PositionMs::as_millis);
                let duration_ms = duration_ms.map_or(0, PositionMs::as_millis);
                match transport {
                    Transport::Playing => PlaybackStatus::Playing {
                        track,
                        position_ms,
                        duration_ms,
                    },
                    Transport::Paused => PlaybackStatus::Paused {
                        track,
                        position_ms,
                        duration_ms,
                    },
                }
            }
        };
        true
    }

    /// Whether an event names a track other than the one shown. Such an
    /// event is about something that is not audible (the held track while a
    /// preview plays), so it must not change what is shown.
    fn names_other_track(&self, hash: &ContentHash) -> bool {
        self.status.track().is_some_and(|shown| shown != hash)
    }

    /// Apply a playback event, returning `true` if a redraw is warranted.
    pub fn apply(&mut self, event: &PlaybackEvent) -> bool {
        match event {
            PlaybackEvent::TrackEnded { hash }
            | PlaybackEvent::TrackStopped { hash }
            | PlaybackEvent::TrackPaused { hash }
            | PlaybackEvent::TrackResumed { hash }
            | PlaybackEvent::PositionUpdate { hash, .. }
                if self.names_other_track(hash) =>
            {
                false
            }
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
            PlaybackEvent::SessionStarted { .. }
            | PlaybackEvent::SessionEnded { .. }
            | PlaybackEvent::OutputChanged { .. } => false,
            // The audible track changes without a TrackStarted: the caller
            // re-reads the transport (`apply_transport`), which says what is
            // audible and whether it plays, as these events do not.
            PlaybackEvent::PreviewStarted { .. } | PlaybackEvent::PreviewEnded { .. } => false,
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
    fn events_about_another_track_do_not_change_what_is_shown() {
        let mut np = NowPlaying::new();
        let shown = make_hash("sha256:preview");
        let held = make_hash("sha256:held");
        np.apply(&PlaybackEvent::TrackStarted {
            hash: shown.clone(),
        });

        assert!(!np.apply(&PlaybackEvent::TrackPaused { hash: held.clone() }));
        assert!(!np.apply(&PlaybackEvent::TrackResumed { hash: held.clone() }));
        assert!(!np.apply(&PlaybackEvent::TrackStopped { hash: held.clone() }));
        assert!(!np.apply(&PlaybackEvent::TrackEnded { hash: held.clone() }));
        np.apply(&PlaybackEvent::PositionUpdate {
            hash: held,
            position_ms: 9,
            duration_ms: 99,
        });

        assert_eq!(
            np.status,
            PlaybackStatus::Playing {
                track: shown,
                position_ms: 0,
                duration_ms: 0
            }
        );
    }

    #[test]
    fn the_transport_the_node_reports_is_what_is_shown() {
        let mut np = NowPlaying::new();
        let loaded = |transport| TransportStatus::Loaded {
            transport,
            now: make_hash("sha256:abc"),
            position_ms: Some(PositionMs::new(1_500)),
            duration_ms: None,
            preview: None,
        };

        assert!(np.apply_transport(&loaded(Transport::Paused)));
        assert_eq!(
            np.status,
            PlaybackStatus::Paused {
                track: make_hash("sha256:abc"),
                position_ms: 1_500,
                duration_ms: 0
            }
        );
        np.apply_transport(&loaded(Transport::Playing));
        assert!(matches!(np.status, PlaybackStatus::Playing { .. }));
        np.apply_transport(&TransportStatus::Idle);
        assert_eq!(np.status, PlaybackStatus::Stopped);
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
