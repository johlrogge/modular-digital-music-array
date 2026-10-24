use crate::error::ServerError;
use crate::playback_state::{PlaybackEffect, PlaybackState, PlaybackStateMachine};
use acid_client::AcidClient;
use color_eyre::Result;
use event_protocol::{to_topic_message, PlaybackEvent};
use media_protocol::{Command, Response, ResponseData, SourceName};
use music_facts::{FactOrigin, FactSource, MusicValue};
use nng::Socket;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use stream_source_protocol::StreamPlaybackState;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::stream_client::StreamClient;

struct QueueEntry {
    hash: media_protocol::ContentHash,
    source: SourceName,
}

fn write_fact(acid_client: &AcidClient, hash: &media_protocol::ContentHash, value: MusicValue) {
    let source = FactSource::new(
        "mdma-playback",
        env!("CARGO_PKG_VERSION"),
        FactOrigin::Unknown,
    );
    if let Err(e) = acid_client.write_music_facts(hash, &[(value, source)]) {
        warn!("Failed to write fact via ACID: {}", e);
    }
}

/// Serializable form of a queue entry for persistence.
#[derive(Serialize, Deserialize)]
struct PersistEntry {
    hash: String,
    source: SourceName,
}

pub struct Server {
    audio: Arc<Mutex<StreamClient>>,
    socket: Socket,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    /// State machine that owns the canonical playback state (replaces the old
    /// `current_hash` and `is_paused` boolean flags).
    state: Arc<Mutex<PlaybackStateMachine>>,
    queue_file: PathBuf,
    event_pub: Socket,
    acid_client: Arc<AcidClient>,
}

impl Server {
    pub fn new(
        audio: Arc<Mutex<StreamClient>>,
        socket: Socket,
        queue_file: PathBuf,
        event_pub: Socket,
        acid_client: Arc<AcidClient>,
    ) -> Self {
        Self {
            audio,
            socket,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            state: Arc::new(Mutex::new(PlaybackStateMachine::new())),
            queue_file,
            event_pub,
            acid_client,
        }
    }

    /// Persist the queue and announce the change (call after every queue edit).
    fn queue_changed(&self, queue: &VecDeque<QueueEntry>) {
        persist_and_publish(&self.queue_file, &self.event_pub, queue);
    }

    /// Pop the queue front, persisting and announcing the change if it popped.
    fn pop_next(&self, queue: &mut VecDeque<QueueEntry>) -> Option<QueueEntry> {
        pop_front_and_publish(&self.queue_file, &self.event_pub, queue)
    }

    /// Load the queue from disk. Returns an empty queue on first start or corruption.
    async fn load_queue(&self) -> VecDeque<QueueEntry> {
        let data = match std::fs::read(&self.queue_file) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return VecDeque::new();
            }
            Err(e) => {
                warn!("Failed to read queue file {:?}: {}", self.queue_file, e);
                return VecDeque::new();
            }
        };

        let entries: Vec<PersistEntry> = match serde_json::from_slice(&data) {
            Ok(e) => e,
            Err(e) => {
                warn!("Queue file corrupt ({}), discarding", e);
                let _ = std::fs::remove_file(&self.queue_file);
                return VecDeque::new();
            }
        };

        let mut queue = VecDeque::new();
        for e in entries {
            queue.push_back(QueueEntry {
                hash: media_protocol::ContentHash::new(e.hash),
                source: e.source,
            });
        }
        info!("Restored {} track(s) from queue file", queue.len());
        queue
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        info!("Playback server starting...");

        // Restore queue from disk.
        let restored = self.load_queue().await;
        *self.queue.lock().await = restored;

        // Background task: auto-advance to next queued track when current track finishes.
        tokio::spawn(auto_advance_task(
            self.audio.clone(),
            self.queue.clone(),
            self.state.clone(),
            self.event_pub.clone(),
            self.acid_client.clone(),
            self.queue_file.clone(),
        ));

        loop {
            // Receive command
            let msg = self.socket.recv().map_err(ServerError::from)?;
            let command: Command = serde_json::from_slice(&msg)?;

            info!("Received command: {:?}", command);

            // Process command
            let response = self.handle_command(command).await;

            info!("Handled command, response {:?}", response);
            // Send response
            let response_data = serde_json::to_vec(&response)?;
            self.socket
                .send(&response_data)
                .map_err(ServerError::from)?;
            info!("sent response");
        }
    }

    async fn handle_command(&self, command: Command) -> Response {
        match command {
            // ---------------------------------------------------------------
            // Deprecated low-level deck commands — not supported in split-architecture mode.
            // ---------------------------------------------------------------
            Command::LoadTrack { .. }
            | Command::SetVolume { .. }
            | Command::Unload { .. }
            | Command::Seek { .. } => Response::Err {
                message: "This command is not supported in split-architecture mode".into(),
            },
            Command::Play { deck: _ } => {
                info!("Play command");
                if let Some(effects) = self.try_resume().await {
                    let result =
                        execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client)
                            .await;
                    return self.create_response(result, None);
                }
                // Playing or Idle — no-op
                if matches!(self.state.lock().await.state(), PlaybackState::Idle) {
                    warn!("Play command received while Idle and no queue entry");
                }
                self.create_response(Ok(()), None)
            }
            // ---------------------------------------------------------------
            // State-machine-backed commands
            // ---------------------------------------------------------------
            Command::Stop { deck: _ } => {
                info!("Stopping playback");
                let effects = self.state.lock().await.stop();
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::Pause { deck: _ } => {
                info!("Pausing playback");
                let effects = self.state.lock().await.pause();
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::Resume { deck: _ } => {
                info!("Resuming playback");
                let effects = self.state.lock().await.resume();
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::GetLength { deck: _ } => {
                info!("Getting length");
                match self.audio.lock().await.loaded() {
                    Ok(Some(info)) => Response::Ok {
                        data: info.duration_ms.map(|d| ResponseData::Length(d as usize)),
                    },
                    Ok(None) => Response::Ok { data: None },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Command::QueueNext { hash, source } => {
                info!("Queue next: hash={:?} source={:?}", hash, source);
                let mut queue = self.queue.lock().await;
                queue.push_front(QueueEntry { hash, source });
                self.queue_changed(&queue);
                self.ok_response()
            }
            Command::QueueAppend { hash, source } => {
                info!("Queue append: hash={:?} source={:?}", hash, source);
                let mut queue = self.queue.lock().await;
                queue.push_back(QueueEntry { hash, source });
                self.queue_changed(&queue);
                self.ok_response()
            }
            Command::QueueList => {
                let hashes: Vec<media_protocol::ContentHash> = self
                    .queue
                    .lock()
                    .await
                    .iter()
                    .map(|e| e.hash.clone())
                    .collect();
                info!("Queue list: {} items", hashes.len());
                Response::Ok {
                    data: Some(ResponseData::Queue(hashes)),
                }
            }
            Command::QueueClear => {
                let mut queue = self.queue.lock().await;
                queue.clear();
                self.queue_changed(&queue);
                info!("Queue cleared");
                self.ok_response()
            }
            Command::QueueRemove { hashes } => {
                let mut queue = self.queue.lock().await;
                let before = queue.len();
                queue.retain(|e| !hashes.contains(&e.hash));
                let removed = before - queue.len();
                self.queue_changed(&queue);
                info!("Removed {}/{} hash(es) from queue", removed, hashes.len());
                Response::Ok {
                    data: Some(ResponseData::Count(removed)),
                }
            }
            Command::QueueReplace { entries } => {
                let mut queue = self.queue.lock().await;
                queue.clear();
                for (hash, source) in entries {
                    queue.push_back(QueueEntry { hash, source });
                }
                self.queue_changed(&queue);
                info!("Queue replaced with {} entries", queue.len());
                self.ok_response()
            }
            Command::PlayQueue => {
                info!("Play from queue");

                // If currently paused, resume the paused track instead of
                // popping from the queue.
                if let Some(effects) = self.try_resume().await {
                    info!("PlayQueue: state is Paused — resuming instead of popping queue");
                    let result =
                        execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client)
                            .await;
                    return self.create_response(result, None);
                }

                let entry = {
                    let mut queue = self.queue.lock().await;
                    self.pop_next(&mut queue)
                };
                match entry {
                    None => Response::Err {
                        message: "Queue is empty".to_string(),
                    },
                    Some(e) => {
                        let effects = self
                            .state
                            .lock()
                            .await
                            .play_queue(e.hash.clone(), e.source.clone());
                        let result = execute_effects(
                            effects,
                            &self.audio,
                            &self.event_pub,
                            &self.acid_client,
                        )
                        .await;
                        self.create_response(result, None)
                    }
                }
            }
            Command::NowPlaying => {
                let hash = self.state.lock().await.current_hash().cloned();
                info!("Now playing: {:?}", hash);
                Response::Ok {
                    data: Some(ResponseData::NowPlaying(hash)),
                }
            }
            Command::Skip => {
                info!("Skip: stopping current track and advancing queue");
                let next = {
                    let mut queue = self.queue.lock().await;
                    self.pop_next(&mut queue)
                        .map(|entry| (entry.hash, entry.source))
                };
                let effects = self.state.lock().await.skip(next);
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::GetSession => {
                let session_id = self
                    .state
                    .lock()
                    .await
                    .session_id()
                    .map(|id| id.as_str().to_owned());
                info!("GetSession: {:?}", session_id);
                Response::Ok {
                    data: Some(ResponseData::Session(session_id)),
                }
            }
            Command::ListAudioOutputs => {
                info!("Listing audio outputs");
                match self.audio.lock().await.list_outputs() {
                    Ok(sinks) => Response::Ok {
                        data: Some(ResponseData::AudioOutputs(sinks)),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Command::SetAudioOutput { device_name } => {
                info!("Setting audio output to {:?}", device_name);
                match self.audio.lock().await.set_output(device_name) {
                    Ok(config) => Response::Ok {
                        data: Some(ResponseData::AudioOutput(config)),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
            Command::GetAudioOutput => {
                info!("Getting current audio output");
                match self.audio.lock().await.get_output() {
                    Ok(config) => Response::Ok {
                        data: Some(ResponseData::AudioOutput(config)),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
        }
    }

    /// If the state machine is Paused, resume it and return the effects.
    /// Returns None if not paused (Idle or Playing).
    async fn try_resume(&self) -> Option<Vec<PlaybackEffect>> {
        let mut sm = self.state.lock().await;
        if matches!(sm.state(), PlaybackState::Paused { .. }) {
            Some(sm.resume())
        } else {
            None
        }
    }

    fn ok_response(&self) -> Response {
        Response::Ok { data: None }
    }

    fn create_response(
        &self,
        result: Result<(), ServerError>,
        data: Option<ResponseData>,
    ) -> Response {
        match result {
            Ok(()) => {
                info!("Command completed successfully");
                Response::Ok { data }
            }
            Err(e) => {
                warn!("Command failed: {}", e);
                Response::Err {
                    message: e.to_string(),
                }
            }
        }
    }
}

/// Execute a list of effects produced by the state machine.
async fn execute_effects(
    effects: Vec<PlaybackEffect>,
    audio: &Arc<Mutex<StreamClient>>,
    event_pub: &nng::Socket,
    acid_client: &AcidClient,
) -> Result<(), ServerError> {
    for effect in effects {
        match effect {
            PlaybackEffect::StopEngine => {
                let guard = audio.lock().await;
                if let Err(e) = guard.stop() {
                    warn!("Failed to stop audio: {e}");
                }
            }
            PlaybackEffect::PauseEngine => {
                if let Err(e) = audio.lock().await.pause() {
                    warn!("Failed to pause audio: {e}");
                }
            }
            PlaybackEffect::PlayEngine => {
                let guard = audio.lock().await;
                if let Err(e) = guard.play() {
                    warn!("Failed to play audio: {e}");
                }
            }
            PlaybackEffect::LoadAndPlay { hash, source: _ } => {
                // Phase 4: single source, always route to mdma-audio
                let client = audio.lock().await;
                client.load(hash.clone()).inspect_err(|e| {
                    warn!("Failed to load {}: {}", hash, e);
                })?;
                client.play().inspect_err(|e| {
                    warn!("Failed to play after load: {}", e);
                })?;
            }
            PlaybackEffect::EmitEvent(event) => {
                publish_event_on_socket(event_pub, &event);
            }
            PlaybackEffect::WriteFact { hash, value } => {
                write_fact(acid_client, &hash, value);
            }
        }
    }
    Ok(())
}

fn publish_event_on_socket(socket: &nng::Socket, event: &PlaybackEvent) {
    let msg = to_topic_message(event);
    if let Err(e) = socket.send(&msg) {
        warn!("Failed to publish event: {:?}", e);
    }
}

/// Persist queue to disk (free function for use in auto_advance_task).
fn persist_queue_to_file(queue_file: &Path, queue: &VecDeque<QueueEntry>) {
    let entries: Vec<PersistEntry> = queue
        .iter()
        .map(|e| PersistEntry {
            hash: e.hash.as_str().to_owned(),
            source: e.source.clone(),
        })
        .collect();

    let json = match serde_json::to_string_pretty(&entries) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize queue: {}", e);
            return;
        }
    };

    let tmp = queue_file.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &json) {
        warn!("Failed to write queue temp file {:?}: {}", tmp, e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, queue_file) {
        warn!("Failed to rename queue file: {}", e);
    }
}

/// Persist the queue and publish `QueueChanged` with its new length.
fn persist_and_publish(queue_file: &Path, event_pub: &Socket, queue: &VecDeque<QueueEntry>) {
    persist_queue_to_file(queue_file, queue);
    publish_event_on_socket(
        event_pub,
        &PlaybackEvent::QueueChanged {
            length: queue.len(),
        },
    );
}

/// Pop the queue front; if something was popped, persist and publish.
fn pop_front_and_publish(
    queue_file: &Path,
    event_pub: &Socket,
    queue: &mut VecDeque<QueueEntry>,
) -> Option<QueueEntry> {
    let entry = queue.pop_front();
    if entry.is_some() {
        persist_and_publish(queue_file, event_pub, queue);
    }
    entry
}

/// The track the audio source should have preloaded: the front of the queue
/// while a track is Playing or Paused, nothing otherwise.
///
/// The same track twice in a row is not preloaded: a gapless switch from `a`
/// to `a` is invisible to the poll (audible == current), so that case goes
/// through the normal finished -> load path instead.
fn desired_preload(
    state: &PlaybackState,
    queue: &VecDeque<QueueEntry>,
) -> Option<media_protocol::ContentHash> {
    match state {
        PlaybackState::Playing { hash } | PlaybackState::Paused { hash } => queue
            .front()
            .filter(|e| e.hash != *hash)
            .map(|e| e.hash.clone()),
        PlaybackState::Idle => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum PreloadAction {
    Preload(media_protocol::ContentHash),
    Clear,
}

/// What to send to the audio source so its preload matches `desired`, given
/// the last value we sent. `None` when already in sync.
fn preload_action(
    desired: Option<&media_protocol::ContentHash>,
    last_sent: Option<&media_protocol::ContentHash>,
) -> Option<PreloadAction> {
    if desired == last_sent {
        return None;
    }
    Some(match desired {
        Some(h) => PreloadAction::Preload(h.clone()),
        None => PreloadAction::Clear,
    })
}

#[derive(Debug, Clone, PartialEq)]
enum PollAction {
    Nothing,
    /// The audio source switched to the preloaded track by itself (gapless).
    Advanced(media_protocol::ContentHash),
    /// The track finished with no switch; fall back to load-and-play.
    Finished,
}

/// Classify a `Loaded` poll while Playing.
///
/// `heard` is the audible track reported by the source, `known_next` every
/// hash the source may legitimately switch to while `current` plays (what we
/// sent as a preload plus what the source reported as its next track, see
/// `remember_next`), `current` the state machine's track.
///
/// The set matters because the source commits to its next track about 3 s
/// before it is audible; a queue edit in that window changes what we last
/// sent, but the committed track still becomes audible.
/// A switch wins over `Finished`: the next tick sees the new track's state.
fn classify_poll(
    finished: bool,
    heard: Option<&media_protocol::ContentHash>,
    known_next: &[media_protocol::ContentHash],
    current: &media_protocol::ContentHash,
) -> PollAction {
    match heard {
        Some(h) if h != current && known_next.contains(h) => PollAction::Advanced(h.clone()),
        _ if finished => PollAction::Finished,
        _ => PollAction::Nothing,
    }
}

/// Record a hash the source may switch to next (no-op for `None` or a known hash).
fn remember_next(
    known_next: &mut Vec<media_protocol::ContentHash>,
    hash: Option<&media_protocol::ContentHash>,
) {
    let Some(h) = hash else {
        return;
    };
    if !known_next.contains(h) {
        known_next.push(h.clone());
    }
}

/// Remove the first queue entry with `hash`. Returns whether one was removed.
fn remove_first(queue: &mut VecDeque<QueueEntry>, hash: &media_protocol::ContentHash) -> bool {
    match queue.iter().position(|e| &e.hash == hash) {
        Some(i) => {
            queue.remove(i);
            true
        }
        None => false,
    }
}

/// Gapless advance: ask the state machine first and only touch the queue if it
/// actually transitioned (its guard may reject a stale observation).
///
/// The heard track is removed from wherever it first sits in the queue: after
/// a queue edit inside the commit window it is no longer at the front, and
/// leaving it would play it twice. Returns the effects and whether the queue
/// changed.
fn advance_and_dequeue(
    sm: &mut PlaybackStateMachine,
    queue: &mut VecDeque<QueueEntry>,
    current: &media_protocol::ContentHash,
    heard: media_protocol::ContentHash,
) -> (Vec<PlaybackEffect>, bool) {
    let effects = sm.track_advanced(current, heard.clone());
    if effects.is_empty() {
        return (effects, false);
    }
    let removed = remove_first(queue, &heard);
    if !removed {
        warn!("Gapless switch to {:?} but it is not in the queue", heard);
    }
    (effects, removed)
}

/// Track finished without a switch: offer the queue front to the state machine
/// and pop it only if the state machine transitioned. Returns the effects and
/// the popped entry, if any.
fn finish_and_dequeue(
    sm: &mut PlaybackStateMachine,
    queue: &mut VecDeque<QueueEntry>,
    finished: &media_protocol::ContentHash,
) -> (Vec<PlaybackEffect>, Option<QueueEntry>) {
    let next = queue.front().map(|e| (e.hash.clone(), e.source.clone()));
    let effects = sm.track_ended(finished, next);
    if effects.is_empty() {
        return (effects, None);
    }
    (effects, queue.pop_front())
}

/// Send Preload/ClearPreload if the queue front differs from the last value sent.
/// `last_sent` is only updated when the source accepted the command; an
/// accepted preload is also remembered in `known_next`.
async fn reconcile_preload(
    audio: &Arc<Mutex<StreamClient>>,
    queue: &Arc<Mutex<VecDeque<QueueEntry>>>,
    state: &PlaybackState,
    last_sent: &mut Option<media_protocol::ContentHash>,
    known_next: &mut Vec<media_protocol::ContentHash>,
) {
    let desired = {
        let q = queue.lock().await;
        desired_preload(state, &q)
    };
    let Some(action) = preload_action(desired.as_ref(), last_sent.as_ref()) else {
        return;
    };
    let client = audio.lock().await;
    match action {
        PreloadAction::Preload(hash) => match client.preload(hash.clone()) {
            Ok(()) => {
                remember_next(known_next, Some(&hash));
                *last_sent = Some(hash);
            }
            Err(e) => warn!("Failed to preload {}: {}", hash, e),
        },
        PreloadAction::Clear => match client.clear_preload() {
            Ok(()) => *last_sent = None,
            Err(e) => warn!("Failed to clear preload: {}", e),
        },
    }
}

/// Polls the audio source every 200 ms.
///
/// * While Playing or Paused, keeps the source's preload in sync with the
///   queue front (`desired_preload`), sending `Preload`/`ClearPreload` only
///   when it differs from the last value sent. The "last sent" value is reset
///   whenever the state machine's track changes or goes Idle, so it is re-sent.
/// * Gapless switch: the source reports the audible track. The source commits
///   to its next track once its mixer promoted it (~3 s before it is audible)
///   and can no longer replace it, so the queue may change in that window.
///   Per current track we therefore remember every hash we sent as a preload
///   plus every `preloaded` hash the source reported. When the audible track
///   is one of those and not the state machine's track, the source switched on
///   its own and the state machine is advanced via `track_advanced` (no engine
///   effect), recording the heard track. Only if the state machine accepted
///   the advance is the heard track's first queue occurrence removed (it may
///   no longer be at the front after a queue edit; the edit is kept).
/// * `Finished` with no switch (end of queue, or the source parked the preload
///   after a sample-rate change) falls back to `track_ended` -> LoadAndPlay
///   (the source reuses a parked preload); the queue front is popped only if
///   the state machine accepted it.
/// * Both transitions carry the track the poll observed, so a poll that raced
///   with a user command (e.g. Skip) cannot act on stale state.
///
/// Auto-advance is suppressed while paused.
/// Every 5th poll iteration (~1 s) broadcasts a `PositionUpdate` event while playing.
async fn auto_advance_task(
    audio: Arc<Mutex<StreamClient>>,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    state: Arc<Mutex<PlaybackStateMachine>>,
    event_pub: nng::Socket,
    acid_client: Arc<AcidClient>,
    queue_file: PathBuf,
) {
    let mut tick: u8 = 0;
    // Last preload value sent to the source, and the track it was sent for.
    let mut last_sent: Option<media_protocol::ContentHash> = None;
    let mut last_current: Option<media_protocol::ContentHash> = None;
    // Hashes the source may switch to while `last_current` plays (see `classify_poll`).
    let mut known: Vec<media_protocol::ContentHash> = Vec::new();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        tick = tick.wrapping_add(1);

        // Every 5th tick (~1 s) broadcast a position update while playing.
        if tick.is_multiple_of(5) {
            let current_state = state.lock().await;
            if let PlaybackState::Playing { hash } = current_state.state() {
                let h = hash.clone();
                drop(current_state);
                if let Ok(Some(ref info)) = audio.lock().await.loaded() {
                    if let (Some(position_ms), Some(duration_ms)) =
                        (info.position_ms, info.duration_ms)
                    {
                        publish_event_on_socket(
                            &event_pub,
                            &PlaybackEvent::PositionUpdate {
                                hash: h.clone(),
                                position_ms,
                                duration_ms,
                            },
                        );
                    }
                }
            }
        }

        let snapshot = state.lock().await.state().clone();
        let (current_hash, is_playing) = match &snapshot {
            PlaybackState::Playing { hash } => (hash.clone(), true),
            PlaybackState::Paused { hash } => (hash.clone(), false),
            PlaybackState::Idle => {
                // Stopped: the source dropped its preload; make sure we re-send later.
                last_current = None;
                last_sent = None;
                known.clear();
                continue;
            }
        };
        if last_current.as_ref() != Some(&current_hash) {
            // New track (manual load, skip, advance): re-send the preload.
            last_current = Some(current_hash.clone());
            last_sent = None;
            known.clear();
        }

        let (finished, heard, reported_next, position_ms, duration_ms) =
            match audio.lock().await.loaded() {
                Ok(Some(ref info)) => (
                    matches!(info.state, StreamPlaybackState::Finished),
                    info.content_hash.clone(),
                    info.preloaded.clone(),
                    info.position_ms,
                    info.duration_ms,
                ),
                Ok(None) => (false, None, None, None, None),
                Err(e) => {
                    warn!("Failed to poll audio source: {e}");
                    (false, None, None, None, None)
                }
            };
        remember_next(&mut known, reported_next.as_ref());

        if !is_playing {
            // Paused: no advancing, but keep the preload in sync.
            reconcile_preload(&audio, &queue, &snapshot, &mut last_sent, &mut known).await;
            continue;
        }

        // The snapshot predates the poll; a command (e.g. Skip) may have moved
        // the state since. Drop this observation rather than act on it.
        if state.lock().await.current_hash() != Some(&current_hash) {
            continue;
        }

        match classify_poll(finished, heard.as_ref(), &known, &current_hash) {
            PollAction::Nothing => {
                reconcile_preload(&audio, &queue, &snapshot, &mut last_sent, &mut known).await;
                continue;
            }
            PollAction::Advanced(heard_hash) => {
                let effects = {
                    let mut sm = state.lock().await;
                    let mut q = queue.lock().await;
                    let (effects, removed) =
                        advance_and_dequeue(&mut sm, &mut q, &current_hash, heard_hash.clone());
                    if removed {
                        persist_and_publish(&queue_file, &event_pub, &q);
                    }
                    effects
                };
                if effects.is_empty() {
                    continue;
                }
                info!("Gapless advance: {:?} -> {:?}", current_hash, heard_hash);
                if let Err(e) = execute_effects(effects, &audio, &event_pub, &acid_client).await {
                    warn!("Gapless advance bookkeeping failed: {}", e);
                }
                // The source consumed its preload; new track resets via last_current.
                continue;
            }
            PollAction::Finished => {}
        }
        let finished_hash = current_hash;

        // Track ended — drive the state machine; the queue front is popped only
        // if it actually transitioned.
        let (effects, next) = {
            let mut sm = state.lock().await;
            let mut q = queue.lock().await;
            let (effects, popped) = finish_and_dequeue(&mut sm, &mut q, &finished_hash);
            if popped.is_some() {
                persist_and_publish(&queue_file, &event_pub, &q);
            }
            (effects, popped)
        };
        if effects.is_empty() {
            continue;
        }

        info!(
            "Track finished: {:?}, next: {}, position_ms={:?} duration_ms={:?}",
            finished_hash,
            next.as_ref()
                .map_or_else(|| "end of queue".to_string(), |e| format!("{:?}", e.hash)),
            position_ms,
            duration_ms,
        );

        if let Err(e) = execute_effects(effects, &audio, &event_pub, &acid_client).await {
            warn!("Auto-advance failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_client::AcidClient;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_server() -> Server {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let event_pub = nng::Socket::new(nng::Protocol::Pub0).unwrap();
        // Create a dummy ACID listener so the client can connect.
        let acid_listen = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let acid_addr = format!("ipc:///tmp/test_acid_{}_{}.sock", std::process::id(), id);
        acid_listen.listen(&acid_addr).unwrap();
        let acid_client = Arc::new(AcidClient::connect(&acid_addr).unwrap());
        std::mem::forget(acid_listen);

        // Create a dummy audio source (Rep0 stub) that auto-replies with Ok so that
        // tests which trigger NNG effects don't block waiting for a timeout.
        let audio_stub = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let audio_addr = format!("ipc:///tmp/test_audio_{}_{}.sock", std::process::id(), id);
        audio_stub.listen(&audio_addr).unwrap();

        std::thread::spawn(move || {
            use stream_source_protocol::StreamResponse;
            while audio_stub.recv().is_ok() {
                let reply = serde_json::to_vec(&StreamResponse::Ok).unwrap();
                let _ = audio_stub.send(reply.as_slice());
            }
        });

        let audio_client = StreamClient::connect(&audio_addr).unwrap();
        let audio = Arc::new(tokio::sync::Mutex::new(audio_client));

        Server::new(
            audio,
            socket,
            PathBuf::from(format!(
                "/tmp/test_queue_{}_{}.json",
                std::process::id(),
                id
            )),
            event_pub,
            acid_client,
        )
    }

    /// PlayQueue while Paused must resume the paused track, not pop from the queue.
    #[tokio::test]
    async fn play_queue_while_paused_resumes_not_pops() {
        let server = make_server();

        // Put something in the queue so we can verify it is NOT popped.
        {
            let mut q = server.queue.lock().await;
            q.push_back(QueueEntry {
                hash: media_protocol::ContentHash::new("sha256:queued"),
                source: SourceName::audio(),
            });
        }

        // Drive state machine to Paused.
        {
            let mut sm = server.state.lock().await;
            let hash = media_protocol::ContentHash::new("sha256:paused_track");
            sm.play_queue(hash, SourceName::audio());
            sm.pause();
        }

        assert!(
            matches!(
                server.state.lock().await.state(),
                PlaybackState::Paused { .. }
            ),
            "Expected Paused before PlayQueue"
        );

        // Issue PlayQueue via handle_command.
        let response = server.handle_command(Command::PlayQueue).await;

        // Must succeed.
        assert!(
            matches!(response, Response::Ok { .. }),
            "Expected Ok response, got {:?}",
            response
        );

        // State must be Playing (resumed), not Idle.
        assert!(
            matches!(
                server.state.lock().await.state(),
                PlaybackState::Playing { .. }
            ),
            "Expected Playing after PlayQueue from Paused"
        );

        // The queued track must still be in the queue (not popped).
        let queue_len = server.queue.lock().await.len();
        assert_eq!(
            queue_len, 1,
            "Queue should still have 1 entry after resume, got {}",
            queue_len
        );
    }

    /// After pause -> skip, the state machine must be in Idle (not Paused).
    /// This is the regression test for the original bug where Skip didn't clear
    /// `is_paused`.
    #[tokio::test]
    async fn skip_after_pause_state_is_not_paused() {
        let server = make_server();

        // Drive the state machine directly into Paused.
        {
            let mut sm = server.state.lock().await;
            let hash = media_protocol::ContentHash::new("sha256:test");
            sm.play_queue(hash, SourceName::audio());
            sm.pause();
        }

        assert!(
            matches!(
                server.state.lock().await.state(),
                PlaybackState::Paused { .. }
            ),
            "Expected Paused state before Skip"
        );

        // Issue Skip directly on the state machine.
        // We skip effects execution entirely — the audio stub doesn't respond,
        // and we only care about the state transition here.
        let _effects = server.state.lock().await.skip(None);

        assert!(
            matches!(server.state.lock().await.state(), PlaybackState::Idle),
            "Expected Idle after Skip from Paused"
        );
    }

    // -------------------------------------------------------------------------
    // Gapless pure helpers
    // -------------------------------------------------------------------------

    fn h(s: &str) -> media_protocol::ContentHash {
        media_protocol::ContentHash::new(s)
    }

    fn entry(s: &str) -> QueueEntry {
        QueueEntry {
            hash: h(s),
            source: SourceName::audio(),
        }
    }

    #[test]
    fn desired_preload_empty_queue_is_none() {
        let q = VecDeque::new();
        assert_eq!(
            desired_preload(&PlaybackState::Playing { hash: h("a") }, &q),
            None
        );
    }

    #[test]
    fn desired_preload_playing_and_paused_is_queue_front() {
        let q: VecDeque<_> = [entry("b"), entry("c")].into();
        assert_eq!(
            desired_preload(&PlaybackState::Playing { hash: h("a") }, &q),
            Some(h("b"))
        );
        assert_eq!(
            desired_preload(&PlaybackState::Paused { hash: h("a") }, &q),
            Some(h("b"))
        );
    }

    #[test]
    fn desired_preload_idle_is_none() {
        let q: VecDeque<_> = [entry("b")].into();
        assert_eq!(desired_preload(&PlaybackState::Idle, &q), None);
    }

    #[test]
    fn desired_preload_follows_queue_changes() {
        let state = PlaybackState::Playing { hash: h("a") };
        let mut q: VecDeque<_> = [entry("b")].into();
        assert_eq!(desired_preload(&state, &q), Some(h("b")));
        q.push_front(entry("x"));
        assert_eq!(desired_preload(&state, &q), Some(h("x")));
        q.clear();
        assert_eq!(desired_preload(&state, &q), None);
    }

    #[test]
    fn preload_action_cases() {
        // unchanged
        assert_eq!(preload_action(Some(&h("b")), Some(&h("b"))), None);
        assert_eq!(preload_action(None, None), None);
        // new / changed
        assert_eq!(
            preload_action(Some(&h("b")), None),
            Some(PreloadAction::Preload(h("b")))
        );
        assert_eq!(
            preload_action(Some(&h("c")), Some(&h("b"))),
            Some(PreloadAction::Preload(h("c")))
        );
        // queue emptied
        assert_eq!(
            preload_action(None, Some(&h("b"))),
            Some(PreloadAction::Clear)
        );
    }

    #[test]
    fn desired_preload_is_none_when_front_is_the_current_track() {
        // Gapless A -> A would be invisible to the poll (heard == current),
        // so the same track twice in a row goes through the load path.
        let q: VecDeque<_> = [entry("a"), entry("c")].into();
        assert_eq!(
            desired_preload(&PlaybackState::Playing { hash: h("a") }, &q),
            None
        );
        assert_eq!(
            desired_preload(&PlaybackState::Paused { hash: h("a") }, &q),
            None
        );
    }

    #[test]
    fn classify_poll_gapless_switch() {
        let action = classify_poll(false, Some(&h("b")), &[h("b")], &h("a"));
        assert_eq!(action, PollAction::Advanced(h("b")));
    }

    #[test]
    fn classify_poll_switch_wins_over_finished() {
        let action = classify_poll(true, Some(&h("b")), &[h("b")], &h("a"));
        assert_eq!(action, PollAction::Advanced(h("b")));
    }

    #[test]
    fn classify_poll_same_track_playing_is_nothing() {
        assert_eq!(
            classify_poll(false, Some(&h("a")), &[h("b")], &h("a")),
            PollAction::Nothing
        );
    }

    #[test]
    fn classify_poll_finished_without_switch() {
        assert_eq!(
            classify_poll(true, Some(&h("a")), &[h("b")], &h("a")),
            PollAction::Finished
        );
        assert_eq!(
            classify_poll(true, Some(&h("a")), &[], &h("a")),
            PollAction::Finished
        );
        assert_eq!(
            classify_poll(true, None, &[], &h("a")),
            PollAction::Finished
        );
    }

    #[test]
    fn classify_poll_unrelated_hash_is_not_a_switch() {
        // Neither preloaded by us nor reported by the source (e.g. stale poll
        // around a manual load).
        assert_eq!(
            classify_poll(false, Some(&h("z")), &[h("b")], &h("a")),
            PollAction::Nothing
        );
        assert_eq!(
            classify_poll(false, Some(&h("b")), &[], &h("a")),
            PollAction::Nothing
        );
    }

    #[test]
    fn classify_poll_detects_switch_to_committed_track_after_preload_was_replaced() {
        // We sent b, the source committed b, then we replaced the preload with x.
        // When b becomes audible it must still be detected.
        let mut known = Vec::new();
        remember_next(&mut known, Some(&h("b"))); // sent
        remember_next(&mut known, Some(&h("b"))); // reported by the source (committed)
        remember_next(&mut known, Some(&h("x"))); // sent after the queue edit
        assert_eq!(
            classify_poll(false, Some(&h("b")), &known, &h("a")),
            PollAction::Advanced(h("b"))
        );
    }

    #[test]
    fn classify_poll_detects_switch_to_committed_track_after_preload_was_cleared() {
        // Queue cleared after commit: nothing is sent any more, but the source
        // reported b as its next track in an earlier poll.
        let mut known = Vec::new();
        remember_next(&mut known, Some(&h("b")));
        remember_next(&mut known, None);
        assert_eq!(
            classify_poll(true, Some(&h("b")), &known, &h("a")),
            PollAction::Advanced(h("b"))
        );
    }

    #[test]
    fn remember_next_ignores_none_and_duplicates() {
        let mut known = Vec::new();
        remember_next(&mut known, None);
        remember_next(&mut known, Some(&h("b")));
        remember_next(&mut known, Some(&h("b")));
        assert_eq!(known, vec![h("b")]);
    }

    // -------------------------------------------------------------------------
    // Auto-advance scenarios through the pure functions
    // -------------------------------------------------------------------------

    #[test]
    fn scenario_queue_edited_inside_commit_window_records_heard_and_keeps_edit() {
        // Playing a; queue [b, c]; b sent and committed; user does QueueNext x.
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        let mut q: VecDeque<_> = [entry("x"), entry("b"), entry("c")].into();
        let known = vec![h("b"), h("x")];

        let action = classify_poll(false, Some(&h("b")), &known, &h("a"));
        assert_eq!(action, PollAction::Advanced(h("b")));
        let PollAction::Advanced(heard) = action else {
            unreachable!()
        };
        // b is not at the front: it is removed from where it is, the edit stays.
        let (effects, removed) = advance_and_dequeue(&mut sm, &mut q, &h("a"), heard);
        assert!(!effects.is_empty());
        assert!(removed);
        assert_eq!(sm.current_hash(), Some(&h("b")));
        let hashes: Vec<_> = q.iter().map(|e| e.hash.clone()).collect();
        assert_eq!(hashes, vec![h("x"), h("c")]);
    }

    #[test]
    fn advance_rejected_by_state_machine_leaves_queue_untouched() {
        // A Pause landed between the poll and the advance: the guard rejects.
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        sm.pause();
        let mut q: VecDeque<_> = [entry("b"), entry("c")].into();

        let (effects, removed) = advance_and_dequeue(&mut sm, &mut q, &h("a"), h("b"));
        assert!(effects.is_empty());
        assert!(!removed);
        assert_eq!(q.len(), 2);
        assert!(matches!(sm.state(), PlaybackState::Paused { .. }));
    }

    #[test]
    fn finish_rejected_by_state_machine_leaves_queue_untouched() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        sm.pause();
        let mut q: VecDeque<_> = [entry("b")].into();

        let (effects, popped) = finish_and_dequeue(&mut sm, &mut q, &h("a"));
        assert!(effects.is_empty());
        assert!(popped.is_none());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn finish_pops_front_when_state_machine_transitions() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        let mut q: VecDeque<_> = [entry("b"), entry("c")].into();

        let (effects, popped) = finish_and_dequeue(&mut sm, &mut q, &h("a"));
        assert!(!effects.is_empty());
        assert_eq!(popped.map(|e| e.hash), Some(h("b")));
        assert_eq!(q.len(), 1);
        assert_eq!(sm.current_hash(), Some(&h("b")));
    }

    #[test]
    fn finish_with_empty_queue_goes_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        let mut q: VecDeque<QueueEntry> = VecDeque::new();

        let (effects, popped) = finish_and_dequeue(&mut sm, &mut q, &h("a"));
        assert!(!effects.is_empty());
        assert!(popped.is_none());
        assert_eq!(sm.state(), &PlaybackState::Idle);
    }

    #[test]
    fn scenario_parked_preload_finished_falls_back_to_load_path() {
        // The source parked the preload (sample-rate change): a finishes with
        // no switch, so the queue front is popped and loaded explicitly.
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        let mut q: VecDeque<_> = [entry("b")].into();

        let action = classify_poll(true, Some(&h("a")), &[h("b")], &h("a"));
        assert_eq!(action, PollAction::Finished);
        let next = q.pop_front().map(|e| (e.hash, e.source));
        let effects = sm.track_ended(&h("a"), next);
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == h("b"))));
        assert_eq!(sm.current_hash(), Some(&h("b")));
    }

    #[test]
    fn scenario_end_of_queue_goes_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        let q: VecDeque<QueueEntry> = VecDeque::new();

        assert_eq!(
            desired_preload(sm.state(), &q),
            None,
            "nothing to preload at the end of the queue"
        );
        assert_eq!(
            classify_poll(true, Some(&h("a")), &[], &h("a")),
            PollAction::Finished
        );
        sm.track_ended(&h("a"), None);
        assert_eq!(sm.state(), &PlaybackState::Idle);
    }

    #[test]
    fn scenario_same_track_twice_plays_twice() {
        let mut sm = PlaybackStateMachine::new();
        sm.play_queue(h("a"), SourceName::audio());
        let mut q: VecDeque<_> = [entry("a")].into();

        // No preload for a duplicate of the current track.
        assert_eq!(desired_preload(sm.state(), &q), None);
        assert_eq!(
            classify_poll(true, Some(&h("a")), &[], &h("a")),
            PollAction::Finished
        );
        let next = q.pop_front().map(|e| (e.hash, e.source));
        let effects = sm.track_ended(&h("a"), next);
        assert!(effects
            .iter()
            .any(|e| matches!(e, PlaybackEffect::LoadAndPlay { hash, .. } if *hash == h("a"))));
        assert!(q.is_empty());
    }

    #[test]
    fn remove_first_removes_first_occurrence_only() {
        let mut q: VecDeque<_> = [entry("b"), entry("c"), entry("b")].into();
        assert!(remove_first(&mut q, &h("c")));
        assert_eq!(q.len(), 2);
        assert!(remove_first(&mut q, &h("b")));
        assert_eq!(q.len(), 1);
        assert_eq!(q.front().unwrap().hash, h("b"));
        assert!(!remove_first(&mut q, &h("z")));
        let mut empty = VecDeque::new();
        assert!(!remove_first(&mut empty, &h("b")));
    }

    /// Verify state machine transitions work for stop/resume without involving audio.
    #[tokio::test]
    async fn stop_from_idle_is_noop_at_server_level() {
        let server = make_server();
        // State starts Idle; stop from Idle should produce no effects and remain Idle.
        let effects = server.state.lock().await.stop();
        assert!(
            effects.is_empty(),
            "Stop from Idle should produce no effects"
        );
        assert!(
            matches!(server.state.lock().await.state(), PlaybackState::Idle),
            "Expected Idle after Stop from Idle"
        );
    }
}
