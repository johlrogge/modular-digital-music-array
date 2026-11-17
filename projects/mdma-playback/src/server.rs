use crate::error::ServerError;
use crate::playback_state::{PlaybackEffect, PlaybackState, PlaybackStateMachine};
use acid_client::AcidClient;
use color_eyre::Result;
use event_protocol::{to_topic_message, PlaybackEvent};
use media_protocol::{
    require_canonical, CanonicalHash, Command, ContentHash, Response, ResponseData, SourceName,
    Transport, TransportStatus,
};
use music_facts::{FactSource, MusicValue};
use nng::Socket;
use playback_primitives::PositionMs;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use stream_source_protocol::{OutputHealth, StreamPlaybackState, StreamTrackInfo};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::stream_client::StreamClient;

/// Commands that only read state. Clients poll them, so they are logged at
/// debug rather than info.
fn is_read_only(command: &Command) -> bool {
    matches!(
        command,
        Command::GetLength { .. }
            | Command::QueueList
            | Command::NowPlaying
            | Command::GetSession
            | Command::ListAudioOutputs
            | Command::GetAudioOutput
            | Command::GetOutputStatus
            | Command::GetTransport { .. }
    )
}

/// A queued track. Playback holds only canonical hashes (ADR-026): resolving
/// a prefix is the library's job, done by the client before it sends.
struct QueueEntry {
    hash: CanonicalHash,
    source: SourceName,
}

fn write_fact(acid_client: &AcidClient, hash: &CanonicalHash, value: MusicValue) {
    let source = FactSource::new("mdma-playback", env!("CARGO_PKG_VERSION"), "Unknown");
    if let Err(e) = acid_client.write_music_facts(hash.as_content_hash(), &[(value, source)]) {
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
    /// Latest output health reported by the audio source; written by the
    /// poll task, read by `GetOutputStatus`.
    output: Arc<Mutex<Option<OutputHealth>>>,
    /// Bumped by every seek, watched by the poll task (see [`SeekEpoch`]).
    seek_epoch: SeekEpoch,
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
            output: Arc::new(Mutex::new(None)),
            seek_epoch: SeekEpoch::default(),
        }
    }

    /// What the poll task shares with this server.
    fn poll_task(&self) -> PollTask {
        PollTask {
            audio: self.audio.clone(),
            queue: self.queue.clone(),
            state: self.state.clone(),
            event_pub: self.event_pub.clone(),
            acid_client: self.acid_client.clone(),
            queue_file: self.queue_file.clone(),
            output_cache: self.output.clone(),
            seek_epoch: self.seek_epoch.clone(),
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
            match CanonicalHash::try_from(ContentHash::new(e.hash)) {
                Ok(hash) => queue.push_back(QueueEntry {
                    hash,
                    source: e.source,
                }),
                Err(e) => warn!("Dropping persisted queue entry: {e}"),
            }
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
        tokio::spawn(auto_advance_task(self.poll_task()));

        loop {
            // Receive command
            let msg = self.socket.recv().map_err(ServerError::from)?;
            let response = self.handle_message(&msg).await;
            let response_data = serde_json::to_vec(&response)?;
            self.socket
                .send(&response_data)
                .map_err(ServerError::from)?;
            debug!("sent response");
        }
    }

    /// Parse one request and answer it. A message that is not a valid
    /// command gets an error response; it must never end the serve loop.
    async fn handle_message(&self, msg: &[u8]) -> Response {
        let command: Command = match serde_json::from_slice(msg) {
            Ok(command) => command,
            Err(e) => {
                warn!("Invalid command: {}", e);
                return Response::Err {
                    message: format!("invalid command: {e}"),
                };
            }
        };

        // Read-only queries are polled by UIs; keep them out of the info log.
        let read_only = is_read_only(&command);
        if read_only {
            debug!("Received command: {:?}", command);
        } else {
            info!("Received command: {:?}", command);
        }

        let response = self.handle_command(command).await;

        if read_only && !matches!(response, Response::Err { .. }) {
            debug!("Handled command, response {:?}", response);
        } else {
            info!("Handled command, response {:?}", response);
        }
        response
    }

    async fn handle_command(&self, command: Command) -> Response {
        // A command carrying a prefix is refused whole before anything runs.
        if let Err(refusal) = command.check_canonical() {
            return refusal;
        }
        match command {
            // ---------------------------------------------------------------
            // Deprecated low-level deck commands — not supported in split-architecture mode.
            // ---------------------------------------------------------------
            Command::LoadTrack { .. } | Command::SetVolume { .. } | Command::Unload { .. } => {
                Response::Err {
                    message: "This command is not supported in split-architecture mode".into(),
                }
            }
            Command::Seek {
                deck: _,
                position_ms,
            } => self.seek(position_ms).await,
            Command::GetTransport { deck: _ } => {
                let state = self.state.lock().await.state().clone();
                let info = self.ask_source(&state).await;
                Response::Ok {
                    data: Some(ResponseData::Transport(transport_status(
                        &state,
                        info.as_ref(),
                    ))),
                }
            }
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
                debug!("Getting length");
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
                let hash = match require_canonical(hash) {
                    Ok(hash) => hash,
                    Err(refusal) => return refusal,
                };
                let mut queue = self.queue.lock().await;
                queue.push_front(QueueEntry { hash, source });
                self.queue_changed(&queue);
                self.ok_response()
            }
            Command::QueueAppend { hash, source } => {
                info!("Queue append: hash={:?} source={:?}", hash, source);
                let hash = match require_canonical(hash) {
                    Ok(hash) => hash,
                    Err(refusal) => return refusal,
                };
                let mut queue = self.queue.lock().await;
                queue.push_back(QueueEntry { hash, source });
                self.queue_changed(&queue);
                self.ok_response()
            }
            Command::QueueList => {
                let hashes: Vec<ContentHash> = self
                    .queue
                    .lock()
                    .await
                    .iter()
                    .map(|e| e.hash.clone().into())
                    .collect();
                debug!("Queue list: {} items", hashes.len());
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
                let hashes = match hashes
                    .into_iter()
                    .map(require_canonical)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(hashes) => hashes,
                    Err(refusal) => return refusal,
                };
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
                // All or nothing: one prefix keeps the old queue.
                let entries = match entries
                    .into_iter()
                    .map(|(hash, source)| {
                        require_canonical(hash).map(|hash| QueueEntry { hash, source })
                    })
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(entries) => entries,
                    Err(refusal) => return refusal,
                };
                let mut queue = self.queue.lock().await;
                queue.clear();
                queue.extend(entries);
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
                let hash: Option<ContentHash> =
                    self.state.lock().await.current_hash().map(Into::into);
                debug!("Now playing: {:?}", hash);
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
                debug!("GetSession: {:?}", session_id);
                Response::Ok {
                    data: Some(ResponseData::Session(session_id)),
                }
            }
            Command::ListAudioOutputs => {
                debug!("Listing audio outputs");
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
            Command::GetOutputStatus => Response::Ok {
                data: Some(ResponseData::OutputStatus(*self.output.lock().await)),
            },
            Command::GetAudioOutput => {
                debug!("Getting current audio output");
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

    /// Seek the audible track. The transport is not touched: the source keeps
    /// its play state across a seek, so playing stays playing and paused stays
    /// paused. Idle has nothing to seek.
    ///
    /// Answers where the track is now, which is not always where it was asked
    /// to go: the source keeps a position within the track.
    async fn seek(&self, position: PositionMs) -> Response {
        let state = self.state.lock().await.state().clone();
        let Some(hash) = state_hash(&state) else {
            return Response::Err {
                message: "nothing loaded".to_string(),
            };
        };
        info!("Seek to {position}");
        if let Err(e) = self.audio.lock().await.seek(position) {
            warn!("Seek failed: {e}");
            return Response::Err {
                message: e.to_string(),
            };
        }
        // The source dropped its preload; tell the poll loop. After the
        // source's acknowledgement, so a preload the loop sent in between is
        // not mistaken for one that survived.
        self.seek_epoch.bump();
        // Let UIs rebase their clocks now rather than at the next tick.
        let info = self.ask_source(&state).await;
        if let Some(event) = info.as_ref().and_then(|i| position_update(hash, i)) {
            publish_event_on_socket(&self.event_pub, &event);
        }
        Response::Ok {
            data: Some(ResponseData::Transport(transport_status(
                &state,
                info.as_ref(),
            ))),
        }
    }

    /// What the source says about its track; nothing while `state` is idle
    /// (there is nobody to ask) or when it cannot be asked.
    async fn ask_source(&self, state: &PlaybackState) -> Option<StreamTrackInfo> {
        state_hash(state)?;
        self.audio.lock().await.loaded().unwrap_or_else(|e| {
            warn!("Could not ask the source where it is: {e}");
            None
        })
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
                client.load(hash.clone().into()).inspect_err(|e| {
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
fn desired_preload(state: &PlaybackState, queue: &VecDeque<QueueEntry>) -> Option<CanonicalHash> {
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
    Preload(CanonicalHash),
    Clear,
}

/// What to send to the audio source so its preload matches `desired`, given
/// the last value we sent. `None` when already in sync.
fn preload_action(
    desired: Option<&CanonicalHash>,
    last_sent: Option<&CanonicalHash>,
) -> Option<PreloadAction> {
    if desired == last_sent {
        return None;
    }
    Some(match desired {
        Some(h) => PreloadAction::Preload(h.clone()),
        None => PreloadAction::Clear,
    })
}

/// The event to publish when the source's reported output health moves from
/// `prev` to `now`. Nothing when unchanged, and nothing when the source
/// reports no health (an old source): absence is not a change. So the cache
/// is never cleared once a source has reported; the last known health stays
/// until a new one replaces it.
fn output_change(prev: Option<OutputHealth>, now: Option<OutputHealth>) -> Option<PlaybackEvent> {
    let health = now?;
    (prev != now).then_some(PlaybackEvent::OutputChanged { health })
}

/// Fold a source's health `reported` into `last`: returns the event to
/// publish, and updates `last` exactly when there is one.
fn apply_output_report(
    last: &mut Option<OutputHealth>,
    reported: Option<OutputHealth>,
) -> Option<PlaybackEvent> {
    let event = output_change(*last, reported);
    if event.is_some() {
        *last = reported;
    }
    event
}

/// Poll the source with `Loaded`, publish an `OutputChanged` event and refresh
/// the `GetOutputStatus` cache when its health moved. Returns the poll result
/// so the caller can reuse it.
async fn refresh_output(
    audio: &Mutex<StreamClient>,
    event_pub: &nng::Socket,
    cache: &Mutex<Option<OutputHealth>>,
    last_output: &mut Option<OutputHealth>,
) -> Result<(Option<StreamTrackInfo>, Option<OutputHealth>), ServerError> {
    let polled = audio.lock().await.loaded_with_output();
    if let Ok((_, reported)) = &polled {
        if let Some(event) = apply_output_report(last_output, *reported) {
            publish_event_on_socket(event_pub, &event);
            *cache.lock().await = *last_output;
        }
    }
    polled
}

#[derive(Debug, Clone, PartialEq)]
enum PollAction {
    Nothing,
    /// The audio source switched to the preloaded track by itself (gapless).
    Advanced(CanonicalHash),
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
    heard: Option<&CanonicalHash>,
    known_next: &[CanonicalHash],
    current: &CanonicalHash,
) -> PollAction {
    match heard {
        Some(h) if h != current && known_next.contains(h) => PollAction::Advanced(h.clone()),
        _ if finished => PollAction::Finished,
        _ => PollAction::Nothing,
    }
}

/// Record a hash the source may switch to next (no-op for `None` or a known hash).
fn remember_next(known_next: &mut Vec<CanonicalHash>, hash: Option<&CanonicalHash>) {
    let Some(h) = hash else {
        return;
    };
    if !known_next.contains(h) {
        known_next.push(h.clone());
    }
}

/// Remove the first queue entry with `hash`. Returns whether one was removed.
fn remove_first(queue: &mut VecDeque<QueueEntry>, hash: &CanonicalHash) -> bool {
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
    current: &CanonicalHash,
    heard: CanonicalHash,
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
    finished: &CanonicalHash,
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
    book: &mut PreloadBook,
) {
    let desired = {
        let q = queue.lock().await;
        desired_preload(state, &q)
    };
    let Some(action) = preload_action(desired.as_ref(), book.last_sent.as_ref()) else {
        return;
    };
    let client = audio.lock().await;
    match action {
        PreloadAction::Preload(hash) => match client.preload(hash.clone().into()) {
            Ok(()) => {
                remember_next(&mut book.known, Some(&hash));
                book.last_sent = Some(hash);
            }
            Err(e) => warn!("Failed to preload {}: {}", hash, e),
        },
        PreloadAction::Clear => match client.clear_preload() {
            Ok(()) => book.last_sent = None,
            Err(e) => warn!("Failed to clear preload: {}", e),
        },
    }
}

/// Counts the seeks. The audio source drops its preload on a seek, but the
/// poll loop only re-sends a preload when it believes the source has none:
/// that belief is reset when the track changes, and, through this counter,
/// when a seek happened. Without it gapless silently breaks after any seek.
#[derive(Debug, Clone, Default)]
struct SeekEpoch(Arc<AtomicU64>);

impl SeekEpoch {
    /// Record that a seek completed. Call after the source acknowledged it:
    /// a poll that sent a preload before the seek must still see the change.
    fn bump(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }

    fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

/// What the poll loop believes about the source's preload.
#[derive(Default)]
struct PreloadBook {
    /// The track the beliefs below were made for.
    last_current: Option<CanonicalHash>,
    /// Last preload value the source accepted.
    last_sent: Option<CanonicalHash>,
    /// Hashes the source may switch to while `last_current` plays (see `classify_poll`).
    known: Vec<CanonicalHash>,
    /// The seek epoch the beliefs were made in.
    seen_epoch: u64,
}

impl PreloadBook {
    /// Stopped: the source dropped its preload and the track.
    fn idle(&mut self) {
        self.last_current = None;
        self.last_sent = None;
        self.known.clear();
    }

    /// Note which track plays and in which seek epoch.
    ///
    /// A new track means everything remembered was for the old one. A seek
    /// only means the source dropped its preload, so what was sent is sent
    /// again; `known` is kept. It is the set of hashes the source may still
    /// switch to, and clearing it would let a switch the source already
    /// committed to look like an unknown track (and be played a second time).
    ///
    /// Remaining window: [`Server::seek`] takes the hash it reports from the
    /// state machine while the source seeks whatever is audible. If a gapless
    /// switch lands between the two, the position update names the old track.
    /// The next poll sees the switch and corrects the state.
    fn observe(&mut self, current: &CanonicalHash, epoch: u64) {
        if self.last_current.as_ref() != Some(current) {
            self.last_current = Some(current.clone());
            self.last_sent = None;
            self.known.clear();
        }
        if self.seen_epoch != epoch {
            self.seen_epoch = epoch;
            self.last_sent = None;
        }
    }
}

/// The track a state is about; none while idle.
fn state_hash(state: &PlaybackState) -> Option<&CanonicalHash> {
    match state {
        PlaybackState::Idle => None,
        PlaybackState::Playing { hash } | PlaybackState::Paused { hash } => Some(hash),
    }
}

/// The `PositionUpdate` for `hash` from what the source reported, if it
/// reported both position and duration.
fn position_update(hash: &CanonicalHash, info: &StreamTrackInfo) -> Option<PlaybackEvent> {
    Some(PlaybackEvent::PositionUpdate {
        hash: hash.clone().into(),
        position_ms: info.position_ms?,
        duration_ms: info.duration_ms?,
    })
}

/// Ask the source where it is and publish one `PositionUpdate` for `hash`.
async fn publish_position(
    audio: &Mutex<StreamClient>,
    event_pub: &nng::Socket,
    hash: &CanonicalHash,
) {
    if let Ok(Some(info)) = audio.lock().await.loaded() {
        if let Some(event) = position_update(hash, &info) {
            publish_event_on_socket(event_pub, &event);
        }
    }
}

/// The answer to `GetTransport` and `Seek` from the state machine's state and
/// what the source reported (`None` when idle, or when the source could not be
/// asked).
fn transport_status(state: &PlaybackState, info: Option<&StreamTrackInfo>) -> TransportStatus {
    let (transport, hash) = match state {
        PlaybackState::Idle => return TransportStatus::Idle,
        PlaybackState::Playing { hash } => (Transport::Playing, hash),
        PlaybackState::Paused { hash } => (Transport::Paused, hash),
    };
    TransportStatus::Loaded {
        transport,
        now: hash.clone().into(),
        position_ms: info.and_then(|i| i.position_ms).map(PositionMs::new),
        duration_ms: info.and_then(|i| i.duration_ms).map(PositionMs::new),
        preview: None,
    }
}

/// What the poll task shares with the server.
struct PollTask {
    audio: Arc<Mutex<StreamClient>>,
    queue: Arc<Mutex<VecDeque<QueueEntry>>>,
    state: Arc<Mutex<PlaybackStateMachine>>,
    event_pub: nng::Socket,
    acid_client: Arc<AcidClient>,
    queue_file: PathBuf,
    output_cache: Arc<Mutex<Option<OutputHealth>>>,
    seek_epoch: SeekEpoch,
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
async fn auto_advance_task(task: PollTask) {
    let mut state = PollState::default();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        poll_once(&task, &mut state).await;
    }
}

/// What the poll task remembers between ticks.
#[derive(Default)]
struct PollState {
    tick: u8,
    /// Last output health reported by the source (None until it reports one).
    last_output: Option<OutputHealth>,
    /// What we sent the source as a preload, and for which track.
    book: PreloadBook,
}

/// One tick of the poll task (see [`auto_advance_task`]); a function of its
/// own so a test drives exactly what the task runs.
async fn poll_once(task: &PollTask, poll: &mut PollState) {
    let PollTask {
        audio,
        queue,
        state,
        event_pub,
        acid_client,
        queue_file,
        output_cache,
        seek_epoch,
    } = task;
    let PollState {
        tick,
        last_output,
        book,
    } = poll;
    *tick = tick.wrapping_add(1);

    // Every 5th tick (~1 s) broadcast a position update while playing.
    if tick.is_multiple_of(5) {
        let current_state = state.lock().await;
        if let PlaybackState::Playing { hash } = current_state.state() {
            let h = hash.clone();
            drop(current_state);
            publish_position(audio, event_pub, &h).await;
        }
    }

    let snapshot = state.lock().await.state().clone();
    let (current_hash, is_playing) = match &snapshot {
        PlaybackState::Playing { hash } => (hash.clone(), true),
        PlaybackState::Paused { hash } => (hash.clone(), false),
        PlaybackState::Idle => {
            // Stopped: the source dropped its preload; make sure we re-send later.
            book.idle();
            // The output keeps running while Idle, so its health stays
            // meaningful: keep watching it (~1 s).
            if tick.is_multiple_of(5) {
                let _ = refresh_output(audio, event_pub, output_cache, last_output).await;
            }
            return;
        }
    };
    // A new track (manual load, skip, advance) or a seek: the source's
    // preload is gone, so re-send it.
    book.observe(&current_hash, seek_epoch.current());

    let polled = refresh_output(audio, event_pub, output_cache, last_output).await;
    let (finished, heard, reported_next, position_ms, duration_ms) =
        match polled.map(|(info, _)| info) {
            Ok(Some(ref info)) => (
                matches!(info.state, StreamPlaybackState::Finished),
                // A hash the source reports that is not canonical is not one
                // we queued, so it is as good as not reported.
                info.content_hash
                    .clone()
                    .and_then(|h| CanonicalHash::try_from(h).ok()),
                info.preloaded
                    .clone()
                    .and_then(|h| CanonicalHash::try_from(h).ok()),
                info.position_ms,
                info.duration_ms,
            ),
            Ok(None) => (false, None, None, None, None),
            Err(e) => {
                warn!("Failed to poll audio source: {e}");
                (false, None, None, None, None)
            }
        };
    remember_next(&mut book.known, reported_next.as_ref());

    if !is_playing {
        // Paused: no advancing, but keep the preload in sync.
        reconcile_preload(audio, queue, &snapshot, book).await;
        return;
    }

    // The snapshot predates the poll; a command (e.g. Skip) may have moved
    // the state since. Drop this observation rather than act on it.
    if state.lock().await.current_hash() != Some(&current_hash) {
        return;
    }

    match classify_poll(finished, heard.as_ref(), &book.known, &current_hash) {
        PollAction::Nothing => {
            reconcile_preload(audio, queue, &snapshot, book).await;
            return;
        }
        PollAction::Advanced(heard_hash) => {
            let effects = {
                let mut sm = state.lock().await;
                let mut q = queue.lock().await;
                let (effects, removed) =
                    advance_and_dequeue(&mut sm, &mut q, &current_hash, heard_hash.clone());
                if removed {
                    persist_and_publish(queue_file, event_pub, &q);
                }
                effects
            };
            if effects.is_empty() {
                return;
            }
            info!("Gapless advance: {:?} -> {:?}", current_hash, heard_hash);
            if let Err(e) = execute_effects(effects, audio, event_pub, acid_client).await {
                warn!("Gapless advance bookkeeping failed: {}", e);
            }
            // The source consumed its preload; new track resets via last_current.
            return;
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
            persist_and_publish(queue_file, event_pub, &q);
        }
        (effects, popped)
    };
    if effects.is_empty() {
        return;
    }

    info!(
        "Track finished: {:?}, next: {}, position_ms={:?} duration_ms={:?}",
        finished_hash,
        next.as_ref()
            .map_or_else(|| "end of queue".to_string(), |e| format!("{:?}", e.hash)),
        position_ms,
        duration_ms,
    );

    if let Err(e) = execute_effects(effects, audio, event_pub, acid_client).await {
        warn!("Auto-advance failed: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_client::AcidClient;
    use std::sync::atomic::AtomicU64;
    use stream_source_protocol::{StreamCommand, StreamResponse};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_server() -> Server {
        make_harness(|_| StreamResponse::Ok).server
    }

    /// A server over stubs, with what the audio stub was sent and where its
    /// events are published.
    struct Harness {
        server: Server,
        /// Every command the audio stub received, in order.
        sent: Arc<std::sync::Mutex<Vec<StreamCommand>>>,
        event_addr: String,
    }

    impl Harness {
        fn sent(&self) -> Vec<StreamCommand> {
            self.sent.lock().unwrap().clone()
        }

        fn sent_preloads(&self) -> Vec<ContentHash> {
            self.sent()
                .into_iter()
                .filter_map(|c| match c {
                    StreamCommand::Preload { content_hash } => Some(content_hash),
                    _ => None,
                })
                .collect()
        }

        /// A subscriber to the server's events, connected before returning.
        fn subscribe(&self) -> nng::Socket {
            use nng::options::{protocol::pubsub::Subscribe, Options, RecvTimeout};
            let sub = nng::Socket::new(nng::Protocol::Sub0).unwrap();
            sub.set_opt::<Subscribe>(vec![]).unwrap();
            sub.set_opt::<RecvTimeout>(Some(Duration::from_secs(2)))
                .unwrap();
            sub.dial(&self.event_addr).unwrap();
            // A publisher drops what it sends before the pipe is up.
            std::thread::sleep(Duration::from_millis(200));
            sub
        }
    }

    fn make_harness(reply: impl Fn(&StreamCommand) -> StreamResponse + Send + 'static) -> Harness {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        let event_pub = nng::Socket::new(nng::Protocol::Pub0).unwrap();
        let event_addr = format!("ipc:///tmp/test_events_{}_{}.sock", std::process::id(), id);
        event_pub.listen(&event_addr).unwrap();
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

        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = sent.clone();
        std::thread::spawn(move || {
            while let Ok(msg) = audio_stub.recv() {
                let command: StreamCommand = serde_json::from_slice(&msg).unwrap();
                let response = reply(&command);
                log.lock().unwrap().push(command);
                let _ = audio_stub.send(serde_json::to_vec(&response).unwrap().as_slice());
            }
        });

        let audio_client = StreamClient::connect(&audio_addr).unwrap();
        let audio = Arc::new(tokio::sync::Mutex::new(audio_client));

        let server = Server::new(
            audio,
            socket,
            PathBuf::from(format!(
                "/tmp/test_queue_{}_{}.json",
                std::process::id(),
                id
            )),
            event_pub,
            acid_client,
        );
        Harness {
            server,
            sent,
            event_addr,
        }
    }

    /// PlayQueue while Paused must resume the paused track, not pop from the queue.
    #[tokio::test]
    async fn play_queue_while_paused_resumes_not_pops() {
        let server = make_server();

        // Put something in the queue so we can verify it is NOT popped.
        {
            let mut q = server.queue.lock().await;
            q.push_back(QueueEntry {
                hash: h("queued"),
                source: SourceName::audio(),
            });
        }

        // Drive state machine to Paused.
        {
            let mut sm = server.state.lock().await;
            sm.play_queue(h("paused_track"), SourceName::audio());
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
            sm.play_queue(h("test"), SourceName::audio());
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

    /// The full hash a test names by `label`: its bytes in hex, zero-padded.
    fn full(label: &str) -> ContentHash {
        let hex: String = label.bytes().map(|b| format!("{b:02x}")).collect();
        ContentHash::new(format!("sha256:{hex:0<64}"))
    }

    fn h(label: &str) -> CanonicalHash {
        CanonicalHash::try_from(full(label)).unwrap()
    }

    /// A prefix, as the CLI used to send (12 hex).
    fn short() -> ContentHash {
        ContentHash::new("9fb4105e0a1b")
    }

    async fn queued(server: &Server) -> Vec<ContentHash> {
        match server.handle_command(Command::QueueList).await {
            Response::Ok {
                data: Some(ResponseData::Queue(hashes)),
            } => hashes,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn assert_refused(response: Response) {
        match response {
            Response::Err { message } => {
                assert!(message.contains("not a full content hash"), "{message}");
                assert!(message.contains("9fb4105e0a1b"), "{message}");
                assert!(
                    message.contains("resolve it through the library"),
                    "{message}"
                );
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn queue_next_refuses_a_short_hash_and_leaves_the_queue() {
        let server = make_server();
        server.queue.lock().await.push_back(entry("a"));
        let response = server
            .handle_command(Command::QueueNext {
                hash: short(),
                source: SourceName::audio(),
            })
            .await;
        assert_refused(response);
        assert_eq!(queued(&server).await, vec![full("a")]);
    }

    #[tokio::test]
    async fn queue_append_refuses_a_short_hash_and_leaves_the_queue() {
        let server = make_server();
        server.queue.lock().await.push_back(entry("a"));
        let response = server
            .handle_command(Command::QueueAppend {
                hash: short(),
                source: SourceName::audio(),
            })
            .await;
        assert_refused(response);
        assert_eq!(queued(&server).await, vec![full("a")]);
    }

    #[tokio::test]
    async fn queue_replace_refuses_a_short_hash_and_leaves_the_queue() {
        let server = make_server();
        server.queue.lock().await.push_back(entry("a"));
        let response = server
            .handle_command(Command::QueueReplace {
                entries: vec![(short(), SourceName::audio())],
            })
            .await;
        assert_refused(response);
        assert_eq!(queued(&server).await, vec![full("a")]);
    }

    #[tokio::test]
    async fn queue_replace_is_all_or_nothing() {
        let server = make_server();
        server.queue.lock().await.push_back(entry("a"));
        let response = server
            .handle_command(Command::QueueReplace {
                entries: vec![
                    (full("b"), SourceName::audio()),
                    (short(), SourceName::audio()),
                ],
            })
            .await;
        assert_refused(response);
        assert_eq!(queued(&server).await, vec![full("a")]);
    }

    #[tokio::test]
    async fn queue_remove_refuses_a_short_hash_and_removes_nothing() {
        let server = make_server();
        server.queue.lock().await.extend([entry("a"), entry("b")]);
        let response = server
            .handle_command(Command::QueueRemove {
                hashes: vec![full("a"), short()],
            })
            .await;
        assert_refused(response);
        assert_eq!(queued(&server).await, vec![full("a"), full("b")]);
    }

    #[tokio::test]
    async fn full_hashes_are_accepted_by_every_queue_command() {
        let server = make_server();
        let ok =
            |response: Response| assert!(matches!(response, Response::Ok { .. }), "{response:?}");
        ok(server
            .handle_command(Command::QueueAppend {
                hash: full("b"),
                source: SourceName::audio(),
            })
            .await);
        ok(server
            .handle_command(Command::QueueNext {
                hash: full("a"),
                source: SourceName::audio(),
            })
            .await);
        assert_eq!(queued(&server).await, vec![full("a"), full("b")]);
        ok(server
            .handle_command(Command::QueueReplace {
                entries: vec![
                    (full("c"), SourceName::audio()),
                    (full("d"), SourceName::audio()),
                ],
            })
            .await);
        assert_eq!(queued(&server).await, vec![full("c"), full("d")]);
        let response = server
            .handle_command(Command::QueueRemove {
                hashes: vec![full("c")],
            })
            .await;
        assert!(matches!(
            response,
            Response::Ok {
                data: Some(ResponseData::Count(1))
            }
        ));
        assert_eq!(queued(&server).await, vec![full("d")]);
    }

    #[tokio::test]
    async fn load_queue_drops_persisted_entries_that_are_not_canonical() {
        let server = make_server();
        let persisted = [
            PersistEntry {
                hash: full("a").to_string(),
                source: SourceName::audio(),
            },
            PersistEntry {
                hash: short().to_string(),
                source: SourceName::audio(),
            },
        ];
        std::fs::write(&server.queue_file, serde_json::to_vec(&persisted).unwrap()).unwrap();
        let loaded = server.load_queue().await;
        let _ = std::fs::remove_file(&server.queue_file);
        let hashes: Vec<_> = loaded.iter().map(|e| e.hash.clone()).collect();
        assert_eq!(hashes, vec![h("a")]);
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
    fn output_change_same_health_publishes_nothing() {
        let r = Some(OutputHealth::Running);
        assert_eq!(output_change(r, r), None);
    }

    #[test]
    fn output_change_publishes_on_transition() {
        assert_eq!(
            output_change(Some(OutputHealth::Running), Some(OutputHealth::Stalled)),
            Some(PlaybackEvent::OutputChanged {
                health: OutputHealth::Stalled
            })
        );
    }

    #[test]
    fn output_change_first_report_publishes() {
        assert_eq!(
            output_change(None, Some(OutputHealth::Starting)),
            Some(PlaybackEvent::OutputChanged {
                health: OutputHealth::Starting
            })
        );
    }

    #[test]
    fn output_change_old_source_publishes_nothing() {
        assert_eq!(output_change(None, None), None);
        assert_eq!(output_change(Some(OutputHealth::Running), None), None);
    }

    #[test]
    fn apply_output_report_updates_last_only_on_a_change() {
        let mut last = None;
        assert_eq!(
            apply_output_report(&mut last, Some(OutputHealth::Stalled)),
            Some(PlaybackEvent::OutputChanged {
                health: OutputHealth::Stalled
            })
        );
        assert_eq!(last, Some(OutputHealth::Stalled));

        assert_eq!(
            apply_output_report(&mut last, Some(OutputHealth::Stalled)),
            None
        );
        assert_eq!(last, Some(OutputHealth::Stalled));

        // An old source reporting nothing never clears what was known.
        assert_eq!(apply_output_report(&mut last, None), None);
        assert_eq!(last, Some(OutputHealth::Stalled));

        assert_eq!(
            apply_output_report(&mut last, Some(OutputHealth::Running)),
            Some(PlaybackEvent::OutputChanged {
                health: OutputHealth::Running
            })
        );
        assert_eq!(last, Some(OutputHealth::Running));
    }

    #[test]
    fn get_output_status_is_read_only() {
        assert!(is_read_only(&Command::GetOutputStatus));
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

    // -------------------------------------------------------------------------
    // Seek and GetTransport
    // -------------------------------------------------------------------------

    fn loaded(hash: &str, state: StreamPlaybackState) -> StreamResponse {
        StreamResponse::Loaded {
            info: Some(StreamTrackInfo {
                state,
                content_hash: Some(full(hash)),
                title: None,
                artist: None,
                album: None,
                position_ms: Some(90_000),
                duration_ms: Some(240_000),
                preloaded: None,
            }),
            output: None,
        }
    }

    /// A source that accepts everything and reports `a` at 1:30 of 4:00.
    fn source_playing_a(command: &StreamCommand) -> StreamResponse {
        match command {
            StreamCommand::Loaded => loaded("a", StreamPlaybackState::Playing),
            _ => StreamResponse::Ok,
        }
    }

    fn seek_to(position_ms: u64) -> Command {
        Command::Seek {
            deck: media_protocol::Deck::A,
            position_ms: PositionMs::new(position_ms),
        }
    }

    fn transport_of(response: Response) -> TransportStatus {
        match response {
            Response::Ok {
                data: Some(ResponseData::Transport(status)),
            } => status,
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    async fn start(server: &Server, label: &str) {
        server
            .state
            .lock()
            .await
            .play_queue(h(label), SourceName::audio());
    }

    #[tokio::test]
    async fn seek_while_playing_sends_the_position_and_stays_playing() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;

        let response = harness.server.handle_command(seek_to(90_000)).await;

        let status = transport_of(response);
        assert_eq!(
            status,
            TransportStatus::Loaded {
                transport: Transport::Playing,
                now: full("a"),
                position_ms: Some(PositionMs::new(90_000)),
                duration_ms: Some(PositionMs::new(240_000)),
                preview: None,
            }
        );
        assert_eq!(
            harness.sent(),
            vec![
                StreamCommand::Seek {
                    position_ms: PositionMs::new(90_000)
                },
                // The follow-up for the PositionUpdate and the answer.
                StreamCommand::Loaded,
            ]
        );
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
    }

    /// The source keeps a seek within the track; the answer is where the
    /// track is, not what was asked for.
    #[tokio::test]
    async fn the_answer_to_a_seek_is_where_the_source_says_the_track_is() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;

        // 99:00 into a 4:00 track; the stub source reports 1:30.
        let status = transport_of(harness.server.handle_command(seek_to(5_940_000)).await);

        assert_eq!(status.position(), Some(PositionMs::new(90_000)));
    }

    #[tokio::test]
    async fn seek_while_paused_stays_paused_and_never_plays() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.state.lock().await.pause();

        let response = harness.server.handle_command(seek_to(5_000)).await;

        assert!(matches!(response, Response::Ok { .. }), "{response:?}");
        assert!(harness
            .sent()
            .iter()
            .all(|c| !matches!(c, StreamCommand::Play)));
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Paused { hash: h("a") }
        );
    }

    #[tokio::test]
    async fn seek_while_idle_says_nothing_is_loaded_and_asks_no_one() {
        let harness = make_harness(source_playing_a);

        let response = harness.server.handle_command(seek_to(1_000)).await;

        let Response::Err { message } = response else {
            panic!("expected Err, got {response:?}");
        };
        assert_eq!(message, "nothing loaded");
        assert!(harness.sent().is_empty());
        assert_eq!(harness.server.seek_epoch.current(), 0);
    }

    /// An older source does not know `seek` and answers "invalid command".
    #[tokio::test]
    async fn seek_on_an_old_source_is_refused_and_changes_nothing() {
        let harness = make_harness(|command| match command {
            StreamCommand::Seek { .. } => StreamResponse::Error {
                message: "invalid command: unknown variant `seek`".to_string(),
            },
            other => source_playing_a(other),
        });
        start(&harness.server, "a").await;

        let response = harness.server.handle_command(seek_to(1_000)).await;

        let Response::Err { message } = response else {
            panic!("expected Err, got {response:?}");
        };
        assert!(message.contains("invalid command"), "{message}");
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
        assert_eq!(
            harness.server.seek_epoch.current(),
            0,
            "the source kept its preload, so the loop keeps its belief"
        );
    }

    #[tokio::test]
    async fn a_position_update_follows_a_seek() {
        let harness = make_harness(source_playing_a);
        let events = harness.subscribe();
        start(&harness.server, "a").await;

        harness.server.handle_command(seek_to(90_000)).await;

        let msg = events.recv().expect("an event after the seek");
        let (_, event) = event_protocol::from_topic_message(msg.as_slice()).unwrap();
        assert_eq!(
            event,
            PlaybackEvent::PositionUpdate {
                hash: full("a"),
                position_ms: 90_000,
                duration_ms: 240_000,
            }
        );
    }

    #[tokio::test]
    async fn get_transport_while_idle_has_no_track() {
        let harness = make_harness(source_playing_a);

        let response = harness
            .server
            .handle_command(Command::GetTransport {
                deck: media_protocol::Deck::A,
            })
            .await;

        assert_eq!(transport_of(response), TransportStatus::Idle);
        assert!(harness.sent().is_empty(), "idle needs no source");
    }

    #[tokio::test]
    async fn get_transport_reports_state_track_position_and_duration() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.state.lock().await.pause();

        let response = harness
            .server
            .handle_command(Command::GetTransport {
                deck: media_protocol::Deck::A,
            })
            .await;

        assert_eq!(
            transport_of(response),
            TransportStatus::Loaded {
                transport: Transport::Paused,
                now: full("a"),
                position_ms: Some(PositionMs::new(90_000)),
                duration_ms: Some(PositionMs::new(240_000)),
                preview: None,
            }
        );
    }

    #[test]
    fn get_transport_is_read_only_and_seek_is_not() {
        assert!(is_read_only(&Command::GetTransport {
            deck: media_protocol::Deck::A
        }));
        assert!(!is_read_only(&seek_to(0)));
    }

    // -------------------------------------------------------------------------
    // The trap: a seek drops the source's preload
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn the_preload_is_sent_once_while_nothing_changes() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.queue.lock().await.push_back(entry("b"));
        let task = harness.server.poll_task();
        let mut poll = PollState::default();

        for _ in 0..3 {
            poll_once(&task, &mut poll).await;
        }

        assert_eq!(harness.sent_preloads(), vec![full("b")]);
    }

    /// Regression: the engine drops the preload on seek, so the loop must send
    /// it again even though the track (and the queue front) did not change.
    #[tokio::test]
    async fn the_preload_is_resent_after_a_seek() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.queue.lock().await.push_back(entry("b"));
        let task = harness.server.poll_task();
        let mut poll = PollState::default();
        poll_once(&task, &mut poll).await;
        poll_once(&task, &mut poll).await;
        assert_eq!(harness.sent_preloads(), vec![full("b")]);

        harness.server.handle_command(seek_to(90_000)).await;
        poll_once(&task, &mut poll).await;

        assert_eq!(harness.sent_preloads(), vec![full("b"), full("b")]);
        poll_once(&task, &mut poll).await;
        assert_eq!(harness.sent_preloads().len(), 2, "and only once per seek");
    }

    /// A seek means the source dropped its preload, so it is sent again. What
    /// the source may still switch to stays known: clearing it would let a
    /// switch the source already committed to look like an unknown track.
    #[test]
    fn a_seek_makes_the_book_send_the_preload_again_but_keeps_what_is_known() {
        let mut book = PreloadBook::default();
        book.observe(&h("a"), 0);
        book.last_sent = Some(h("b"));
        book.known.push(h("b"));

        book.observe(&h("a"), 0);
        assert_eq!(book.last_sent, Some(h("b")));

        book.observe(&h("a"), 1);
        assert_eq!(book.last_sent, None);
        assert_eq!(book.known, vec![h("b")]);
    }

    #[test]
    fn a_new_track_makes_the_book_forget_everything() {
        let mut book = PreloadBook::default();
        book.observe(&h("a"), 0);
        book.last_sent = Some(h("b"));
        book.known.push(h("b"));

        book.observe(&h("b"), 0);
        assert_eq!(book.last_sent, None);
        assert!(book.known.is_empty());
    }

    #[test]
    fn transport_status_of_a_playing_track_without_source_info_has_no_position() {
        let status = transport_status(&PlaybackState::Playing { hash: h("a") }, None);
        assert_eq!(
            status,
            TransportStatus::Loaded {
                transport: Transport::Playing,
                now: full("a"),
                position_ms: None,
                duration_ms: None,
                preview: None,
            }
        );
    }

    #[test]
    fn transport_status_while_idle_is_idle_whatever_the_source_says() {
        let StreamResponse::Loaded { info, .. } = loaded("a", StreamPlaybackState::Playing) else {
            unreachable!()
        };
        assert_eq!(
            transport_status(&PlaybackState::Idle, info.as_ref()),
            TransportStatus::Idle
        );
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

    #[tokio::test]
    async fn invalid_message_gets_error_response_and_server_continues() {
        let server = make_server();
        let response = server.handle_message(b"not json at all").await;
        assert!(matches!(response, Response::Err { .. }));
        let response = server.handle_message(br#"{"cmd":"bogus"}"#).await;
        assert!(matches!(response, Response::Err { .. }));
        let valid = serde_json::to_vec(&Command::QueueList).unwrap();
        let response = server.handle_message(&valid).await;
        assert!(matches!(response, Response::Ok { .. }));
    }
}
