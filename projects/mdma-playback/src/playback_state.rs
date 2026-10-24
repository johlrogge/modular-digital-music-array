use event_protocol::PlaybackEvent;
use media_protocol::ContentHash;
use music_facts::{MusicValue, StartReason, StopReason};
use playback_primitives::{SessionId, SourceName};

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackState {
    Idle,
    Playing { hash: ContentHash },
    Paused { hash: ContentHash },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEffect {
    StopEngine,
    PauseEngine,
    PlayEngine,
    LoadAndPlay {
        hash: ContentHash,
        source: SourceName,
    },
    EmitEvent(PlaybackEvent),
    WriteFact {
        hash: ContentHash,
        value: MusicValue,
    },
}

pub struct PlaybackStateMachine {
    state: PlaybackState,
    session_id: Option<SessionId>,
}

impl PlaybackStateMachine {
    pub fn new() -> Self {
        Self {
            state: PlaybackState::Idle,
            session_id: None,
        }
    }

    pub fn state(&self) -> &PlaybackState {
        &self.state
    }

    pub fn current_hash(&self) -> Option<&ContentHash> {
        match &self.state {
            PlaybackState::Idle => None,
            PlaybackState::Playing { hash } | PlaybackState::Paused { hash } => Some(hash),
        }
    }

    /// Returns the current session ID, if a session is active.
    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    #[allow(dead_code)]
    pub fn is_playing(&self) -> bool {
        matches!(self.state, PlaybackState::Playing { .. })
    }

    /// Start a new session if one is not already active.
    /// Returns a `SessionStarted` effect if a new session was created.
    fn maybe_start_session(&mut self, effects: &mut Vec<PlaybackEffect>) {
        if self.session_id.is_none() {
            let id = SessionId::now();
            self.session_id = Some(id.clone());
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::SessionStarted {
                id,
            }));
        }
    }

    /// End the active session if one exists.
    /// Returns a `SessionEnded` effect if a session was active.
    fn maybe_end_session(&mut self, effects: &mut Vec<PlaybackEffect>) {
        if let Some(id) = self.session_id.take() {
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::SessionEnded {
                id,
            }));
        }
    }

    /// Start playing a track popped from the queue. If a track is currently
    /// Playing or Paused, its stop is recorded and the new load cuts it off.
    pub fn play_queue(&mut self, hash: ContentHash, source: SourceName) -> Vec<PlaybackEffect> {
        let mut effects = Vec::new();

        // Record the stop of whatever is currently playing/paused.
        match &self.state.clone() {
            PlaybackState::Playing { hash: h } | PlaybackState::Paused { hash: h } => {
                // No StopEngine: LoadAndPlay below cuts immediately, and an
                // explicit stop would discard the preloaded next track.
                effects.push(PlaybackEffect::WriteFact {
                    hash: h.clone(),
                    value: MusicValue::TrackStopped(StopReason::OnRequest),
                });
                effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                    hash: h.clone(),
                }));
            }
            PlaybackState::Idle => {}
        }

        // Transition to Playing.
        self.state = PlaybackState::Playing { hash: hash.clone() };

        // Start a session if this is the first track (Idle -> Playing).
        self.maybe_start_session(&mut effects);

        effects.push(PlaybackEffect::LoadAndPlay {
            hash: hash.clone(),
            source,
        });
        effects.push(PlaybackEffect::WriteFact {
            hash: hash.clone(),
            value: MusicValue::TrackStarted(StartReason::ByQueue),
        });
        effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted {
            hash: hash.clone(),
        }));

        effects
    }

    /// Stop the current track explicitly (user request).
    pub fn stop(&mut self) -> Vec<PlaybackEffect> {
        match self.state.clone() {
            PlaybackState::Playing { hash } | PlaybackState::Paused { hash } => {
                self.state = PlaybackState::Idle;
                let mut effects = vec![
                    PlaybackEffect::StopEngine,
                    PlaybackEffect::WriteFact {
                        hash: hash.clone(),
                        value: MusicValue::TrackStopped(StopReason::OnRequest),
                    },
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped { hash: hash.clone() }),
                ];
                self.maybe_end_session(&mut effects);
                effects
            }
            PlaybackState::Idle => vec![],
        }
    }

    /// Skip the current track and optionally start the next.
    pub fn skip(&mut self, next: Option<(ContentHash, SourceName)>) -> Vec<PlaybackEffect> {
        let mut effects = Vec::new();

        match self.state.clone() {
            PlaybackState::Playing { hash: h } | PlaybackState::Paused { hash: h } => {
                // With a next track, LoadAndPlay cuts immediately and StopEngine
                // would discard the preload; only stop when nothing follows.
                if next.is_none() {
                    effects.push(PlaybackEffect::StopEngine);
                }
                effects.push(PlaybackEffect::WriteFact {
                    hash: h.clone(),
                    value: MusicValue::TrackStopped(StopReason::OnSkip),
                });
                effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                    hash: h.clone(),
                }));

                match next {
                    Some((next_hash, next_source)) => {
                        self.state = PlaybackState::Playing {
                            hash: next_hash.clone(),
                        };
                        // Session continues — Playing -> Playing, no new session start.
                        effects.push(PlaybackEffect::LoadAndPlay {
                            hash: next_hash.clone(),
                            source: next_source,
                        });
                        effects.push(PlaybackEffect::WriteFact {
                            hash: next_hash.clone(),
                            value: MusicValue::TrackStarted(StartReason::ByQueue),
                        });
                        effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted {
                            hash: next_hash.clone(),
                        }));
                    }
                    None => {
                        self.state = PlaybackState::Idle;
                        self.maybe_end_session(&mut effects);
                    }
                }
            }
            PlaybackState::Idle => {
                if let Some((next_hash, next_source)) = next {
                    self.state = PlaybackState::Playing {
                        hash: next_hash.clone(),
                    };
                    self.maybe_start_session(&mut effects);
                    effects.push(PlaybackEffect::LoadAndPlay {
                        hash: next_hash.clone(),
                        source: next_source,
                    });
                    effects.push(PlaybackEffect::WriteFact {
                        hash: next_hash.clone(),
                        value: MusicValue::TrackStarted(StartReason::ByQueue),
                    });
                    effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted {
                        hash: next_hash.clone(),
                    }));
                }
                // Idle + None => no-op
            }
        }

        effects
    }

    /// Pause a playing track.
    pub fn pause(&mut self) -> Vec<PlaybackEffect> {
        match self.state.clone() {
            PlaybackState::Playing { hash } => {
                self.state = PlaybackState::Paused { hash: hash.clone() };
                vec![
                    PlaybackEffect::PauseEngine,
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackPaused { hash: hash.clone() }),
                ]
            }
            PlaybackState::Paused { .. } | PlaybackState::Idle => vec![],
        }
    }

    /// Resume a paused track.
    pub fn resume(&mut self) -> Vec<PlaybackEffect> {
        match self.state.clone() {
            PlaybackState::Paused { hash } => {
                self.state = PlaybackState::Playing { hash: hash.clone() };
                vec![
                    PlaybackEffect::PlayEngine,
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackResumed { hash: hash.clone() }),
                ]
            }
            PlaybackState::Playing { .. } | PlaybackState::Idle => vec![],
        }
    }

    /// Called when the engine switched to the preloaded next track on its own
    /// (gapless advance). Emits the same facts/events as `track_ended` with a
    /// next track, but no engine effect: the engine is already playing `next`.
    ///
    /// `from` is the track the caller observed as current. If the state moved
    /// on in the meantime (e.g. a skip), this is a no-op.
    pub fn track_advanced(&mut self, from: &ContentHash, next: ContentHash) -> Vec<PlaybackEffect> {
        let mut effects = Vec::new();

        if let PlaybackState::Playing { hash } = self.state.clone() {
            if hash != *from {
                return effects;
            }
            effects.push(PlaybackEffect::WriteFact {
                hash: hash.clone(),
                value: MusicValue::TrackStopped(StopReason::OnCompletion),
            });
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                hash,
            }));
            self.state = PlaybackState::Playing { hash: next.clone() };
            // Session continues.
            effects.push(PlaybackEffect::WriteFact {
                hash: next.clone(),
                value: MusicValue::TrackStarted(StartReason::ByQueue),
            });
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted {
                hash: next,
            }));
        }

        effects
    }

    /// Called when the engine reports a track has played to completion.
    ///
    /// `finished` is the track the caller observed as finished. If the state
    /// moved on in the meantime (e.g. a skip), this is a no-op.
    pub fn track_ended(
        &mut self,
        finished: &ContentHash,
        next: Option<(ContentHash, SourceName)>,
    ) -> Vec<PlaybackEffect> {
        let mut effects = Vec::new();

        match self.state.clone() {
            PlaybackState::Playing { hash } if hash == *finished => {
                effects.push(PlaybackEffect::WriteFact {
                    hash: hash.clone(),
                    value: MusicValue::TrackStopped(StopReason::OnCompletion),
                });
                effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                    hash: hash.clone(),
                }));

                match next {
                    Some((next_hash, next_source)) => {
                        self.state = PlaybackState::Playing {
                            hash: next_hash.clone(),
                        };
                        // Session continues — auto-advance does not create a new session.
                        effects.push(PlaybackEffect::LoadAndPlay {
                            hash: next_hash.clone(),
                            source: next_source,
                        });
                        effects.push(PlaybackEffect::WriteFact {
                            hash: next_hash.clone(),
                            value: MusicValue::TrackStarted(StartReason::ByQueue),
                        });
                        effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted {
                            hash: next_hash.clone(),
                        }));
                    }
                    None => {
                        self.state = PlaybackState::Idle;
                        self.maybe_end_session(&mut effects);
                    }
                }
            }
            // Stale observation, paused or idle: safe no-op.
            _ => {}
        }

        effects
    }
}

impl Default for PlaybackStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn hash_a() -> ContentHash {
        ContentHash::new("sha256:aaaaaa")
    }

    fn hash_b() -> ContentHash {
        ContentHash::new("sha256:bbbbbb")
    }

    fn source_a() -> SourceName {
        SourceName::audio()
    }

    fn source_b() -> SourceName {
        SourceName::audio()
    }

    // -------------------------------------------------------------------------
    // Initial state
    // -------------------------------------------------------------------------

    #[test]
    fn initial_state_is_idle() {
        let sm = PlaybackStateMachine::new();
        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(sm.current_hash(), None);
        assert!(!sm.is_playing());
        assert_eq!(sm.session_id(), None);
    }

    // -------------------------------------------------------------------------
    // play_queue transitions
    // -------------------------------------------------------------------------

    #[test]
    fn play_queue_from_idle_transitions_to_playing() {
        let mut sm = PlaybackStateMachine::new();
        let effects = sm.play_queue(hash_a(), source_a());

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_a()));
        assert_eq!(sm.current_hash(), Some(&hash_a()));
        assert!(sm.is_playing());

        // No StopEngine since we were Idle.
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == hash_a())));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStarted(StartReason::ByQueue) }
            if *hash == hash_a()
        )));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted { hash })
            if *hash == hash_a()
        )));
    }

    #[test]
    fn play_queue_from_playing_records_stop_and_loads_new() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.play_queue(hash_b(), source_b());

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));

        // No StopEngine: it would discard the preload; load cuts immediately.
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnRequest) }
            if *hash == hash_a()
        )));
        // Then load the new one.
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == hash_b())));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStarted(StartReason::ByQueue) }
            if *hash == hash_b()
        )));
    }

    #[test]
    fn play_queue_from_paused_records_stop_and_loads_new() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        assert!(matches!(sm.state(), PlaybackState::Paused { .. }));

        let effects = sm.play_queue(hash_b(), source_b());

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == hash_b())));
    }

    // -------------------------------------------------------------------------
    // stop transitions
    // -------------------------------------------------------------------------

    #[test]
    fn stop_from_playing_transitions_to_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.stop();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(sm.current_hash(), None);
        assert!(!sm.is_playing());

        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnRequest) }
            if *hash == hash_a()
        )));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped { hash })
            if *hash == hash_a()
        )));
    }

    #[test]
    fn stop_from_paused_transitions_to_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        let effects = sm.stop();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnRequest) }
            if *hash == hash_a()
        )));
    }

    #[test]
    fn stop_from_idle_is_noop() {
        let mut sm = PlaybackStateMachine::new();
        let effects = sm.stop();
        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(effects, vec![]);
    }

    // -------------------------------------------------------------------------
    // skip transitions
    // -------------------------------------------------------------------------

    #[test]
    fn skip_from_playing_with_next_transitions_to_playing_next() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.skip(Some((hash_b(), source_b())));

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
        // No StopEngine when a next track follows (would discard the preload).
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnSkip) }
            if *hash == hash_a()
        )));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == hash_b())));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStarted(StartReason::ByQueue) }
            if *hash == hash_b()
        )));
    }

    #[test]
    fn skip_from_playing_empty_queue_transitions_to_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.skip(None);

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnSkip) }
            if *hash == hash_a()
        )));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { .. })));
    }

    #[test]
    fn skip_from_paused_with_next_track() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        let effects = sm.skip(Some((hash_b(), source_b())));

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { .. })));
        // Must use OnSkip, not OnRequest.
        assert!(effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::WriteFact {
                value: MusicValue::TrackStopped(StopReason::OnSkip),
                ..
            }
        )));
    }

    #[test]
    fn skip_from_paused_empty_queue() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        let effects = sm.skip(None);

        assert_eq!(sm.state(), &PlaybackState::Idle);
        // Still emits stop effects.
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { .. })));
    }

    #[test]
    fn skip_from_idle_with_next_starts_playing() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.skip(Some((hash_a(), source_a())));

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_a()));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { .. })));
    }

    #[test]
    fn skip_from_idle_no_next_is_noop() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.skip(None);

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(effects, vec![]);
    }

    // -------------------------------------------------------------------------
    // pause transitions
    // -------------------------------------------------------------------------

    #[test]
    fn pause_from_playing_transitions_to_paused() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.pause();

        assert!(matches!(sm.state(), PlaybackState::Paused { hash } if *hash == hash_a()));
        assert!(!sm.is_playing());
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::PauseEngine)));
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::StopEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::EmitEvent(PlaybackEvent::TrackPaused { hash })
            if *hash == hash_a()
        )));
    }

    #[test]
    fn pause_from_paused_is_noop() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        let effects = sm.pause();

        assert!(matches!(sm.state(), PlaybackState::Paused { .. }));
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn pause_from_idle_is_noop() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.pause();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(effects, vec![]);
    }

    // -------------------------------------------------------------------------
    // resume transitions
    // -------------------------------------------------------------------------

    #[test]
    fn resume_from_paused_transitions_to_playing() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        let effects = sm.resume();

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_a()));
        assert!(sm.is_playing());
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::PlayEngine)));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::EmitEvent(PlaybackEvent::TrackResumed { hash })
            if *hash == hash_a()
        )));
    }

    #[test]
    fn resume_from_playing_is_noop() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.resume();

        assert!(matches!(sm.state(), PlaybackState::Playing { .. }));
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn resume_from_idle_is_noop() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.resume();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(effects, vec![]);
    }

    // -------------------------------------------------------------------------
    // track_ended transitions
    // -------------------------------------------------------------------------

    #[test]
    fn track_ended_from_playing_with_next_advances_queue() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.track_ended(&hash_a(), Some((hash_b(), source_b())));

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnCompletion) }
            if *hash == hash_a()
        )));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == hash_b())));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStarted(StartReason::ByQueue) }
            if *hash == hash_b()
        )));
    }

    #[test]
    fn track_ended_from_playing_no_next_transitions_to_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.track_ended(&hash_a(), None);

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnCompletion) }
            if *hash == hash_a()
        )));
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped { hash })
            if *hash == hash_a()
        )));
    }

    #[test]
    fn track_ended_from_paused_is_noop() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        let effects = sm.track_ended(&hash_a(), None);

        // Safe no-op — should not happen but handled gracefully.
        assert!(matches!(sm.state(), PlaybackState::Paused { .. }));
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn track_ended_from_idle_is_noop() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.track_ended(&hash_a(), None);

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn track_ended_for_a_track_that_is_no_longer_current_is_noop() {
        // A poll observed A finishing, but a skip moved us to B meanwhile.
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.skip(Some((hash_b(), source_b())));

        let effects = sm.track_ended(&hash_a(), None);

        assert_eq!(effects, vec![]);
        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
    }

    // -------------------------------------------------------------------------
    // track_advanced (gapless) transitions
    // -------------------------------------------------------------------------

    #[test]
    fn track_advanced_emits_same_facts_and_events_as_track_ended_without_engine_effect() {
        let mut gapless = PlaybackStateMachine::new();
        gapless.play_queue(hash_a(), source_a());
        let mut ended = PlaybackStateMachine::new();
        ended.play_queue(hash_a(), source_a());

        let advanced_effects = gapless.track_advanced(&hash_a(), hash_b());
        let ended_effects = ended.track_ended(&hash_a(), Some((hash_b(), source_b())));

        assert!(matches!(gapless.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
        // No engine effects at all: the engine already switched.
        assert!(!advanced_effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::LoadAndPlay { .. }
                | PlaybackEffect::StopEngine
                | PlaybackEffect::PauseEngine
                | PlaybackEffect::PlayEngine
        )));
        // Same as track_ended minus the LoadAndPlay.
        let ended_without_load: Vec<_> = ended_effects
            .into_iter()
            .filter(|e| !matches!(e, PlaybackEffect::LoadAndPlay { .. }))
            .collect();
        assert_eq!(advanced_effects, ended_without_load);
        assert!(advanced_effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnCompletion) }
            if *hash == hash_a()
        )));
        assert!(advanced_effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStarted(StartReason::ByQueue) }
            if *hash == hash_b()
        )));
    }

    #[test]
    fn track_advanced_keeps_session() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        let session = sm.session_id().cloned();

        let effects = sm.track_advanced(&hash_a(), hash_b());

        assert_eq!(sm.session_id(), session.as_ref());
        assert!(!effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionStarted { .. })
                | PlaybackEffect::EmitEvent(PlaybackEvent::SessionEnded { .. })
        )));
    }

    #[test]
    fn track_advanced_from_paused_or_idle_is_noop() {
        let mut sm = PlaybackStateMachine::new();
        assert_eq!(sm.track_advanced(&hash_a(), hash_b()), vec![]);
        assert_eq!(sm.state(), &PlaybackState::Idle);

        sm.play_queue(hash_a(), source_a());
        sm.pause();
        assert_eq!(sm.track_advanced(&hash_a(), hash_b()), vec![]);
        assert!(matches!(sm.state(), PlaybackState::Paused { hash } if *hash == hash_a()));
    }

    #[test]
    fn track_advanced_from_a_track_that_is_no_longer_current_is_noop() {
        // A stale poll saw B audible, but a skip already moved us to B:
        // no bogus TrackStopped(B)/TrackStarted(B).
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.skip(Some((hash_b(), source_b())));

        let effects = sm.track_advanced(&hash_a(), hash_b());

        assert_eq!(effects, vec![]);
        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_b()));
    }

    // -------------------------------------------------------------------------
    // Compound scenarios
    // -------------------------------------------------------------------------

    #[test]
    fn pause_resume_cycle_stays_on_same_track() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();
        sm.resume();

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_a()));
        assert_eq!(sm.current_hash(), Some(&hash_a()));
    }

    #[test]
    fn stop_from_playing_clears_current_hash() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.stop();

        assert_eq!(sm.current_hash(), None);
    }

    #[test]
    fn skip_from_paused_clears_paused_state() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());
        sm.pause();

        // After skip with no next, must be Idle — not Paused.
        sm.skip(None);

        assert_eq!(sm.state(), &PlaybackState::Idle);
        // Auto-advance (track_ended) should now be a no-op.
        let effects = sm.track_ended(&hash_a(), None);
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn play_queue_stop_fact_uses_on_request_reason() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let effects = sm.play_queue(hash_b(), source_b());

        // The stop fact for track A must use OnRequest (interrupted by new track).
        assert!(effects.iter().any(|e| matches!(e,
            PlaybackEffect::WriteFact { hash, value: MusicValue::TrackStopped(StopReason::OnRequest) }
            if *hash == hash_a()
        )));
    }

    // -------------------------------------------------------------------------
    // Session tests
    // -------------------------------------------------------------------------

    #[test]
    fn session_starts_on_first_play_queue_from_idle() {
        let mut sm = PlaybackStateMachine::new();
        assert_eq!(sm.session_id(), None);

        let effects = sm.play_queue(hash_a(), source_a());

        // Session is now set.
        assert!(sm.session_id().is_some());

        // SessionStarted event emitted.
        assert!(effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionStarted { .. })
        )));
    }

    #[test]
    fn session_does_not_restart_on_skip_to_next() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let first_session = sm.session_id().cloned().expect("session must be active");

        let effects = sm.skip(Some((hash_b(), source_b())));

        // Same session ID — not restarted.
        assert_eq!(sm.session_id(), Some(&first_session));

        // No SessionStarted emitted.
        assert!(!effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionStarted { .. })
        )));
        // No SessionEnded emitted.
        assert!(!effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionEnded { .. })
        )));
    }

    #[test]
    fn session_ends_on_stop_from_playing() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        assert!(sm.session_id().is_some());

        let effects = sm.stop();

        // Session cleared.
        assert_eq!(sm.session_id(), None);

        // SessionEnded emitted.
        assert!(effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionEnded { .. })
        )));
    }

    #[test]
    fn session_ends_when_queue_empties_on_track_ended() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        assert!(sm.session_id().is_some());

        let effects = sm.track_ended(&hash_a(), None);

        // Session cleared.
        assert_eq!(sm.session_id(), None);
        assert_eq!(sm.state(), &PlaybackState::Idle);

        // SessionEnded emitted.
        assert!(effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionEnded { .. })
        )));
    }

    #[test]
    fn session_persists_across_pause_and_resume() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        let session_before = sm.session_id().cloned().expect("session must be active");

        sm.pause();
        assert_eq!(sm.session_id(), Some(&session_before));

        sm.resume();
        assert_eq!(sm.session_id(), Some(&session_before));
    }

    #[test]
    fn session_ends_on_skip_with_empty_queue() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(hash_a(), source_a());

        assert!(sm.session_id().is_some());

        let effects = sm.skip(None);

        assert_eq!(sm.session_id(), None);
        assert_eq!(sm.state(), &PlaybackState::Idle);

        assert!(effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionEnded { .. })
        )));
    }

    #[test]
    fn new_session_starts_after_previous_ended() {
        let mut sm = PlaybackStateMachine::new();

        // First session.
        sm.play_queue(hash_a(), source_a());
        let first_id = sm.session_id().cloned().expect("session active");
        sm.stop();
        assert_eq!(sm.session_id(), None);

        // Second session.
        let effects = sm.play_queue(hash_b(), source_b());
        let second_id = sm.session_id().cloned().expect("new session active");

        // The IDs must differ (different timestamps).
        assert_ne!(first_id.as_str(), second_id.as_str());

        assert!(effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::EmitEvent(PlaybackEvent::SessionStarted { .. })
        )));
    }

    #[test]
    fn session_id_emitted_in_session_started_event_matches_accessor() {
        let mut sm = PlaybackStateMachine::new();
        let effects = sm.play_queue(hash_a(), source_a());

        let session_id = sm.session_id().expect("session active");

        // The event must carry the same ID that the accessor returns.
        let event_id = effects.iter().find_map(|e| {
            if let PlaybackEffect::EmitEvent(PlaybackEvent::SessionStarted { id }) = e {
                Some(id.clone())
            } else {
                None
            }
        });

        assert_eq!(event_id, Some(session_id.clone()));
    }
}
