use event_protocol::PlaybackEvent;
use media_protocol::{CanonicalHash, Transport};
use music_facts::{MusicValue, StartReason, StopReason};
use playback_primitives::{PositionMs, SessionId, SourceName};

// Playback state and play facts hold only canonical hashes (ADR-026); events
// carry the wire `ContentHash`, converted from them at the point of emission.

/// Why a transition did not happen. The caller answers the command with the
/// message; nothing else has happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A preview runs: `skip` and `play_queue` wait for Back (ADR-031).
    Previewing,
    /// The source did not say where the audible track is, so a preview could
    /// not hold it.
    PositionUnknown,
    /// `play_queue` with nothing queued.
    QueueEmpty,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Previewing => "Previewing: go back first",
            Refusal::PositionUnknown => {
                "Cannot preview: the source does not say where the playing track is"
            }
            Refusal::QueueEmpty => "Queue is empty",
        })
    }
}

/// What the source said about its audible track: which, and where in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub hash: CanonicalHash,
    pub at: PositionMs,
}

/// What a preview goes back to: a track, or nothing. It cannot hold a
/// preview, so a preview of a preview is not a state that can be built.
#[derive(Debug, Clone, PartialEq)]
pub enum BackTo {
    /// The preview started over nothing; going back stops.
    Nothing,
    /// The track that was audible: where it was and whether it played.
    Track {
        hash: CanonicalHash,
        at: PositionMs,
        transport: Transport,
    },
}

impl BackTo {
    pub fn hash(&self) -> Option<&CanonicalHash> {
        match self {
            BackTo::Nothing => None,
            BackTo::Track { hash, .. } => Some(hash),
        }
    }

    fn into_hash(self) -> Option<CanonicalHash> {
        match self {
            BackTo::Nothing => None,
            BackTo::Track { hash, .. } => Some(hash),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackState {
    Idle,
    Playing {
        hash: CanonicalHash,
    },
    Paused {
        hash: CanonicalHash,
    },
    /// `hash` is audible as a preview over what `back_to` holds.
    Previewing {
        hash: CanonicalHash,
        transport: Transport,
        back_to: BackTo,
    },
}

impl PlaybackState {
    /// `hash` as the audible track with `transport`.
    fn track(hash: CanonicalHash, transport: Transport) -> Self {
        match transport {
            Transport::Playing => PlaybackState::Playing { hash },
            Transport::Paused => PlaybackState::Paused { hash },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackEffect {
    StopEngine,
    PauseEngine,
    PlayEngine,
    LoadAndPlay {
        hash: CanonicalHash,
        source: SourceName,
    },
    /// Start `hash` at `at`, then play it if `then` says so: load, seek, play.
    /// The one way to start a track anywhere but its start.
    LoadAt {
        hash: CanonicalHash,
        at: PositionMs,
        then: Transport,
    },
    EmitEvent(PlaybackEvent),
    WriteFact {
        hash: CanonicalHash,
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

    /// The audible track: the preview while one runs.
    pub fn current_hash(&self) -> Option<&CanonicalHash> {
        match &self.state {
            PlaybackState::Idle => None,
            PlaybackState::Playing { hash }
            | PlaybackState::Paused { hash }
            | PlaybackState::Previewing { hash, .. } => Some(hash),
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
    ///
    /// `next` is the queue front. Refused while previewing (whatever the queue
    /// holds) and when there is none.
    pub fn play_queue(
        &mut self,
        next: Option<(CanonicalHash, SourceName)>,
    ) -> Result<Vec<PlaybackEffect>, Refusal> {
        let stopped = match self.state.clone() {
            PlaybackState::Previewing { .. } => return Err(Refusal::Previewing),
            PlaybackState::Playing { hash } | PlaybackState::Paused { hash } => Some(hash),
            PlaybackState::Idle => None,
        };
        let (hash, source) = next.ok_or(Refusal::QueueEmpty)?;
        let mut effects = Vec::new();

        // Record the stop of whatever is currently playing/paused.
        if let Some(h) = stopped {
            // No StopEngine: LoadAndPlay below cuts immediately, and an
            // explicit stop would discard the preloaded next track.
            effects.push(PlaybackEffect::WriteFact {
                hash: h.clone(),
                value: MusicValue::TrackStopped(StopReason::OnRequest),
            });
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                hash: h.into(),
            }));
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
            hash: hash.into(),
        }));

        Ok(effects)
    }

    /// Stop everything explicitly (user request). While previewing that drops
    /// the hold: the held track's play is what ends, the preview was never one.
    pub fn stop(&mut self) -> Vec<PlaybackEffect> {
        let (played, preview) = match std::mem::replace(&mut self.state, PlaybackState::Idle) {
            PlaybackState::Idle => return vec![],
            PlaybackState::Playing { hash } | PlaybackState::Paused { hash } => (Some(hash), None),
            PlaybackState::Previewing { hash, back_to, .. } => (back_to.into_hash(), Some(hash)),
        };
        let mut effects = vec![PlaybackEffect::StopEngine];
        if let Some(hash) = played {
            effects.push(PlaybackEffect::WriteFact {
                hash: hash.clone(),
                value: MusicValue::TrackStopped(StopReason::OnRequest),
            });
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                hash: hash.into(),
            }));
        }
        if let Some(hash) = preview {
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::PreviewEnded {
                hash: hash.into(),
                resumed: None,
            }));
        }
        self.maybe_end_session(&mut effects);
        effects
    }

    /// Preview `hash` over what is audible, without losing it. `observed` is
    /// what the source says about its audible track. A playing or paused
    /// track is held only if `observed` is about that very track, otherwise
    /// the preview is refused: going back would not know where to go. Idle
    /// and a running preview ignore it (nothing to hold, and a second preview
    /// replaces the first and keeps what the first held). A preview is not a
    /// play: no facts, no play events, no session.
    pub fn preview(
        &mut self,
        hash: CanonicalHash,
        observed: Option<Observed>,
    ) -> Result<Vec<PlaybackEffect>, Refusal> {
        let hold = |held: CanonicalHash, transport| match &observed {
            Some(o) if o.hash == held => Ok(BackTo::Track {
                hash: held,
                at: o.at,
                transport,
            }),
            _ => Err(Refusal::PositionUnknown),
        };
        let back_to = match self.state.clone() {
            PlaybackState::Idle => BackTo::Nothing,
            PlaybackState::Playing { hash } => hold(hash, Transport::Playing)?,
            PlaybackState::Paused { hash } => hold(hash, Transport::Paused)?,
            PlaybackState::Previewing { back_to, .. } => back_to,
        };
        let held = back_to.hash().map(Into::into);
        self.state = PlaybackState::Previewing {
            hash: hash.clone(),
            transport: Transport::Playing,
            back_to,
        };
        Ok(vec![
            PlaybackEffect::LoadAndPlay {
                hash: hash.clone(),
                // Phase 4: single source, always mdma-audio.
                source: SourceName::audio(),
            },
            PlaybackEffect::EmitEvent(PlaybackEvent::PreviewStarted {
                hash: hash.into(),
                back_to: held,
            }),
        ])
    }

    /// End the preview and restore what it held: the track at its position and
    /// transport, or a stop. Nothing happens when no preview runs.
    pub fn end_preview(&mut self) -> Vec<PlaybackEffect> {
        let PlaybackState::Previewing { hash, back_to, .. } = self.state.clone() else {
            return vec![];
        };
        let (resumed, mut effects) = match back_to {
            BackTo::Nothing => {
                self.state = PlaybackState::Idle;
                (None, vec![PlaybackEffect::StopEngine])
            }
            BackTo::Track {
                hash: held,
                at,
                transport,
            } => {
                self.state = PlaybackState::track(held.clone(), transport);
                (
                    Some(held.clone().into()),
                    vec![PlaybackEffect::LoadAt {
                        hash: held,
                        at,
                        then: transport,
                    }],
                )
            }
        };
        effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::PreviewEnded {
            hash: hash.into(),
            resumed,
        }));
        effects
    }

    /// Called when the engine reports the preview played to its end: playback
    /// goes back by itself.
    ///
    /// `finished` is the track the caller observed as finished. If the preview
    /// moved on in the meantime (replaced, ended), this is a no-op.
    pub fn preview_finished(&mut self, finished: &CanonicalHash) -> Vec<PlaybackEffect> {
        match &self.state {
            PlaybackState::Previewing { hash, .. } if hash == finished => self.end_preview(),
            _ => vec![],
        }
    }

    /// Skip the current track and optionally start the next. Refused while
    /// previewing.
    pub fn skip(
        &mut self,
        next: Option<(CanonicalHash, SourceName)>,
    ) -> Result<Vec<PlaybackEffect>, Refusal> {
        let mut effects = Vec::new();

        match self.state.clone() {
            PlaybackState::Previewing { .. } => return Err(Refusal::Previewing),
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
                    hash: h.into(),
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
                            hash: next_hash.into(),
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
                        hash: next_hash.into(),
                    }));
                }
                // Idle + None => no-op
            }
        }

        Ok(effects)
    }

    /// Pause a playing track, or the preview.
    pub fn pause(&mut self) -> Vec<PlaybackEffect> {
        match self.state.clone() {
            PlaybackState::Playing { hash } => {
                self.state = PlaybackState::Paused { hash: hash.clone() };
                vec![
                    PlaybackEffect::PauseEngine,
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackPaused { hash: hash.into() }),
                ]
            }
            PlaybackState::Previewing {
                hash,
                transport: Transport::Playing,
                back_to,
            } => {
                self.state = PlaybackState::Previewing {
                    hash: hash.clone(),
                    transport: Transport::Paused,
                    back_to,
                };
                vec![
                    PlaybackEffect::PauseEngine,
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackPaused { hash: hash.into() }),
                ]
            }
            PlaybackState::Paused { .. }
            | PlaybackState::Previewing { .. }
            | PlaybackState::Idle => vec![],
        }
    }

    /// Resume a paused track, or the preview.
    pub fn resume(&mut self) -> Vec<PlaybackEffect> {
        match self.state.clone() {
            PlaybackState::Paused { hash } => {
                self.state = PlaybackState::Playing { hash: hash.clone() };
                vec![
                    PlaybackEffect::PlayEngine,
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackResumed { hash: hash.into() }),
                ]
            }
            PlaybackState::Previewing {
                hash,
                transport: Transport::Paused,
                back_to,
            } => {
                self.state = PlaybackState::Previewing {
                    hash: hash.clone(),
                    transport: Transport::Playing,
                    back_to,
                };
                vec![
                    PlaybackEffect::PlayEngine,
                    PlaybackEffect::EmitEvent(PlaybackEvent::TrackResumed { hash: hash.into() }),
                ]
            }
            PlaybackState::Playing { .. }
            | PlaybackState::Previewing { .. }
            | PlaybackState::Idle => vec![],
        }
    }

    /// Called when the engine switched to the preloaded next track on its own
    /// (gapless advance). Emits the same facts/events as `track_ended` with a
    /// next track, but no engine effect: the engine is already playing `next`.
    ///
    /// `from` is the track the caller observed as current. If the state moved
    /// on in the meantime (e.g. a skip), this is a no-op.
    pub fn track_advanced(
        &mut self,
        from: &CanonicalHash,
        next: CanonicalHash,
    ) -> Vec<PlaybackEffect> {
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
                hash: hash.into(),
            }));
            self.state = PlaybackState::Playing { hash: next.clone() };
            // Session continues.
            effects.push(PlaybackEffect::WriteFact {
                hash: next.clone(),
                value: MusicValue::TrackStarted(StartReason::ByQueue),
            });
            effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStarted {
                hash: next.into(),
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
        finished: &CanonicalHash,
        next: Option<(CanonicalHash, SourceName)>,
    ) -> Vec<PlaybackEffect> {
        let mut effects = Vec::new();

        match self.state.clone() {
            PlaybackState::Playing { hash } if hash == *finished => {
                effects.push(PlaybackEffect::WriteFact {
                    hash: hash.clone(),
                    value: MusicValue::TrackStopped(StopReason::OnCompletion),
                });
                effects.push(PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                    hash: hash.into(),
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
                            hash: next_hash.into(),
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

    fn hash_a() -> CanonicalHash {
        CanonicalHash::try_from(media_protocol::ContentHash::new(format!(
            "sha256:{}",
            "a".repeat(64)
        )))
        .unwrap()
    }

    fn hash_b() -> CanonicalHash {
        CanonicalHash::try_from(media_protocol::ContentHash::new(format!(
            "sha256:{}",
            "b".repeat(64)
        )))
        .unwrap()
    }

    fn hash_c() -> CanonicalHash {
        CanonicalHash::try_from(media_protocol::ContentHash::new(format!(
            "sha256:{}",
            "c".repeat(64)
        )))
        .unwrap()
    }

    fn queued(hash: CanonicalHash) -> Option<(CanonicalHash, SourceName)> {
        Some((hash, source_a()))
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
        let effects = sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

        let effects = sm.play_queue(queued(hash_b())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();

        assert!(matches!(sm.state(), PlaybackState::Paused { .. }));

        let effects = sm.play_queue(queued(hash_b())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
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
        sm.play_queue(queued(hash_a())).unwrap();

        let effects = sm.skip(Some((hash_b(), source_b()))).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

        let effects = sm.skip(None).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();

        let effects = sm.skip(Some((hash_b(), source_b()))).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();

        let effects = sm.skip(None).unwrap();

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

        let effects = sm.skip(Some((hash_a(), source_a()))).unwrap();

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_a()));
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { .. })));
    }

    #[test]
    fn skip_from_idle_no_next_is_noop() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.skip(None).unwrap();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(effects, vec![]);
    }

    // -------------------------------------------------------------------------
    // pause transitions
    // -------------------------------------------------------------------------

    #[test]
    fn pause_from_playing_transitions_to_paused() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
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
        sm.play_queue(queued(hash_a())).unwrap();
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
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
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
        sm.play_queue(queued(hash_a())).unwrap();
        sm.skip(Some((hash_b(), source_b()))).unwrap();

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
        gapless.play_queue(queued(hash_a())).unwrap();
        let mut ended = PlaybackStateMachine::new();
        ended.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
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

        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();
        assert_eq!(sm.track_advanced(&hash_a(), hash_b()), vec![]);
        assert!(matches!(sm.state(), PlaybackState::Paused { hash } if *hash == hash_a()));
    }

    #[test]
    fn track_advanced_from_a_track_that_is_no_longer_current_is_noop() {
        // A stale poll saw B audible, but a skip already moved us to B:
        // no bogus TrackStopped(B)/TrackStarted(B).
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        sm.skip(Some((hash_b(), source_b()))).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();
        sm.resume();

        assert!(matches!(sm.state(), PlaybackState::Playing { hash } if *hash == hash_a()));
        assert_eq!(sm.current_hash(), Some(&hash_a()));
    }

    #[test]
    fn stop_from_playing_clears_current_hash() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        sm.stop();

        assert_eq!(sm.current_hash(), None);
    }

    #[test]
    fn skip_from_paused_clears_paused_state() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();

        // After skip with no next, must be Idle — not Paused.
        sm.skip(None).unwrap();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        // Auto-advance (track_ended) should now be a no-op.
        let effects = sm.track_ended(&hash_a(), None);
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn play_queue_stop_fact_uses_on_request_reason() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();

        let effects = sm.play_queue(queued(hash_b())).unwrap();

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

        let effects = sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

        let first_session = sm.session_id().cloned().expect("session must be active");

        let effects = sm.skip(Some((hash_b(), source_b()))).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();

        let session_before = sm.session_id().cloned().expect("session must be active");

        sm.pause();
        assert_eq!(sm.session_id(), Some(&session_before));

        sm.resume();
        assert_eq!(sm.session_id(), Some(&session_before));
    }

    #[test]
    fn session_ends_on_skip_with_empty_queue() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();

        assert!(sm.session_id().is_some());

        let effects = sm.skip(None).unwrap();

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
        sm.play_queue(queued(hash_a())).unwrap();
        let first_id = sm.session_id().cloned().expect("session active");
        sm.stop();
        assert_eq!(sm.session_id(), None);

        // Second session.
        let effects = sm.play_queue(queued(hash_b())).unwrap();
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
        let effects = sm.play_queue(queued(hash_a())).unwrap();

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

    // -------------------------------------------------------------------------
    // Preview hold (ADR-031)
    // -------------------------------------------------------------------------

    fn at(ms: u64) -> PositionMs {
        PositionMs::new(ms)
    }

    fn held(hash: CanonicalHash, ms: u64, transport: Transport) -> BackTo {
        BackTo::Track {
            hash,
            at: at(ms),
            transport,
        }
    }

    fn started(hash: CanonicalHash, back_to: Option<CanonicalHash>) -> PlaybackEffect {
        PlaybackEffect::EmitEvent(PlaybackEvent::PreviewStarted {
            hash: hash.into(),
            back_to: back_to.map(Into::into),
        })
    }

    fn ended(hash: CanonicalHash, resumed: Option<CanonicalHash>) -> PlaybackEffect {
        PlaybackEffect::EmitEvent(PlaybackEvent::PreviewEnded {
            hash: hash.into(),
            resumed: resumed.map(Into::into),
        })
    }

    /// `a` is audible at 1:30 and `b` is previewed over it.
    fn previewing_over_a(transport: Transport) -> PlaybackStateMachine {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        if transport == Transport::Paused {
            sm.pause();
        }
        sm.preview(hash_b(), seen(hash_a(), 90_000)).unwrap();
        sm
    }

    /// What the source says: `hash` is audible at `ms`.
    fn seen(hash: CanonicalHash, ms: u64) -> Option<Observed> {
        Some(Observed { hash, at: at(ms) })
    }

    #[test]
    fn preview_from_idle_holds_nothing() {
        let mut sm = PlaybackStateMachine::new();

        let effects = sm.preview(hash_b(), None).unwrap();

        assert_eq!(
            sm.state(),
            &PlaybackState::Previewing {
                hash: hash_b(),
                transport: Transport::Playing,
                back_to: BackTo::Nothing,
            }
        );
        assert_eq!(
            effects,
            vec![
                PlaybackEffect::LoadAndPlay {
                    hash: hash_b(),
                    source: source_b()
                },
                started(hash_b(), None),
            ]
        );
    }

    #[test]
    fn preview_from_playing_holds_the_track_its_position_and_that_it_played() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();

        let effects = sm.preview(hash_b(), seen(hash_a(), 90_000)).unwrap();

        assert_eq!(
            sm.state(),
            &PlaybackState::Previewing {
                hash: hash_b(),
                transport: Transport::Playing,
                back_to: held(hash_a(), 90_000, Transport::Playing),
            }
        );
        assert_eq!(
            effects,
            vec![
                PlaybackEffect::LoadAndPlay {
                    hash: hash_b(),
                    source: source_b()
                },
                started(hash_b(), Some(hash_a())),
            ]
        );
    }

    #[test]
    fn preview_from_paused_holds_that_it_was_paused() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        sm.pause();

        sm.preview(hash_b(), seen(hash_a(), 12_000)).unwrap();

        assert_eq!(
            sm.state(),
            &PlaybackState::Previewing {
                hash: hash_b(),
                transport: Transport::Playing,
                back_to: held(hash_a(), 12_000, Transport::Paused),
            }
        );
    }

    #[test]
    fn previewing_again_replaces_the_preview_and_keeps_what_is_held() {
        let mut sm = previewing_over_a(Transport::Playing);

        // What is observed now is the first preview's, not the held track's.
        let effects = sm.preview(hash_c(), seen(hash_b(), 5_000)).unwrap();

        assert_eq!(
            sm.state(),
            &PlaybackState::Previewing {
                hash: hash_c(),
                transport: Transport::Playing,
                back_to: held(hash_a(), 90_000, Transport::Playing),
            }
        );
        assert_eq!(
            effects,
            vec![
                PlaybackEffect::LoadAndPlay {
                    hash: hash_c(),
                    source: source_a()
                },
                started(hash_c(), Some(hash_a())),
            ]
        );
    }

    #[test]
    fn previewing_again_over_nothing_still_holds_nothing() {
        let mut sm = PlaybackStateMachine::new();
        sm.preview(hash_b(), None).unwrap();

        sm.preview(hash_c(), seen(hash_b(), 7_000)).unwrap();

        assert!(matches!(
            sm.state(),
            PlaybackState::Previewing {
                back_to: BackTo::Nothing,
                ..
            }
        ));
    }

    #[test]
    fn back_restores_the_held_position_and_a_playing_transport() {
        let mut sm = previewing_over_a(Transport::Playing);

        let effects = sm.end_preview();

        assert_eq!(sm.state(), &PlaybackState::Playing { hash: hash_a() });
        assert_eq!(
            effects,
            vec![
                PlaybackEffect::LoadAt {
                    hash: hash_a(),
                    at: at(90_000),
                    then: Transport::Playing
                },
                ended(hash_b(), Some(hash_a())),
            ]
        );
    }

    #[test]
    fn back_restores_a_paused_transport() {
        let mut sm = previewing_over_a(Transport::Paused);

        let effects = sm.end_preview();

        assert_eq!(sm.state(), &PlaybackState::Paused { hash: hash_a() });
        assert!(effects.contains(&PlaybackEffect::LoadAt {
            hash: hash_a(),
            at: at(90_000),
            then: Transport::Paused
        }));
    }

    #[test]
    fn back_from_a_preview_over_nothing_stops() {
        let mut sm = PlaybackStateMachine::new();
        sm.preview(hash_b(), None).unwrap();

        let effects = sm.end_preview();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(
            effects,
            vec![PlaybackEffect::StopEngine, ended(hash_b(), None)]
        );
    }

    #[test]
    fn ending_a_preview_when_none_runs_is_a_noop() {
        let mut idle = PlaybackStateMachine::new();
        assert_eq!(idle.end_preview(), vec![]);
        assert_eq!(idle.state(), &PlaybackState::Idle);

        let mut playing = PlaybackStateMachine::new();
        playing.play_queue(queued(hash_a())).unwrap();
        assert_eq!(playing.end_preview(), vec![]);
        assert_eq!(playing.state(), &PlaybackState::Playing { hash: hash_a() });
    }

    #[test]
    fn the_natural_end_of_the_preview_goes_back_by_itself() {
        let mut natural = previewing_over_a(Transport::Playing);
        let mut by_hand = previewing_over_a(Transport::Playing);

        let effects = natural.preview_finished(&hash_b());

        assert_eq!(effects, by_hand.end_preview());
        assert_eq!(natural.state(), &PlaybackState::Playing { hash: hash_a() });
    }

    #[test]
    fn a_stale_end_of_a_replaced_preview_does_nothing() {
        let mut sm = previewing_over_a(Transport::Playing);
        sm.preview(hash_c(), None).unwrap();

        // The poll saw `b` finish, but `c` is the preview now.
        let effects = sm.preview_finished(&hash_b());

        assert_eq!(effects, vec![]);
        assert!(matches!(
            sm.state(),
            PlaybackState::Previewing { hash, .. } if *hash == hash_c()
        ));
    }

    #[test]
    fn stop_while_previewing_drops_the_hold_and_stops_the_held_track() {
        let mut sm = previewing_over_a(Transport::Playing);

        let effects = sm.stop();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert_eq!(sm.session_id(), None);
        assert!(effects.contains(&PlaybackEffect::StopEngine));
        assert!(effects.contains(&PlaybackEffect::WriteFact {
            hash: hash_a(),
            value: MusicValue::TrackStopped(StopReason::OnRequest),
        }));
        assert!(
            effects.contains(&PlaybackEffect::EmitEvent(PlaybackEvent::TrackStopped {
                hash: hash_a().into()
            }))
        );
        assert!(effects.contains(&ended(hash_b(), None)));
        // Nothing is said about the preview as a played track.
        assert!(!effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::WriteFact { hash, .. } if *hash == hash_b()
        )));
    }

    #[test]
    fn stop_while_previewing_over_nothing_writes_no_facts() {
        let mut sm = PlaybackStateMachine::new();
        sm.preview(hash_b(), None).unwrap();

        let effects = sm.stop();

        assert_eq!(sm.state(), &PlaybackState::Idle);
        assert!(!effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::WriteFact { .. })));
    }

    #[test]
    fn skip_while_previewing_is_refused_and_changes_nothing() {
        let mut sm = previewing_over_a(Transport::Playing);
        let before = sm.state().clone();

        let with_next = sm.skip(Some((hash_c(), source_a())));
        let without_next = sm.skip(None);

        assert_eq!(with_next, Err(Refusal::Previewing));
        assert_eq!(without_next, Err(Refusal::Previewing));
        assert_eq!(sm.state(), &before);
    }

    #[test]
    fn play_queue_while_previewing_is_refused_whatever_the_queue_holds() {
        let mut sm = previewing_over_a(Transport::Playing);
        let before = sm.state().clone();

        assert_eq!(sm.play_queue(queued(hash_c())), Err(Refusal::Previewing));
        assert_eq!(sm.play_queue(None), Err(Refusal::Previewing));
        assert_eq!(sm.state(), &before);
    }

    #[test]
    fn play_queue_with_nothing_queued_is_refused_as_empty() {
        let mut idle = PlaybackStateMachine::new();
        assert_eq!(idle.play_queue(None), Err(Refusal::QueueEmpty));
        assert_eq!(idle.state(), &PlaybackState::Idle);

        let mut playing = PlaybackStateMachine::new();
        playing.play_queue(queued(hash_a())).unwrap();
        assert_eq!(playing.play_queue(None), Err(Refusal::QueueEmpty));
        assert_eq!(playing.state(), &PlaybackState::Playing { hash: hash_a() });
    }

    #[test]
    fn the_refusals_say_what_to_do() {
        assert_eq!(Refusal::Previewing.to_string(), "Previewing: go back first");
        assert_eq!(Refusal::QueueEmpty.to_string(), "Queue is empty");
        assert_eq!(
            Refusal::PositionUnknown.to_string(),
            "Cannot preview: the source does not say where the playing track is"
        );
    }

    #[test]
    fn a_preview_over_a_track_the_source_does_not_place_is_refused_and_changes_nothing() {
        for transport in [Transport::Playing, Transport::Paused] {
            let mut sm = PlaybackStateMachine::new();
            sm.play_queue(queued(hash_a())).unwrap();
            if transport == Transport::Paused {
                sm.pause();
            }
            let before = sm.state().clone();

            assert_eq!(sm.preview(hash_b(), None), Err(Refusal::PositionUnknown));
            assert_eq!(sm.state(), &before);
        }
    }

    /// The source answered about another track than the one playback has as
    /// audible: a gapless switch landed between asking and previewing.
    #[test]
    fn a_preview_on_an_observation_of_another_track_is_refused_and_changes_nothing() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        let before = sm.state().clone();

        assert_eq!(
            sm.preview(hash_c(), seen(hash_b(), 5_000)),
            Err(Refusal::PositionUnknown)
        );
        assert_eq!(sm.state(), &before);
    }

    #[test]
    fn a_preview_over_idle_or_a_preview_needs_no_observation() {
        let mut idle = PlaybackStateMachine::new();
        assert!(idle.preview(hash_b(), None).is_ok());
        assert!(
            idle.preview(hash_c(), None).is_ok(),
            "a preview over a preview"
        );
    }

    #[test]
    fn previews_write_no_play_facts_and_no_play_events() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        let mut effects = sm.preview(hash_b(), seen(hash_a(), 90_000)).unwrap();
        effects.extend(sm.preview(hash_c(), None).unwrap());
        effects.extend(sm.pause());
        effects.extend(sm.resume());
        effects.extend(sm.end_preview());

        assert!(!effects.iter().any(|e| matches!(
            e,
            PlaybackEffect::WriteFact { .. }
                | PlaybackEffect::EmitEvent(
                    PlaybackEvent::TrackStarted { .. }
                        | PlaybackEvent::TrackStopped { .. }
                        | PlaybackEvent::SessionStarted { .. }
                        | PlaybackEvent::SessionEnded { .. }
                )
        )));
    }

    #[test]
    fn a_preview_from_idle_starts_no_session() {
        let mut sm = PlaybackStateMachine::new();

        sm.preview(hash_b(), None).unwrap();

        assert_eq!(sm.session_id(), None);
    }

    #[test]
    fn the_session_of_the_held_track_survives_a_preview_and_the_way_back() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(queued(hash_a())).unwrap();
        let session = sm.session_id().cloned();

        sm.preview(hash_b(), seen(hash_a(), 1_000)).unwrap();
        assert_eq!(sm.session_id(), session.as_ref());
        sm.end_preview();
        assert_eq!(sm.session_id(), session.as_ref());
    }

    #[test]
    fn pause_and_resume_act_on_the_preview_and_keep_what_is_held() {
        let mut sm = previewing_over_a(Transport::Playing);

        let paused = sm.pause();
        assert_eq!(
            sm.state(),
            &PlaybackState::Previewing {
                hash: hash_b(),
                transport: Transport::Paused,
                back_to: held(hash_a(), 90_000, Transport::Playing),
            }
        );
        assert!(paused.contains(&PlaybackEffect::PauseEngine));

        let resumed = sm.resume();
        assert!(matches!(
            sm.state(),
            PlaybackState::Previewing {
                transport: Transport::Playing,
                ..
            }
        ));
        assert!(resumed.contains(&PlaybackEffect::PlayEngine));
    }

    #[test]
    fn a_paused_preview_that_is_previewed_again_plays() {
        let mut sm = previewing_over_a(Transport::Playing);
        sm.pause();

        sm.preview(hash_c(), None).unwrap();

        assert!(matches!(
            sm.state(),
            PlaybackState::Previewing {
                transport: Transport::Playing,
                ..
            }
        ));
    }

    #[test]
    fn the_audible_track_is_the_preview() {
        let sm = previewing_over_a(Transport::Playing);
        assert_eq!(sm.current_hash(), Some(&hash_b()));
    }

    #[test]
    fn the_queue_cannot_advance_a_preview() {
        let mut sm = previewing_over_a(Transport::Playing);

        assert_eq!(
            sm.track_ended(&hash_b(), Some((hash_c(), source_a()))),
            vec![]
        );
        assert_eq!(sm.track_advanced(&hash_b(), hash_c()), vec![]);
        assert!(matches!(sm.state(), PlaybackState::Previewing { .. }));
    }
}
