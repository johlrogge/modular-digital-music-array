use crate::error::ServerError;
use crate::playback_state::{
    BackTo, Observed, PlaybackEffect, PlaybackState, PlaybackStateMachine, Refusal,
};
use acid_client::AcidClient;
use color_eyre::Result;
use event_protocol::{to_topic_message, PlaybackEvent};
use library_ipc_client::{resolve_full_hash, ClientError, LibraryClient, ResolveError, TrackInfo};
use media_protocol::{
    require_canonical, CanonicalHash, Command, ContentHash, Held, PreviewStatus, Response,
    ResponseData, SourceName, Transport, TransportStatus,
};
use music_facts::{FactSource, MusicValue};
use nng::Socket;
use playback_primitives::PositionMs;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// How long one library request may take before it counts as unreachable.
pub const LIBRARY_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The queue read from the queue file at startup; see [`restore`].
#[derive(Default)]
pub struct RestoredQueue(VecDeque<QueueEntry>);

/// Read the persisted queue, resolving entries written as prefixes through the
/// library (ADR-026, amendment 2026-11-18). Blocks, possibly for a minute, so
/// run it before the command socket is bound: a
/// request that arrived meanwhile would be served after its client gave up.
pub fn restore(queue_file: &Path, library: &LibraryClient) -> RestoredQueue {
    RestoredQueue(restore_queue(
        queue_file,
        &|hash| library.get_track(hash),
        &ResolveRetry::startup(),
    ))
}

/// How long and how often [`restore_queue`] asks an unreachable library again.
struct ResolveRetry {
    /// Waits between the first attempts.
    initial: Vec<Duration>,
    /// The wait after `initial` is used up, repeated.
    steady: Duration,
    /// How long one request may take, at worst.
    request_timeout: Duration,
    /// Total time after which the library counts as unreachable, counting the
    /// request that is still running at the end.
    deadline: Duration,
}

impl ResolveRetry {
    /// At startup the library may be up but not answering yet: 0.5, 1, 2, 4,
    /// 8 s, then every 8 s, for at most 60 s.
    fn startup() -> Self {
        Self {
            initial: [500, 1000, 2000, 4000]
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
            steady: Duration::from_secs(8),
            request_timeout: LIBRARY_REQUEST_TIMEOUT,
            deadline: Duration::from_secs(60),
        }
    }

    fn delay(&self, attempt: usize) -> Duration {
        self.initial.get(attempt).copied().unwrap_or(self.steady)
    }
}

/// Where the original of a queue file is kept when it could not be restored as
/// written.
fn rejected_path(queue_file: &Path) -> PathBuf {
    let mut name = queue_file.as_os_str().to_owned();
    name.push(".rejected");
    PathBuf::from(name)
}

/// Resolve `prefix` through the library's resolver, asking again while the
/// library cannot be reached and `retry` allows it. Stops at the first answer
/// that is not a transport failure.
fn resolve_with_retry(
    prefix: &ContentHash,
    resolve: &dyn Fn(&ContentHash) -> Result<TrackInfo, ClientError>,
    retry: &ResolveRetry,
    started: Instant,
) -> Result<CanonicalHash, ResolveError> {
    let mut attempt = 0;
    loop {
        match resolve_full_hash(prefix, resolve) {
            Err(e) if e.is_transport() => {
                let delay = retry.delay(attempt);
                if started.elapsed() + delay + retry.request_timeout > retry.deadline {
                    return Err(e);
                }
                warn!("Library unreachable resolving {prefix}; retrying in {delay:?}");
                std::thread::sleep(delay);
                attempt += 1;
            }
            answer => return answer,
        }
    }
}

/// Write `data` to `rejected` unless it exists: the first original is kept,
/// and a later one never replaces it. An existing file counts as success.
fn keep_original(data: &[u8], rejected: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(rejected)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    };
    let written = file.write_all(data).and_then(|()| file.sync_all());
    if written.is_err() {
        // Do not leave half a copy that would stop the next try.
        let _ = std::fs::remove_file(rejected);
    }
    written
}

/// Read the persisted queue. Entries that are not canonical (written as
/// prefixes by playback up to 0.38) are resolved through the library, in file
/// order, so a mixed file keeps its order. Entries the library cannot resolve
/// are dropped with a warning.
///
/// A file with any such entry is first copied to `<file>.rejected`, and a
/// corrupt file is moved to it, so the original is never lost. An existing
/// `.rejected` is never overwritten: the first original wins. This runs
/// before anything can persist the queue again.
fn restore_queue(
    queue_file: &Path,
    resolve: &dyn Fn(&ContentHash) -> Result<TrackInfo, ClientError>,
    retry: &ResolveRetry,
) -> VecDeque<QueueEntry> {
    let data = match std::fs::read(queue_file) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return VecDeque::new();
        }
        Err(e) => {
            warn!("Failed to read queue file {:?}: {}", queue_file, e);
            return VecDeque::new();
        }
    };
    let rejected = rejected_path(queue_file);

    let entries: Vec<PersistEntry> = match serde_json::from_slice(&data) {
        Ok(e) => e,
        Err(e) => {
            // Remove the original only once its copy is safe. If a `.rejected`
            // already exists, the earlier original wins and this file's bytes
            // are discarded.
            match keep_original(&data, &rejected) {
                Ok(()) => {
                    warn!("Queue file corrupt ({e}); original kept at {rejected:?}");
                    if let Err(e) = std::fs::remove_file(queue_file) {
                        warn!("Failed to remove the corrupt queue file: {e}");
                    }
                }
                Err(keep) => {
                    warn!("Queue file corrupt ({e}); failed to keep it at {rejected:?}: {keep}")
                }
            }
            return VecDeque::new();
        }
    };

    let entries: Vec<_> = entries
        .into_iter()
        .map(|e| {
            let hash = ContentHash::new(e.hash);
            (CanonicalHash::try_from(hash).map_err(|n| n.0), e.source)
        })
        .collect();
    // Resolving loses nothing; only a later persist does. So a failed copy is
    // a warning, and the restore goes on as usual.
    if entries.iter().any(|(hash, _)| hash.is_err()) {
        if let Err(e) = keep_original(&data, &rejected) {
            warn!("Failed to keep the original queue file at {rejected:?}: {e}");
        }
    }

    let started = Instant::now();
    // Set once the library has not answered in time, so that each remaining
    // prefix does not cost another request timeout.
    let mut unreachable = false;
    let mut dropped = 0usize;
    let mut queue = VecDeque::new();
    for (hash, source) in entries {
        let hash = match hash {
            Ok(hash) => hash,
            Err(_) if unreachable => {
                dropped += 1;
                continue;
            }
            Err(prefix) => match resolve_with_retry(&prefix, resolve, retry, started) {
                Ok(hash) => hash,
                Err(e) => {
                    dropped += 1;
                    if e.is_transport() {
                        unreachable = true;
                        warn!(
                            "Library unreachable; original kept at {:?}: {}",
                            rejected, e
                        );
                    } else {
                        warn!(
                            hash = %prefix,
                            error = %e,
                            "Dropping persisted queue entry"
                        );
                    }
                    continue;
                }
            },
        };
        queue.push_back(QueueEntry { hash, source });
    }
    info!(
        "Restored {} track(s) from queue file, dropped {}",
        queue.len(),
        dropped
    );
    queue
}

impl Server {
    pub fn new(
        audio: Arc<Mutex<StreamClient>>,
        socket: Socket,
        queue_file: PathBuf,
        event_pub: Socket,
        acid_client: Arc<AcidClient>,
        restored: RestoredQueue,
    ) -> Self {
        Self {
            audio,
            socket,
            queue: Arc::new(Mutex::new(restored.0)),
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

    /// Offer the queue front to a transition of the state machine; see
    /// [`offer_front`]. The state is locked before the queue, as the poll task
    /// does. Persists and announces the queue if it lost its front.
    async fn offer_front(
        &self,
        transition: impl FnOnce(
            &mut PlaybackStateMachine,
            Option<(CanonicalHash, SourceName)>,
        ) -> Result<Vec<PlaybackEffect>, Refusal>,
    ) -> Result<Vec<PlaybackEffect>, Refusal> {
        let mut sm = self.state.lock().await;
        let mut queue = self.queue.lock().await;
        let (effects, popped) = offer_front(&mut sm, &mut queue, transition)?;
        if popped.is_some() {
            self.queue_changed(&queue);
        }
        Ok(effects)
    }

    /// Move playback along the queue with `transition` (skip, play the front)
    /// and run its effects; a refusal answers the command and changes nothing.
    async fn advance_queue(
        &self,
        transition: impl FnOnce(
            &mut PlaybackStateMachine,
            Option<(CanonicalHash, SourceName)>,
        ) -> Result<Vec<PlaybackEffect>, Refusal>,
    ) -> Response {
        match self.offer_front(transition).await {
            Err(refusal) => refusal_response(refusal),
            Ok(effects) => {
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
        }
    }

    pub async fn run(&self) -> Result<(), ServerError> {
        info!("Playback server starting...");

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
                // Resumes a paused track or preview; otherwise a no-op.
                let (effects, idle) = {
                    let mut sm = self.state.lock().await;
                    (sm.resume(), matches!(sm.state(), PlaybackState::Idle))
                };
                if idle {
                    warn!("Play command received while Idle: nothing to resume");
                }
                let result =
                    execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
                self.create_response(result, None)
            }
            Command::Preview { deck: _, hash } => {
                let hash = match require_canonical(hash) {
                    Ok(hash) => hash,
                    Err(refusal) => return refusal,
                };
                info!("Preview {}", hash);
                self.preview(hash).await
            }
            Command::EndPreview { deck: _ } => {
                info!("Ending preview");
                let effects = self.state.lock().await.end_preview();
                let response = self.run_preview_effects(effects).await;
                // Say what is audible afterwards: the track that was held, or
                // nothing.
                match response {
                    Response::Ok { .. } => {
                        let now: Option<ContentHash> =
                            self.state.lock().await.current_hash().map(Into::into);
                        Response::Ok {
                            data: Some(ResponseData::NowPlaying(now)),
                        }
                    }
                    err => err,
                }
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

                self.advance_queue(|sm, next| sm.play_queue(next)).await
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
                self.advance_queue(|sm, next| sm.skip(next)).await
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

    /// Preview `hash` over what is audible. The track and position to come
    /// back to are asked of the source now: the state machine does not track
    /// them, and refuses the preview when they are missing or are not the
    /// track it has as audible (a gapless switch in between, an unreadable
    /// position, a source that could not be asked).
    async fn preview(&self, hash: CanonicalHash) -> Response {
        let state = self.state.lock().await.state().clone();
        let observed = self
            .ask_source(&state)
            .await
            .as_ref()
            .and_then(observed_position);
        match self.state.lock().await.preview(hash, observed) {
            Err(refusal) => refusal_response(refusal),
            Ok(effects) => self.run_preview_effects(effects).await,
        }
    }

    /// Run what a preview transition asked for. Loading drops the source's
    /// preload, which the poll loop is told like after a seek (see
    /// [`SeekEpoch`]), after the source has done it. The bump is
    /// unconditional on purpose: after a no-op `EndPreview` it costs one
    /// preload that the source treats as already held.
    async fn run_preview_effects(&self, effects: Vec<PlaybackEffect>) -> Response {
        let result =
            execute_effects(effects, &self.audio, &self.event_pub, &self.acid_client).await;
        self.seek_epoch.bump();
        self.create_response(result, None)
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
            PlaybackEffect::LoadAt { hash, at, then } => {
                // Load, seek, then play if it was playing: the same seek as
                // `Command::Seek`, which leaves the play state as it was.
                let client = audio.lock().await;
                client.load(hash.clone().into()).inspect_err(|e| {
                    warn!("Failed to load {}: {}", hash, e);
                })?;
                client.seek(at).inspect_err(|e| {
                    warn!("Failed to seek {} to {}: {}", hash, at, e);
                })?;
                if then == Transport::Playing {
                    client.play().inspect_err(|e| {
                        warn!("Failed to play after load: {}", e);
                    })?;
                }
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

/// The track the audio source should have preloaded: the front of the queue
/// while a track is Playing or Paused, nothing otherwise (a preview is not
/// followed by the queue: playback goes back to what it held).
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
        PlaybackState::Idle | PlaybackState::Previewing { .. } => None,
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

/// Offer the queue front to a transition of the state machine and pop it only
/// if the transition took it: one that did nothing (a stale observation, no
/// effects) or refused (`Err`, while previewing) leaves the queue as it was.
/// Returns the effects and the popped entry, if any.
fn offer_front<E>(
    sm: &mut PlaybackStateMachine,
    queue: &mut VecDeque<QueueEntry>,
    transition: impl FnOnce(
        &mut PlaybackStateMachine,
        Option<(CanonicalHash, SourceName)>,
    ) -> Result<Vec<PlaybackEffect>, E>,
) -> Result<(Vec<PlaybackEffect>, Option<QueueEntry>), E> {
    let next = queue.front().map(|e| (e.hash.clone(), e.source.clone()));
    let effects = transition(sm, next)?;
    if effects.is_empty() {
        return Ok((effects, None));
    }
    Ok((effects, queue.pop_front()))
}

/// Track finished without a switch: offer the queue front to the state machine
/// and pop it only if the state machine transitioned.
fn finish_and_dequeue(
    sm: &mut PlaybackStateMachine,
    queue: &mut VecDeque<QueueEntry>,
    finished: &CanonicalHash,
) -> (Vec<PlaybackEffect>, Option<QueueEntry>) {
    let Ok(taken) = offer_front::<std::convert::Infallible>(sm, queue, |sm, next| {
        Ok(sm.track_ended(finished, next))
    });
    taken
}

/// The answer to a command the state machine refused.
fn refusal_response(refusal: Refusal) -> Response {
    Response::Err {
        message: refusal.to_string(),
    }
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
        PlaybackState::Playing { hash }
        | PlaybackState::Paused { hash }
        | PlaybackState::Previewing { hash, .. } => Some(hash),
    }
}

/// The audible track while it plays, a preview too; none when paused or idle.
fn playing_hash(state: &PlaybackState) -> Option<&CanonicalHash> {
    match state {
        PlaybackState::Playing { hash }
        | PlaybackState::Previewing {
            hash,
            transport: Transport::Playing,
            ..
        } => Some(hash),
        PlaybackState::Idle | PlaybackState::Paused { .. } | PlaybackState::Previewing { .. } => {
            None
        }
    }
}

/// Which track the source says is audible and where, when it says both.
fn observed_position(info: &StreamTrackInfo) -> Option<Observed> {
    Some(Observed {
        hash: CanonicalHash::try_from(info.content_hash.clone()?).ok()?,
        at: PositionMs::new(info.position_ms?),
    })
}

/// Whether a poll's `Finished` is about `current`: only the track the state
/// machine has as audible can have finished. A late `Finished` for the track
/// that was audible before (a preview that just ended) is not this one's.
fn finished_current(
    finished: bool,
    heard: Option<&CanonicalHash>,
    current: &CanonicalHash,
) -> bool {
    finished && heard == Some(current)
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
    let (transport, hash, preview) = match state {
        PlaybackState::Idle => return TransportStatus::Idle,
        PlaybackState::Playing { hash } => (Transport::Playing, hash, None),
        PlaybackState::Paused { hash } => (Transport::Paused, hash, None),
        PlaybackState::Previewing {
            hash,
            transport,
            back_to,
        } => (
            *transport,
            hash,
            Some(PreviewStatus {
                back_to: match back_to {
                    BackTo::Nothing => None,
                    BackTo::Track {
                        hash,
                        at,
                        transport,
                    } => Some(Held {
                        hash: hash.into(),
                        at: *at,
                        transport: *transport,
                    }),
                },
            }),
        ),
    };
    TransportStatus::Loaded {
        transport,
        now: hash.clone().into(),
        position_ms: info.and_then(|i| i.position_ms).map(PositionMs::new),
        duration_ms: info.and_then(|i| i.duration_ms).map(PositionMs::new),
        preview,
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

    // Every 5th tick (~1 s) broadcast a position update while playing, a
    // preview too.
    if tick.is_multiple_of(5) {
        let current_state = state.lock().await;
        if let Some(h) = playing_hash(current_state.state()).cloned() {
            drop(current_state);
            publish_position(audio, event_pub, &h).await;
        }
    }

    let snapshot = state.lock().await.state().clone();
    let Some(current_hash) = state_hash(&snapshot).cloned() else {
        // Stopped: the source dropped its preload; make sure we re-send later.
        book.idle();
        // The output keeps running while Idle, so its health stays
        // meaningful: keep watching it (~1 s).
        if tick.is_multiple_of(5) {
            let _ = refresh_output(audio, event_pub, output_cache, last_output).await;
        }
        return;
    };
    let is_playing = playing_hash(&snapshot).is_some();
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
    // Both branches below act on `finished` for the audible track only.
    let finished = finished_current(finished, heard.as_ref(), &current_hash);

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

    if matches!(snapshot, PlaybackState::Previewing { .. }) {
        // No preload while previewing (`desired_preload` is none). The preview
        // ended by itself: go back, whatever the queue holds.
        reconcile_preload(audio, queue, &snapshot, book).await;
        if finished {
            let effects = state.lock().await.preview_finished(&current_hash);
            if effects.is_empty() {
                return;
            }
            info!("Preview finished: {:?}", current_hash);
            if let Err(e) = execute_effects(effects, audio, event_pub, acid_client).await {
                warn!("Going back after a preview failed: {}", e);
            }
            // The source dropped its preload when it loaded the held track.
            seek_epoch.bump();
        }
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
    use library_ipc_client::ProtocolError;
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
            RestoredQueue::default(),
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
            sm.play_queue(Some((h("paused_track"), SourceName::audio())))
                .unwrap();
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
            sm.play_queue(Some((h("test"), SourceName::audio())))
                .unwrap();
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
        let _effects = server.state.lock().await.skip(None).unwrap();

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

    type LibraryAnswer = Result<TrackInfo, ClientError>;

    fn track_with(hash: ContentHash) -> TrackInfo {
        TrackInfo::new(hash)
    }

    /// The full hash a fake library answers for a prefix.
    fn expanded(prefix: &str) -> ContentHash {
        ContentHash::new(format!("sha256:{prefix:0<64}"))
    }

    fn canonical(prefix: &str) -> CanonicalHash {
        CanonicalHash::try_from(expanded(prefix)).unwrap()
    }

    /// A fake library that knows every prefix.
    fn knows_all(hash: &ContentHash) -> LibraryAnswer {
        Ok(track_with(expanded(hash.as_str())))
    }

    fn unreachable_library() -> ClientError {
        ClientError::from(nng::Error::TimedOut)
    }

    fn quick_retry() -> ResolveRetry {
        ResolveRetry {
            initial: vec![],
            steady: Duration::from_millis(1),
            request_timeout: Duration::ZERO,
            deadline: Duration::from_secs(5),
        }
    }

    fn persisted_json(hashes: &[&str]) -> Vec<u8> {
        let entries: Vec<_> = hashes
            .iter()
            .map(|hash| PersistEntry {
                hash: (*hash).to_string(),
                source: SourceName::audio(),
            })
            .collect();
        serde_json::to_vec(&entries).unwrap()
    }

    fn restored_hashes(queue: &VecDeque<QueueEntry>) -> Vec<CanonicalHash> {
        queue.iter().map(|e| e.hash.clone()).collect()
    }

    /// Restore a queue file holding `data` with `resolve`; returns the queue
    /// and the file's path (the directory lives as long as the guard).
    fn restore_from(
        data: &[u8],
        resolve: &dyn Fn(&ContentHash) -> LibraryAnswer,
        retry: &ResolveRetry,
    ) -> (VecDeque<QueueEntry>, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("queue.json");
        std::fs::write(&file, data).unwrap();
        let queue = restore_queue(&file, resolve, retry);
        (queue, file, dir)
    }

    #[test]
    fn restore_queue_migrates_a_queue_written_by_0_38() {
        let fixture = include_bytes!("../tests/fixtures/queue-0.38.json");
        let (queue, file, _dir) = restore_from(fixture, &knows_all, &quick_retry());
        assert_eq!(
            restored_hashes(&queue),
            vec![
                canonical("c7ef816a7705"),
                canonical("0123456789ab"),
                canonical("fedcba987654"),
            ]
        );
        assert_eq!(std::fs::read(rejected_path(&file)).unwrap(), fixture);
    }

    #[test]
    fn restore_queue_keeps_the_order_of_a_mixed_file() {
        let data = persisted_json(&[
            expanded("a1").as_str(),
            "9fb4105e0a1b",
            expanded("b2").as_str(),
        ]);
        let (queue, _file, _dir) = restore_from(&data, &knows_all, &quick_retry());
        assert_eq!(
            restored_hashes(&queue),
            vec![canonical("a1"), canonical("9fb4105e0a1b"), canonical("b2")]
        );
    }

    #[test]
    fn restore_queue_drops_unknown_and_ambiguous_prefixes_and_keeps_the_original() {
        let resolve = |hash: &ContentHash| -> LibraryAnswer {
            match hash.as_str() {
                "aaaaaaaaaaaa" => Err(ClientError::Protocol(ProtocolError::TrackNotFound {
                    hash: hash.to_string(),
                })),
                "bbbbbbbbbbbb" => Err(ClientError::Protocol(ProtocolError::AmbiguousHash {
                    hash: hash.to_string(),
                    candidates: vec![expanded("b1").to_string(), expanded("b2").to_string()],
                })),
                _ => knows_all(hash),
            }
        };
        let data = persisted_json(&[
            "aaaaaaaaaaaa",
            expanded("d1").as_str(),
            "bbbbbbbbbbbb",
            "cccccccccccc",
        ]);
        let (queue, file, _dir) = restore_from(&data, &resolve, &quick_retry());
        assert_eq!(
            restored_hashes(&queue),
            vec![canonical("d1"), canonical("cccccccccccc")]
        );
        assert_eq!(std::fs::read(rejected_path(&file)).unwrap(), data);
    }

    #[test]
    fn restore_queue_retries_while_the_library_is_unreachable() {
        let asked = std::cell::Cell::new(0);
        let resolve = |hash: &ContentHash| -> LibraryAnswer {
            asked.set(asked.get() + 1);
            if asked.get() <= 2 {
                Err(unreachable_library())
            } else {
                knows_all(hash)
            }
        };
        let data = persisted_json(&["c7ef816a7705", "0123456789ab"]);
        let (queue, _file, _dir) = restore_from(&data, &resolve, &quick_retry());
        assert_eq!(
            restored_hashes(&queue),
            vec![canonical("c7ef816a7705"), canonical("0123456789ab")]
        );
        assert_eq!(asked.get(), 4);
    }

    #[test]
    fn restore_queue_gives_up_within_the_deadline_and_keeps_canonical_entries() {
        let resolve = |_: &ContentHash| -> LibraryAnswer { Err(unreachable_library()) };
        let retry = ResolveRetry {
            initial: vec![],
            steady: Duration::from_millis(10),
            request_timeout: Duration::from_millis(20),
            deadline: Duration::from_millis(100),
        };
        let data = persisted_json(&[
            expanded("a1").as_str(),
            "c7ef816a7705",
            expanded("b2").as_str(),
            "0123456789ab",
        ]);
        let started = std::time::Instant::now();
        let (queue, file, _dir) = restore_from(&data, &resolve, &retry);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            restored_hashes(&queue),
            vec![canonical("a1"), canonical("b2")]
        );
        assert_eq!(std::fs::read(rejected_path(&file)).unwrap(), data);
    }

    #[test]
    fn restore_queue_of_canonical_entries_writes_no_copy_and_asks_nothing() {
        let asked = std::cell::Cell::new(0);
        let resolve = |hash: &ContentHash| -> LibraryAnswer {
            asked.set(asked.get() + 1);
            knows_all(hash)
        };
        let data = persisted_json(&[expanded("a1").as_str(), expanded("b2").as_str()]);
        let (queue, file, _dir) = restore_from(&data, &resolve, &quick_retry());
        assert_eq!(
            restored_hashes(&queue),
            vec![canonical("a1"), canonical("b2")]
        );
        assert_eq!(asked.get(), 0);
        assert!(!rejected_path(&file).exists());
    }

    #[test]
    fn restore_queue_moves_a_corrupt_file_instead_of_deleting_it() {
        let (queue, file, _dir) = restore_from(b"not json", &knows_all, &quick_retry());
        assert!(queue.is_empty());
        assert!(!file.exists());
        assert_eq!(std::fs::read(rejected_path(&file)).unwrap(), b"not json");
    }

    #[test]
    fn restore_queue_never_overwrites_an_existing_rejected_file_for_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("queue.json");
        std::fs::write(rejected_path(&file), b"the first original").unwrap();
        std::fs::write(&file, b"not json").unwrap();
        let queue = restore_queue(&file, &knows_all, &quick_retry());
        assert!(queue.is_empty());
        assert!(!file.exists());
        assert_eq!(
            std::fs::read(rejected_path(&file)).unwrap(),
            b"the first original"
        );
    }

    #[test]
    fn restore_queue_keeps_the_original_through_a_persist_and_a_second_restore() {
        let fixture = include_bytes!("../tests/fixtures/queue-0.38.json");
        let (queue, file, _dir) = restore_from(fixture, &knows_all, &quick_retry());
        persist_queue_to_file(&file, &queue);
        let again = restore_queue(&file, &knows_all, &quick_retry());
        assert_eq!(restored_hashes(&again), restored_hashes(&queue));
        assert_eq!(std::fs::read(rejected_path(&file)).unwrap(), fixture);
    }

    #[test]
    fn restore_queue_still_resolves_when_the_original_cannot_be_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("queue.json");
        let data = persisted_json(&[expanded("a1").as_str(), "c7ef816a7705"]);
        std::fs::write(&file, &data).unwrap();
        // A read-only directory makes creating the copy fail with an error
        // other than AlreadyExists.
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap()
        };
        dir_mode(0o555);
        let asked = std::cell::Cell::new(0);
        let resolve = |hash: &ContentHash| -> LibraryAnswer {
            asked.set(asked.get() + 1);
            knows_all(hash)
        };
        let queue = restore_queue(&file, &resolve, &quick_retry());
        dir_mode(0o755);
        assert_eq!(
            restored_hashes(&queue),
            vec![canonical("a1"), canonical("c7ef816a7705")]
        );
        assert_eq!(asked.get(), 1);
        assert!(!rejected_path(&file).exists());
    }

    #[test]
    fn restore_queue_of_a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let queue = restore_queue(&dir.path().join("queue.json"), &knows_all, &quick_retry());
        assert!(queue.is_empty());
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
        sm.play_queue(Some((h("a"), SourceName::audio()))).unwrap();
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
            .play_queue(Some((h(label), SourceName::audio())))
            .unwrap();
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
    // Preview hold (ADR-031)
    // -------------------------------------------------------------------------

    fn preview_command(label: &str) -> Command {
        Command::Preview {
            deck: media_protocol::Deck::A,
            hash: full(label),
        }
    }

    fn end_preview_command() -> Command {
        Command::EndPreview {
            deck: media_protocol::Deck::A,
        }
    }

    fn get_transport_command() -> Command {
        Command::GetTransport {
            deck: media_protocol::Deck::A,
        }
    }

    fn load_of(label: &str) -> StreamCommand {
        StreamCommand::Load {
            content_hash: full(label),
        }
    }

    fn assert_ok(response: Response) {
        assert!(matches!(response, Response::Ok { .. }), "{response:?}");
    }

    fn held_a_at_1_30(transport: Transport) -> crate::playback_state::BackTo {
        crate::playback_state::BackTo::Track {
            hash: h("a"),
            at: PositionMs::new(90_000),
            transport,
        }
    }

    /// `a` plays at 1:30 and `b` is previewed over it.
    async fn previewing_b_over_a(harness: &Harness) {
        start(&harness.server, "a").await;
        assert_ok(harness.server.handle_command(preview_command("b")).await);
    }

    #[tokio::test]
    async fn a_preview_holds_where_the_source_says_the_audible_track_is() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;

        assert_ok(harness.server.handle_command(preview_command("b")).await);

        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Previewing {
                hash: h("b"),
                transport: Transport::Playing,
                back_to: held_a_at_1_30(Transport::Playing),
            }
        );
        assert_eq!(
            harness.sent(),
            vec![StreamCommand::Loaded, load_of("b"), StreamCommand::Play]
        );
    }

    #[tokio::test]
    async fn a_preview_from_idle_asks_no_one_and_holds_nothing() {
        let harness = make_harness(source_playing_a);

        assert_ok(harness.server.handle_command(preview_command("b")).await);

        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Previewing {
                hash: h("b"),
                transport: Transport::Playing,
                back_to: crate::playback_state::BackTo::Nothing,
            }
        );
        assert_eq!(harness.sent(), vec![load_of("b"), StreamCommand::Play]);
    }

    #[tokio::test]
    async fn a_preview_refuses_a_prefix_and_changes_nothing() {
        let harness = make_harness(source_playing_a);

        let response = harness
            .server
            .handle_command(Command::Preview {
                deck: media_protocol::Deck::A,
                hash: short(),
            })
            .await;

        assert_refused(response);
        assert!(harness.sent().is_empty());
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Idle
        );
    }

    /// A preview over a playing `a` whose source answers `Loaded` with `reply`
    /// is refused, and nothing is loaded or changed.
    async fn assert_preview_refused_when_source_says(reply: StreamResponse) {
        let harness = make_harness(move |command| match command {
            StreamCommand::Loaded => reply.clone(),
            _ => StreamResponse::Ok,
        });
        start(&harness.server, "a").await;

        let response = harness.server.handle_command(preview_command("b")).await;

        let Response::Err { message } = response else {
            panic!("expected a refusal, got {response:?}");
        };
        assert_eq!(
            message,
            "Cannot preview: the source does not say where the playing track is"
        );
        assert_eq!(harness.sent(), vec![StreamCommand::Loaded]);
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
        assert_eq!(harness.server.seek_epoch.current(), 0);
    }

    #[tokio::test]
    async fn a_preview_is_refused_when_the_source_does_not_say_the_position() {
        assert_preview_refused_when_source_says(StreamResponse::Loaded {
            info: Some(StreamTrackInfo {
                state: StreamPlaybackState::Playing,
                content_hash: Some(full("a")),
                title: None,
                artist: None,
                album: None,
                position_ms: None,
                duration_ms: None,
                preloaded: None,
            }),
            output: None,
        })
        .await;
    }

    #[tokio::test]
    async fn a_preview_is_refused_when_the_source_cannot_be_asked() {
        assert_preview_refused_when_source_says(StreamResponse::Error {
            message: "busy".to_string(),
        })
        .await;
    }

    /// A gapless switch landed: the source says `x`, playback still has `a`.
    #[tokio::test]
    async fn a_preview_is_refused_when_the_source_speaks_of_another_track() {
        assert_preview_refused_when_source_says(loaded("x", StreamPlaybackState::Playing)).await;
    }

    #[tokio::test]
    async fn play_queue_while_previewing_is_refused_even_with_an_empty_queue() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;

        let response = harness.server.handle_command(Command::PlayQueue).await;

        let Response::Err { message } = response else {
            panic!("expected a refusal, got {response:?}");
        };
        assert_eq!(message, "Previewing: go back first");
    }

    #[tokio::test]
    async fn play_queue_with_an_empty_queue_says_so() {
        let harness = make_harness(source_playing_a);

        let response = harness.server.handle_command(Command::PlayQueue).await;

        let Response::Err { message } = response else {
            panic!("expected an error, got {response:?}");
        };
        assert_eq!(message, "Queue is empty");
    }

    #[tokio::test]
    async fn play_queue_on_a_paused_track_resumes_it_and_keeps_the_queue() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.state.lock().await.pause();
        harness.server.queue.lock().await.push_back(entry("c"));

        assert_ok(harness.server.handle_command(Command::PlayQueue).await);

        assert_eq!(queued(&harness.server).await, vec![full("c")]);
        assert_eq!(harness.sent(), vec![StreamCommand::Play]);
    }

    #[tokio::test]
    async fn play_resumes_a_paused_preview() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;
        harness.server.state.lock().await.pause();
        let before = harness.sent().len();

        assert_ok(
            harness
                .server
                .handle_command(Command::Play {
                    deck: media_protocol::Deck::A,
                })
                .await,
        );

        assert_eq!(harness.sent()[before..], [StreamCommand::Play]);
    }

    #[tokio::test]
    async fn end_preview_says_which_track_is_audible_afterwards() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;

        let response = harness.server.handle_command(end_preview_command()).await;

        assert!(matches!(
            response,
            Response::Ok { data: Some(ResponseData::NowPlaying(Some(now))) } if now == full("a")
        ));
    }

    #[tokio::test]
    async fn back_to_nothing_says_nothing_is_audible() {
        let harness = make_harness(source_playing_a);
        assert_ok(harness.server.handle_command(preview_command("b")).await);

        let response = harness.server.handle_command(end_preview_command()).await;

        assert!(matches!(
            response,
            Response::Ok {
                data: Some(ResponseData::NowPlaying(None))
            }
        ));
    }

    #[tokio::test]
    async fn back_loads_the_held_track_seeks_to_where_it_was_and_plays() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;
        let before = harness.sent().len();

        assert_ok(harness.server.handle_command(end_preview_command()).await);

        assert_eq!(
            harness.sent()[before..],
            [
                load_of("a"),
                StreamCommand::Seek {
                    position_ms: PositionMs::new(90_000)
                },
                StreamCommand::Play
            ]
        );
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
    }

    #[tokio::test]
    async fn back_to_a_paused_track_does_not_play() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.state.lock().await.pause();
        assert_ok(harness.server.handle_command(preview_command("b")).await);
        let before = harness.sent().len();

        assert_ok(harness.server.handle_command(end_preview_command()).await);

        assert_eq!(
            harness.sent()[before..],
            [
                load_of("a"),
                StreamCommand::Seek {
                    position_ms: PositionMs::new(90_000)
                }
            ]
        );
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Paused { hash: h("a") }
        );
    }

    #[tokio::test]
    async fn end_preview_with_no_preview_is_ok_and_asks_no_one() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;

        assert_ok(harness.server.handle_command(end_preview_command()).await);

        assert!(harness.sent().is_empty());
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
    }

    #[tokio::test]
    async fn skip_and_play_queue_are_refused_while_previewing_and_the_queue_is_kept() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;
        harness.server.queue.lock().await.push_back(entry("c"));
        let before = harness.sent().len();

        for command in [Command::Skip, Command::PlayQueue] {
            let Response::Err { message } = harness.server.handle_command(command).await else {
                panic!("expected a refusal");
            };
            assert_eq!(message, "Previewing: go back first");
        }

        assert_eq!(queued(&harness.server).await, vec![full("c")]);
        assert_eq!(harness.sent().len(), before, "the source is not touched");
        assert!(matches!(
            harness.server.state.lock().await.state(),
            PlaybackState::Previewing { hash, .. } if *hash == h("b")
        ));
    }

    #[tokio::test]
    async fn queue_edits_are_allowed_while_previewing() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;

        assert_ok(
            harness
                .server
                .handle_command(Command::QueueAppend {
                    hash: full("c"),
                    source: SourceName::audio(),
                })
                .await,
        );

        assert_eq!(queued(&harness.server).await, vec![full("c")]);
    }

    #[tokio::test]
    async fn stop_while_previewing_ends_everything() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;

        let response = harness
            .server
            .handle_command(Command::Stop {
                deck: media_protocol::Deck::A,
            })
            .await;

        assert_ok(response);
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Idle
        );
        assert_eq!(harness.sent().last(), Some(&StreamCommand::Stop));
    }

    #[tokio::test]
    async fn pause_resume_and_seek_act_on_the_preview() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;
        let before = harness.sent().len();

        assert_ok(
            harness
                .server
                .handle_command(Command::Pause {
                    deck: media_protocol::Deck::A,
                })
                .await,
        );
        let seeked = transport_of(harness.server.handle_command(seek_to(5_000)).await);
        assert_ok(
            harness
                .server
                .handle_command(Command::Resume {
                    deck: media_protocol::Deck::A,
                })
                .await,
        );

        let sent = harness.sent();
        assert_eq!(sent[before], StreamCommand::Pause);
        assert_eq!(
            sent[before + 1],
            StreamCommand::Seek {
                position_ms: PositionMs::new(5_000)
            }
        );
        assert_eq!(sent.last(), Some(&StreamCommand::Play));
        // The seek answers about the preview, not the held track.
        assert!(matches!(
            seeked,
            TransportStatus::Loaded { transport: Transport::Paused, now, .. } if now == full("b")
        ));
        assert!(matches!(
            harness.server.state.lock().await.state(),
            PlaybackState::Previewing { transport: Transport::Playing, back_to, .. }
                if *back_to == held_a_at_1_30(Transport::Playing)
        ));
    }

    #[tokio::test]
    async fn get_transport_shows_the_preview_and_where_it_goes_back_to() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;

        let status = transport_of(harness.server.handle_command(get_transport_command()).await);

        assert_eq!(
            status,
            TransportStatus::Loaded {
                transport: Transport::Playing,
                now: full("b"),
                position_ms: Some(PositionMs::new(90_000)),
                duration_ms: Some(PositionMs::new(240_000)),
                preview: Some(media_protocol::PreviewStatus {
                    back_to: Some(Held {
                        hash: full("a"),
                        at: PositionMs::new(90_000),
                        transport: Transport::Playing,
                    }),
                }),
            }
        );
    }

    #[tokio::test]
    async fn get_transport_of_a_preview_over_nothing_goes_back_to_nothing() {
        let harness = make_harness(source_playing_a);
        assert_ok(harness.server.handle_command(preview_command("b")).await);

        let status = transport_of(harness.server.handle_command(get_transport_command()).await);

        assert!(matches!(
            status,
            TransportStatus::Loaded {
                preview: Some(media_protocol::PreviewStatus { back_to: None }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn now_playing_names_the_preview_while_it_runs() {
        let harness = make_harness(source_playing_a);
        previewing_b_over_a(&harness).await;

        let response = harness.server.handle_command(Command::NowPlaying).await;

        assert!(matches!(
            response,
            Response::Ok { data: Some(ResponseData::NowPlaying(Some(now))) } if now == full("b")
        ));
    }

    /// The source dropped its preload when it loaded the preview, and the
    /// poll loop must neither send one nor believe it still has one.
    #[tokio::test]
    async fn no_preload_while_previewing_and_it_comes_back_after_the_pop() {
        let harness = make_harness(source_playing_a);
        start(&harness.server, "a").await;
        harness.server.queue.lock().await.push_back(entry("c"));
        let task = harness.server.poll_task();
        let mut poll = PollState::default();
        poll_once(&task, &mut poll).await;
        assert_eq!(harness.sent_preloads(), vec![full("c")]);

        assert_ok(harness.server.handle_command(preview_command("b")).await);
        for _ in 0..3 {
            poll_once(&task, &mut poll).await;
        }
        assert_eq!(
            harness.sent_preloads(),
            vec![full("c")],
            "nothing is preloaded while previewing"
        );

        assert_ok(harness.server.handle_command(end_preview_command()).await);
        poll_once(&task, &mut poll).await;
        assert_eq!(harness.sent_preloads(), vec![full("c"), full("c")]);
    }

    #[tokio::test]
    async fn position_updates_name_the_preview_while_it_plays() {
        let harness = make_harness(source_playing_a);
        let events = harness.subscribe();
        previewing_b_over_a(&harness).await;
        let task = harness.server.poll_task();
        let mut poll = PollState::default();

        for _ in 0..5 {
            poll_once(&task, &mut poll).await;
        }

        let mut seen = Vec::new();
        while let Ok(msg) = events.recv() {
            let (_, event) = event_protocol::from_topic_message(msg.as_slice()).unwrap();
            if let PlaybackEvent::PositionUpdate { hash, .. } = event {
                assert_eq!(hash, full("b"));
                return;
            }
            seen.push(event);
        }
        panic!("no PositionUpdate, saw {seen:?}");
    }

    /// A source that reports `a` playing until it is told to load anything,
    /// and from then on `b` as finished, whatever it loaded: a preview of `b`
    /// that ended, and a source that is late to report what is audible.
    fn source_whose_preview_b_finishes(
    ) -> impl Fn(&StreamCommand) -> StreamResponse + Send + 'static {
        let loaded_any = std::sync::atomic::AtomicBool::new(false);
        move |command| match command {
            StreamCommand::Load { .. } => {
                loaded_any.store(true, Ordering::SeqCst);
                StreamResponse::Ok
            }
            StreamCommand::Loaded if loaded_any.load(Ordering::SeqCst) => {
                loaded("b", StreamPlaybackState::Finished)
            }
            other => source_playing_a(other),
        }
    }

    /// A late `Finished` for the preview, after Back, is not the end of the
    /// restored track.
    #[tokio::test]
    async fn a_late_finished_for_the_old_preview_does_not_end_the_restored_track() {
        let harness = make_harness(source_whose_preview_b_finishes());
        start(&harness.server, "a").await;
        harness.server.queue.lock().await.push_back(entry("c"));
        assert_ok(harness.server.handle_command(preview_command("b")).await);
        let task = harness.server.poll_task();
        let mut poll = PollState::default();
        poll_once(&task, &mut poll).await;
        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
        let before = harness.sent().len();

        // The source still says `b` finished.
        poll_once(&task, &mut poll).await;

        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
        assert_eq!(queued(&harness.server).await, vec![full("c")]);
        assert!(
            harness.sent()[before..]
                .iter()
                .all(|c| !matches!(c, StreamCommand::Load { .. })),
            "nothing is loaded over the restored track"
        );
    }

    /// The preview played to its end: the poll loop sends playback back by
    /// itself, and the queue is not advanced.
    #[tokio::test]
    async fn a_preview_that_finishes_goes_back_and_does_not_pop_the_queue() {
        let harness = make_harness(source_whose_preview_b_finishes());
        let events = harness.subscribe();
        start(&harness.server, "a").await;
        harness.server.queue.lock().await.push_back(entry("c"));
        assert_ok(harness.server.handle_command(preview_command("b")).await);
        let before = harness.sent().len();
        let task = harness.server.poll_task();
        let mut poll = PollState::default();

        poll_once(&task, &mut poll).await;

        assert_eq!(
            harness.server.state.lock().await.state(),
            &PlaybackState::Playing { hash: h("a") }
        );
        assert_eq!(
            harness.sent()[before..]
                .iter()
                .filter(|c| !matches!(c, StreamCommand::Loaded))
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                load_of("a"),
                StreamCommand::Seek {
                    position_ms: PositionMs::new(90_000)
                },
                StreamCommand::Play
            ]
        );
        assert_eq!(queued(&harness.server).await, vec![full("c")]);
        let mut seen = Vec::new();
        while let Ok(msg) = events.recv() {
            let (_, event) = event_protocol::from_topic_message(msg.as_slice()).unwrap();
            if let PlaybackEvent::PreviewEnded { hash, resumed } = event {
                assert_eq!((hash, resumed), (full("b"), Some(full("a"))));
                return;
            }
            seen.push(event);
        }
        panic!("no PreviewEnded, saw {seen:?}");
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
