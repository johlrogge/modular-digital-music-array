//! Player wire types for the new web UI.
//!
//! `ApiPlayer` is the shape of `GET /api/player` and the `{queue}` every `/api/queue/*`
//! mutation returns. `static/js/core.js` reads exactly these keys. It is built from the playback
//! service's `GetTransport`, so what is playing, paused or previewed is the node's word, never a
//! guess from events.
//!
//! Every playback command sent here targets deck A (the console has one) or is queue-level,
//! which the protocol routes to deck A and which carries no `deck` field yet (ADR-017 phase 1).

use super::library::playlist_name;
use super::{
    error_reply, gw, library, library_failed, parse_hashes, playback_failed,
    playback_failure_status, queue_entries, resolve_for_playback, resolve_for_queue_replace,
    respond, send, PlaybackFailure, Reply,
};
use crate::play_now::{
    queue_last_commands, queue_next_commands, require_idle, start_command, Busy,
};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Json, Response},
};
use gateway_client::{Command, GatewayClient, LibraryRequest};
use library_ipc_client::{
    entries_for_playback, interpret_playlist_entries, CanonicalHash, PlayableEntries, PlaylistName,
};
use mdma_client::{
    seek_to, skip_beats, LibraryBackend, NodeError, Placement, PlaybackBackend, SeekError,
};
use media_protocol::{
    ContentHash, Deck, OutputHealth, PositionMs, PreviewStatus, ResponseData, Transport,
    TransportStatus,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Transport state. `idle` means nothing is loaded; paused counts as busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerState {
    Idle,
    Playing,
    Paused,
}

impl From<Transport> for PlayerState {
    fn from(transport: Transport) -> Self {
        match transport {
            Transport::Playing => PlayerState::Playing,
            Transport::Paused => PlayerState::Paused,
        }
    }
}

/// What a running preview goes back to. All three are null when the preview started over nothing
/// (going back stops playback); otherwise the held track, where in it, and whether it played.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiPreview {
    pub back_to: Option<String>,
    pub back_at_ms: Option<u64>,
    pub back_transport: Option<PlayerState>,
}

impl From<PreviewStatus> for ApiPreview {
    fn from(preview: PreviewStatus) -> Self {
        match preview.back_to {
            Some(held) => Self {
                back_to: Some(held.hash.as_str().to_owned()),
                back_at_ms: Some(held.at.as_millis()),
                back_transport: Some(held.transport.into()),
            },
            None => Self {
                back_to: None,
                back_at_ms: None,
                back_transport: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiPlayer {
    pub state: PlayerState,
    /// Hash of the audible track (the preview, while one runs), or null when idle.
    pub now: Option<String>,
    /// Hashes in Up next, next track first.
    pub queue: Vec<String>,
    /// Audio output health (`absent|starting|running|stalled`), or null when the node cannot
    /// say (an old node). Reported next to the transport state, never folded into it (ADR-028).
    pub output: Option<OutputHealth>,
    /// Where the audible track is, or null when idle or the source cannot say.
    pub position_ms: Option<u64>,
    /// How long the audible track is, or null when idle or the source cannot say.
    pub duration_ms: Option<u64>,
    /// Null unless the audible track is a preview.
    pub preview: Option<ApiPreview>,
}

/// What one read of the player found.
struct Snapshot {
    transport: TransportStatus,
    queue: Vec<ContentHash>,
    output: Option<OutputHealth>,
}

/// The audio output health. Never fails: an old node, an old gateway or an answer that is not
/// an `OutputStatus` all read as unknown (null), so the player itself never turns into a 500.
fn output_of(client: &GatewayClient) -> Option<OutputHealth> {
    match send(client, &Command::GetOutputStatus) {
        Ok(Some(ResponseData::OutputStatus(health))) => health,
        _ => None,
    }
}

/// The loaded track. An answer that is not a `NowPlaying` is an error, never "idle": the idle
/// guard must not pass on a reply it does not understand.
fn now_of(client: &GatewayClient) -> Result<Option<ContentHash>, PlaybackFailure> {
    match send(client, &Command::NowPlaying)? {
        Some(ResponseData::NowPlaying(now)) => Ok(now),
        other => Err(PlaybackFailure::Unexpected(format!(
            "Unexpected NowPlaying response: {other:?}"
        ))),
    }
}

/// The transport of deck A. An answer that is not a `Transport` (an old node) is an error, never
/// "idle".
fn transport_of(client: &GatewayClient) -> Result<TransportStatus, PlaybackFailure> {
    match send(client, &Command::GetTransport { deck: Deck::A })? {
        Some(ResponseData::Transport(transport)) => Ok(transport),
        other => Err(PlaybackFailure::Unexpected(format!(
            "Unexpected GetTransport response: {other:?}"
        ))),
    }
}

pub fn queue_of(client: &GatewayClient) -> Result<Vec<ContentHash>, PlaybackFailure> {
    match send(client, &Command::QueueList)? {
        Some(ResponseData::Queue(queue)) => Ok(queue),
        other => Err(PlaybackFailure::Unexpected(format!(
            "Unexpected QueueList response: {other:?}"
        ))),
    }
}

fn strings(hashes: &[ContentHash]) -> Vec<String> {
    hashes.iter().map(|h| h.as_str().to_owned()).collect()
}

/// Send `commands` in order, stopping at the first failure. Returns how many succeeded and the
/// failure, if any.
fn send_all(client: &GatewayClient, commands: &[Command]) -> (usize, Option<PlaybackFailure>) {
    for (done, command) in commands.iter().enumerate() {
        if let Err(failure) = send(client, command) {
            return (done, Some(failure));
        }
    }
    (commands.len(), None)
}

/// Report `queued` insertions and the queue as it now is. No failure: 200 `{queued, queue}`.
/// Some done: 207 `{queued, error, queue}`. None done: the failure's status
/// ([`playback_failure_status`]) with `{error, queued: 0}`.
fn batch_reply(client: &GatewayClient, queued: usize, failure: Option<PlaybackFailure>) -> Reply {
    match failure {
        None => match queue_of(client) {
            Ok(queue) => (
                StatusCode::OK,
                serde_json::json!({ "queued": queued, "queue": strings(&queue) }),
            ),
            Err(failure) => playback_failed(&failure),
        },
        Some(failure) if queued == 0 => (
            playback_failure_status(&failure),
            serde_json::json!({ "error": failure.to_string(), "queued": 0 }),
        ),
        Some(failure) => {
            let queue = queue_of(client).map(|q| strings(&q)).unwrap_or_default();
            (
                StatusCode::MULTI_STATUS,
                serde_json::json!({
                    "queued": queued,
                    "error": failure.to_string(),
                    "queue": queue
                }),
            )
        }
    }
}

/// Send `commands` and report the outcome (see [`batch_reply`]).
fn run_batch(client: &GatewayClient, commands: &[Command]) -> Reply {
    let (done, failure) = send_all(client, commands);
    batch_reply(client, done, failure)
}

/// Everything was queued but the track could not be started: the failure's status
/// ([`playback_failure_status`]) with `{error, queued, stage: "start", queue}`.
fn start_failed_reply(client: &GatewayClient, queued: usize, failure: PlaybackFailure) -> Reply {
    let queue = queue_of(client).map(|q| strings(&q)).unwrap_or_default();
    (
        playback_failure_status(&failure),
        serde_json::json!({
            "error": failure.to_string(),
            "queued": queued,
            "stage": "start",
            "queue": queue
        }),
    )
}

/// The player as `GET /api/player` reports it.
fn api_player(snapshot: Snapshot) -> ApiPlayer {
    let queue = strings(&snapshot.queue);
    let output = snapshot.output;
    match snapshot.transport {
        TransportStatus::Idle => ApiPlayer {
            state: PlayerState::Idle,
            now: None,
            queue,
            output,
            position_ms: None,
            duration_ms: None,
            preview: None,
        },
        TransportStatus::Loaded {
            transport,
            now,
            position_ms,
            duration_ms,
            preview,
        } => ApiPlayer {
            state: transport.into(),
            now: Some(now.as_str().to_owned()),
            queue,
            output,
            position_ms: position_ms.map(|p| p.as_millis()),
            duration_ms: duration_ms.map(|p| p.as_millis()),
            preview: preview.map(ApiPreview::from),
        },
    }
}

fn snapshot(client: &GatewayClient) -> Result<Snapshot, PlaybackFailure> {
    Ok(Snapshot {
        transport: transport_of(client)?,
        queue: queue_of(client)?,
        output: output_of(client),
    })
}

/// The reply of `GET /api/player`: `{state, now, queue, output, position_ms, duration_ms,
/// preview}`, the one answer every transport route ends with.
fn player_reply(client: &GatewayClient) -> Reply {
    match snapshot(client) {
        Ok(snap) => (
            StatusCode::OK,
            serde_json::to_value(api_player(snap)).unwrap_or_default(),
        ),
        Err(failure) => playback_failed(&failure),
    }
}

/// Send `command`, then answer like `GET /api/player`. A command that fails answers with
/// `failed`.
fn then_player(
    client: &GatewayClient,
    command: &Command,
    failed: fn(&PlaybackFailure) -> Reply,
) -> Reply {
    match send(client, command) {
        Ok(_) => player_reply(client),
        Err(failure) => failed(&failure),
    }
}

/// `command` to deck A, answered like `GET /api/player`; a failure by the transport policy
/// ([`transport_failed`]).
async fn deck_a(state: &Arc<AppState>, command: Command) -> Response {
    respond(
        gw(state, move |client| {
            then_player(client, &command, transport_failed)
        })
        .await,
    )
}

/// `GET /api/player`.
pub async fn player(State(state): State<Arc<AppState>>) -> Response {
    respond(gw(&state, player_reply).await)
}

/// `POST /api/pause`: pause deck A (the preview, while one runs), then answer like
/// `GET /api/player`.
pub async fn pause(State(state): State<Arc<AppState>>) -> Response {
    deck_a(&state, Command::Pause { deck: Deck::A }).await
}

/// `POST /api/resume`: resume deck A (the preview, while one runs), then answer like
/// `GET /api/player`.
pub async fn resume(State(state): State<Arc<AppState>>) -> Response {
    deck_a(&state, Command::Resume { deck: Deck::A }).await
}

#[derive(Debug, Deserialize)]
pub struct PreviewRequest {
    hash: Option<String>,
}

/// The one policy of every transport route (pause, resume, preview, back, seek): a refusal by
/// playback is a 409 with its own words (it said no to this state of the player); a gateway that
/// does not know the command or an answer of the wrong kind is a 502, an unreachable node a 503
/// ([`playback_failure_status`]).
fn transport_failed(failure: &PlaybackFailure) -> Reply {
    match failure {
        PlaybackFailure::Refused(message) => error_reply(StatusCode::CONFLICT, message.clone()),
        other => playback_failed(other),
    }
}

/// `POST /api/preview {hash}`: play the track at once on deck A, holding what is audible to go
/// back to (ADR-031), and answer like `GET /api/player`. The hash is resolved like every hash
/// bound for playback.
/// Replies: 400 without a hash, 422 for a hash the library does not know (nothing is sent), 409
/// with playback's words when it refuses (for example when the source does not say where the
/// playing track is), 502/503 per [`playback_failure_status`].
pub async fn preview(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PreviewRequest>,
) -> Response {
    let hashes = match parse_hashes(req.hash.into_iter().collect()) {
        Ok(h) => h,
        Err(response) => return response,
    };
    let resolver = Arc::clone(&state);
    respond(
        gw(&state, move |client| {
            match resolve_for_playback(&resolver, &hashes) {
                Ok(mut resolved) => {
                    let command = Command::Preview {
                        deck: Deck::A,
                        hash: resolved.remove(0).into(),
                    };
                    then_player(client, &command, transport_failed)
                }
                Err(reply) => reply,
            }
        })
        .await,
    )
}

/// `POST /api/preview/back`: end the preview and go back to what it held (a no-op when none
/// runs), then answer like `GET /api/player`.
pub async fn preview_back(State(state): State<Arc<AppState>>) -> Response {
    deck_a(&state, Command::EndPreview { deck: Deck::A }).await
}

/// How a requested time is placed (see [`Placement`]); the rule is the node's, the browser only
/// says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeekPlacement {
    Snap,
    Free,
}

impl From<SeekPlacement> for Placement {
    fn from(placement: SeekPlacement) -> Self {
        match placement {
            SeekPlacement::Snap => Placement::Snap,
            SeekPlacement::Free => Placement::Free,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeekTo {
    to_ms: u64,
    placement: SeekPlacement,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeekBy {
    by_beats: i32,
}

/// Where to seek: to a time (placed as asked), or by beats (negative goes back).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SeekRequest {
    To(SeekTo),
    By(SeekBy),
}

/// The reply for a seek that could not be made.
fn seek_failed(error: SeekError) -> Reply {
    match error {
        SeekError::NothingLoaded | SeekError::NoPosition => {
            error_reply(StatusCode::CONFLICT, error.to_string())
        }
        SeekError::NoBeatGrid => error_reply(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
        SeekError::Playback(e) => transport_failed(&e.into()),
        SeekError::Library(e) => library_failed(&e),
        SeekError::Node(e) => error_reply(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    }
}

/// `POST /api/seek {to_ms, placement: "snap"|"free"}` or `{by_beats}`: move the audible track of
/// deck A (the preview, while one runs) and answer like `GET /api/player`. Snapping and beat
/// steps are mdma_client's rules (`seek_to`, `skip_beats`); playing stays playing and paused
/// stays paused.
/// Replies: 409 when nothing is loaded (or playback refuses), 422 for a beat step on a track with
/// no beat grid or a body that is neither form, 502/503 per the library's and playback's policy.
pub async fn seek(State(state): State<Arc<AppState>>, Json(req): Json<SeekRequest>) -> Response {
    let gateway = state.gateway.clone();
    respond(
        gw(&state, move |client| {
            let playback = PlaybackBackend::Gateway(client.clone());
            let library = || {
                LibraryBackend::connect_gateway(&gateway)
                    .map_err(|e| NodeError::Connect(e.to_string()))
            };
            let moved = match req {
                SeekRequest::To(SeekTo { to_ms, placement }) => seek_to(
                    &playback,
                    library,
                    Deck::A,
                    PositionMs::new(to_ms),
                    placement.into(),
                ),
                SeekRequest::By(SeekBy { by_beats }) => {
                    skip_beats(&playback, library, Deck::A, by_beats)
                }
            };
            match moved {
                Ok(_) => player_reply(client),
                Err(error) => seek_failed(error),
            }
        })
        .await,
    )
}

fn busy_reply() -> Reply {
    error_reply(StatusCode::CONFLICT, "busy")
}

/// The full hashes of the tracks a queue request names: never empty, so no queue action can run
/// on nothing (an empty set never changes Up next).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSet(Vec<CanonicalHash>);

impl TrackSet {
    /// The set of `hashes`, or `None` when there are none.
    pub fn new(hashes: Vec<CanonicalHash>) -> Option<Self> {
        (!hashes.is_empty()).then_some(Self(hashes))
    }

    pub fn as_slice(&self) -> &[CanonicalHash] {
        &self.0
    }
}

/// The tracks a queue request names, resolved, or the reply that says why not.
type Resolved = Result<TrackSet, Reply>;

/// The set of `hashes`, which resolution made from a non-empty list ([`parse_hashes`], the one
/// 400 for an empty request, which comes before any player check): one full hash per hash asked
/// for, so it cannot be empty.
fn resolved_set(hashes: Vec<CanonicalHash>) -> TrackSet {
    debug_assert!(!hashes.is_empty());
    TrackSet(hashes)
}

/// Run `then` on the resolved set, or answer with why it did not resolve (nothing sent).
fn resolved(resolve: impl FnOnce() -> Resolved, then: impl FnOnce(&TrackSet) -> Reply) -> Reply {
    match resolve() {
        Ok(set) => then(&set),
        Err(reply) => reply,
    }
}

/// `reply` with `key` set to `value` in its body (every reply body is an object).
fn with_field((status, mut body): Reply, key: &str, value: impl serde::Serialize) -> Reply {
    body[key] = serde_json::to_value(value).unwrap_or_default();
    (status, body)
}

/// Run `then`, which changes Up next, and add `previous` to its reply: Up next as it was just
/// before (read here, so it is the queue `then` replaced, not what the browser last saw). It is
/// what Undo puts back. Between the read and the change the queue can still move: a change by
/// someone else in that window is not in `previous` (ADR-027, amendment 2026-11-17).
fn changing(client: &GatewayClient, then: impl FnOnce() -> Reply) -> Reply {
    match queue_of(client) {
        Ok(previous) => with_field(then(), "previous", strings(&previous)),
        Err(failure) => playback_failed(&failure),
    }
}

/// `QueueReplace` with `hashes`: one atomic command, so a refusal leaves Up next as it was. What
/// plays is not touched: the playback service replaces its queue only, and nothing here stops or
/// starts a track.
fn replace_reply(client: &GatewayClient, set: &TrackSet) -> Reply {
    let command = Command::QueueReplace {
        entries: queue_entries(set.as_slice().to_vec()),
    };
    match send(client, &command) {
        Ok(_) => batch_reply(client, set.as_slice().len(), None),
        Err(failure) => batch_reply(client, 0, Some(failure)),
    }
}

/// Queue the set at the front, then start it, but only while nothing is loaded; the guard is
/// checked before resolving and queueing and again before `PlayQueue` (something may have
/// started in between). See [`queue`] for the replies.
fn play_now_reply(client: &GatewayClient, resolve: impl FnOnce() -> Resolved) -> Reply {
    let now = match now_of(client) {
        Ok(now) => now,
        Err(failure) => return playback_failed(&failure),
    };
    if require_idle(now).is_err() {
        return busy_reply();
    }
    // No `previous`: an Undo could put the queue back but not stop the track this starts.
    resolved(resolve, |set| {
        let (queued, failure) = send_all(client, &queue_next_commands(set.as_slice()));
        if failure.is_some() {
            return batch_reply(client, queued, failure);
        }
        let again = match now_of(client) {
            Ok(now) => now,
            Err(failure) => return start_failed_reply(client, queued, failure),
        };
        match require_idle(again) {
            Ok(idle) => match send(client, &start_command(idle)) {
                Ok(_) => batch_reply(client, queued, None),
                Err(failure) => start_failed_reply(client, queued, failure),
            },
            Err(Busy::Loaded(_)) => {
                let queue = queue_of(client).map(|q| strings(&q)).unwrap_or_default();
                (
                    StatusCode::CONFLICT,
                    serde_json::json!({ "error": "busy", "queued": queued, "queue": queue }),
                )
            }
        }
    })
}

/// What a queue request does with the tracks it names: the one list of queue actions, whether
/// the tracks come as hashes or as a playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueAction {
    /// The set goes to the front of Up next, in the order given.
    Next,
    /// The set goes to the end of Up next, in the order given.
    Last,
    /// The set becomes the whole of Up next; what plays keeps playing.
    Replace,
    /// The set goes to the front and starts, but only while nothing is loaded.
    Now,
}

impl QueueAction {
    /// Do the action to the tracks `resolve` gives, over `client`. A set that does not resolve is
    /// answered, and nothing is sent. The set is never empty ([`TrackSet`]), so Up next is never
    /// changed by nothing. A change that can be undone (not `now`) answers with `previous`
    /// ([`changing`]).
    fn run(self, client: &GatewayClient, resolve: impl FnOnce() -> Resolved) -> Reply {
        match self {
            QueueAction::Next => resolved(resolve, |set| {
                changing(client, || {
                    run_batch(client, &queue_next_commands(set.as_slice()))
                })
            }),
            QueueAction::Last => resolved(resolve, |set| {
                changing(client, || {
                    run_batch(client, &queue_last_commands(set.as_slice()))
                })
            }),
            QueueAction::Replace => resolved(resolve, |set| {
                changing(client, || replace_reply(client, set))
            }),
            QueueAction::Now => play_now_reply(client, resolve),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct QueueRequest {
    action: QueueAction,
    hashes: Vec<String>,
}

/// `POST /api/queue {action: "next"|"last"|"replace"|"now", hashes}`: the one route of every queue
/// action on a list of tracks. Every hash is resolved against the library's add set (any track the
/// library has, ADR-027 amendment 2026-11-17) before anything is sent.
/// - next: the set goes to the front of Up next, in the order given;
/// - last: the set goes to the end of Up next, in the order given;
/// - replace: the set becomes the whole of Up next, unconditionally (`/api/queue/edit` is the
///   guarded edit), in one atomic `QueueReplace`; the track that plays keeps playing;
/// - now: the set goes to the front and starts, but only while nothing is loaded; the guard is
///   checked before resolving and queueing and again before `PlayQueue` (something may have
///   started in between).
///
/// Replies: 200 `{queued, queue}`, with `previous` (Up next as it was just before, for Undo) for
/// next, last and replace, and none for `now`: an Undo cannot stop the track it started; 207
/// `{queued, error, queue}` when queueing stopped partway; 409
/// `{error: "busy"}` (nothing sent) or `{error: "busy", queued, queue}` (became busy
/// after queueing, so not started); `{error, queued, stage: "start", queue}` with the
/// failure's status (502/503) when everything was queued but the start failed; the failure's
/// status (502/503) with `{error, queued: 0}` when playback refuses or cannot be reached; 422/503
/// when a hash can't be resolved (nothing sent); 400 without hashes; 422 for an unknown action.
/// A busy player answers `now` with 409 before any lookup.
pub async fn queue(State(state): State<Arc<AppState>>, Json(req): Json<QueueRequest>) -> Response {
    let hashes = match parse_hashes(req.hashes) {
        Ok(h) => h,
        Err(response) => return response,
    };
    let action = req.action;
    let resolver = Arc::clone(&state);
    respond(
        gw(&state, move |client| {
            action.run(client, || {
                resolve_for_playback(&resolver, &hashes).map(resolved_set)
            })
        })
        .await,
    )
}

#[derive(Debug, Deserialize)]
pub struct PlaylistQueueRequest {
    action: QueueAction,
}

/// 200 `{queued: 0, queue}`: there was nothing to queue, so Up next is as it was.
fn nothing_queued(client: &GatewayClient) -> Reply {
    batch_reply(client, 0, None)
}

/// The playlist `name` queued by `action` ([`entries_for_playback`], ADR-027 §10): its playable
/// entries get the action's treatment, as for a list of hashes; its unavailable entries are
/// skipped and reported (`skipped: [{position, error}]`, positions 0-based among the playlist's
/// entries; the browser adds 1 to show one), never failed on. With nothing playable nothing is
/// sent and the answer is 200 `{queued: 0, queue, skipped}`, whatever the action.
pub fn queue_playlist(client: &GatewayClient, name: &PlaylistName, action: QueueAction) -> Reply {
    let entries = match library(
        client,
        &LibraryRequest::PlaylistEntries { name: name.clone() },
    )
    .and_then(interpret_playlist_entries)
    {
        Ok(entries) => entries,
        Err(e) => return library_failed(&e),
    };
    let PlayableEntries { hashes, skipped } = entries_for_playback(&entries);
    let reply = match TrackSet::new(hashes) {
        Some(set) => action.run(client, || Ok(set)),
        None => nothing_queued(client),
    };
    with_field(reply, "skipped", skipped)
}

/// `POST /api/playlists/{name}/queue {action}`: [`queue_playlist`]. Replies are `POST /api/queue`'s
/// with `skipped` added. 404 for a missing playlist; 400 for a name no playlist can have (nothing
/// is sent); 422 for an unknown action.
pub async fn playlist_queue(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<PlaylistQueueRequest>,
) -> Response {
    queue_named_playlist(&state, &name, req.action).await
}

/// `action` on the playlist called `name` (from a path), over a blocking gateway connection:
/// what every route that queues a playlist answers with.
pub async fn queue_named_playlist(
    state: &Arc<AppState>,
    name: &str,
    action: QueueAction,
) -> Response {
    let name = match playlist_name(name) {
        Ok(n) => n,
        Err(response) => return response,
    };
    respond(gw(state, move |client| queue_playlist(client, &name, action)).await)
}

/// `POST /api/start`: start the queue, only when nothing is loaded; 409 otherwise.
pub async fn start(State(state): State<Arc<AppState>>) -> Response {
    respond(
        gw(&state, |client| {
            let now = match now_of(client) {
                Ok(now) => now,
                Err(failure) => return playback_failed(&failure),
            };
            match require_idle(now) {
                Ok(idle) => run_batch(client, &[start_command(idle)]),
                Err(Busy::Loaded(_)) => busy_reply(),
            }
        })
        .await,
    )
}

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    /// The queue the caller's change was computed from.
    expect: Vec<String>,
    /// The queue to install.
    queue: Vec<String>,
}

/// `POST /api/queue/edit {expect, queue}`: replace Up next with `queue` if it still equals
/// `expect`; otherwise 409 `{error, queue: current}` and nothing is touched. A change answers 200
/// `{queue, previous}` (the queue it replaced, for Undo); an edit that leaves the queue as it is,
/// once the hashes are resolved, sends nothing and answers `{queue}` with no `previous`.
pub async fn queue_edit(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EditRequest>,
) -> Response {
    let resolver = Arc::clone(&state);
    respond(
        gw(&state, move |client| {
            let queue = match queue_of(client) {
                Ok(q) => q,
                Err(failure) => return playback_failed(&failure),
            };
            let current = strings(&queue);
            if current != req.expect {
                return (
                    StatusCode::CONFLICT,
                    serde_json::json!({ "error": "stale", "queue": current }),
                );
            }
            let requested: Vec<ContentHash> = req.queue.into_iter().map(ContentHash::new).collect();
            let full = match resolve_for_queue_replace(&resolver, &queue, &requested) {
                Ok(full) => full,
                Err(reply) => return reply,
            };
            // Compared after resolution, so a short hash naming the same track is no change. An
            // edit that changes nothing writes nothing and has no `previous`: nothing to undo.
            let resolved: Vec<ContentHash> = full.iter().cloned().map(Into::into).collect();
            if resolved == queue {
                return (StatusCode::OK, serde_json::json!({ "queue": current }));
            }
            let entries = queue_entries(full);
            if let Err(failure) = send(client, &Command::QueueReplace { entries }) {
                return playback_failed(&failure);
            }
            match queue_of(client) {
                Ok(queue) => (
                    StatusCode::OK,
                    serde_json::json!({ "queue": strings(&queue), "previous": current }),
                ),
                Err(failure) => playback_failed(&failure),
            }
        })
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(json: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    #[test]
    fn player_json_has_exactly_the_documented_keys() {
        let p = ApiPlayer {
            state: PlayerState::Paused,
            now: Some("sha256:a".into()),
            queue: vec!["sha256:b".into()],
            output: Some(OutputHealth::Running),
            position_ms: Some(1500),
            duration_ms: Some(200_000),
            preview: Some(ApiPreview {
                back_to: Some("sha256:c".into()),
                back_at_ms: Some(151_000),
                back_transport: Some(PlayerState::Playing),
            }),
        };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            keys_of(&json),
            [
                "duration_ms",
                "now",
                "output",
                "position_ms",
                "preview",
                "queue",
                "state"
            ]
        );
        assert_eq!(json["state"], "paused");
        assert_eq!(json["queue"][0], "sha256:b");
        assert_eq!(json["output"], "running");
        assert_eq!(json["position_ms"], 1500);
        assert_eq!(
            keys_of(&json["preview"]),
            ["back_at_ms", "back_to", "back_transport"]
        );
        assert_eq!(json["preview"]["back_transport"], "playing");
    }

    #[test]
    fn a_preview_over_nothing_goes_back_to_nothing() {
        let status = PreviewStatus { back_to: None };
        let json = serde_json::to_value(ApiPreview::from(status)).unwrap();
        assert_eq!(
            json,
            json!({"back_to": null, "back_at_ms": null, "back_transport": null})
        );
    }

    #[test]
    fn idle_player_has_null_now_and_every_state_is_lowercase() {
        let p = ApiPlayer {
            state: PlayerState::Idle,
            now: None,
            queue: vec![],
            output: None,
            position_ms: None,
            duration_ms: None,
            preview: None,
        };
        let json = serde_json::to_value(&p).unwrap();
        assert!(json["now"].is_null());
        assert!(json["preview"].is_null());
        for (s, want) in [
            (PlayerState::Idle, "idle"),
            (PlayerState::Playing, "playing"),
            (PlayerState::Paused, "paused"),
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), want);
        }
    }

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    // ---- handlers, against a fake gateway ----

    use crate::test_support::{
        body_json, described, library_ok, playback_err, playback_ok, send_get, send_post_json,
        test_app_with, track_not_found, track_ok, FakeGateway,
    };
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{CanonicalHash, EntryError, PlaylistEntry};
    use media_protocol::{Held, PositionMs, Transport};
    use serde_json::json;

    fn track_entry(full: &str) -> PlaylistEntry {
        PlaylistEntry::Track {
            hash: CanonicalHash::try_from(ContentHash::new(full)).unwrap(),
        }
    }

    fn unresolved_entry(token: &str) -> PlaylistEntry {
        PlaylistEntry::Unresolved {
            error: EntryError::Unknown {
                hash: ContentHash::new(token),
            },
        }
    }

    // Playback holds only full hashes, so the handlers are given full ones.
    const A: &str = concat!(
        "sha256:",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    const B: &str = concat!(
        "sha256:",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    const C: &str = concat!(
        "sha256:",
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    );
    const X: &str = concat!(
        "sha256:",
        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
    );
    /// The fake library knows `A`, `B` and `C` (and `SHORT_A`), not `X`. A prefix it resolves to
    /// `A`:
    const SHORT_A: &str = "sha256:aaaa";

    #[test]
    fn the_test_hashes_are_canonical() {
        for h in [A, B, C, X, STALE_A] {
            assert!(
                library_ipc_client::CanonicalHash::try_from(ContentHash::new(h)).is_ok(),
                "{h}"
            );
        }
    }

    /// `now` audible at 1 s of a 200 s track, `transport` playing or paused, and `preview` if it
    /// is a preview.
    fn loaded(now: &str, transport: Transport, preview: Option<PreviewStatus>) -> TransportStatus {
        TransportStatus::Loaded {
            transport,
            now: hash(now),
            position_ms: Some(PositionMs::new(1_000)),
            duration_ms: Some(PositionMs::new(200_000)),
            preview,
        }
    }

    /// A playback service with `now` loaded and `queue` queued; everything else succeeds, except
    /// commands for which `fails` returns true. Its library knows `SHORT_A` (it is `A`) and
    /// nothing else; the output answers unknown.
    fn playback_script(
        now: Option<&'static str>,
        queue: &'static [&'static str],
        fails: fn(&Command) -> bool,
    ) -> FakeGateway {
        playback_script_with_output(now, queue, fails, None)
    }

    /// As `playback_script`, with `GetOutputStatus` answering `output`.
    fn playback_script_with_output(
        now: Option<&'static str>,
        queue: &'static [&'static str],
        fails: fn(&Command) -> bool,
        output: Option<OutputHealth>,
    ) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Playback { request } if fails(request) => playback_err("boom"),
            GatewayRequest::Playback {
                request: Command::NowPlaying,
            } => playback_ok(Some(ResponseData::NowPlaying(now.map(hash)))),
            GatewayRequest::Playback {
                request: Command::GetTransport { .. },
            } => playback_ok(Some(ResponseData::Transport(
                now.map_or(TransportStatus::Idle, |now| {
                    loaded(now, Transport::Playing, None)
                }),
            ))),
            GatewayRequest::Playback {
                request: Command::QueueList,
            } => playback_ok(Some(ResponseData::Queue(
                queue.iter().map(|h| hash(h)).collect(),
            ))),
            GatewayRequest::Playback {
                request: Command::GetOutputStatus,
            } => playback_ok(Some(ResponseData::OutputStatus(output))),
            GatewayRequest::Playback { .. } => playback_ok(None),
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistEntries { name },
            } => library_ok(match name.as_str() {
                "set" => LibraryResponse::PlaylistEntries(vec![
                    track_entry(A),
                    unresolved_entry("dead"),
                    track_entry(B),
                ]),
                "holes" => LibraryResponse::PlaylistEntries(vec![
                    unresolved_entry("dead"),
                    track_entry(A),
                    unresolved_entry("beef"),
                    track_entry(B),
                ]),
                "gone" => LibraryResponse::PlaylistEntries(vec![unresolved_entry("dead")]),
                other => {
                    LibraryResponse::Error(library_ipc_client::ProtocolError::PlaylistNotFound {
                        name: other.to_owned(),
                    })
                }
            }),
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } if hash.as_str() == SHORT_A => track_ok(A),
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } if [A, B, C].contains(&hash.as_str()) => track_ok(hash.as_str()),
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => track_not_found(hash.as_str()),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    fn never(_: &Command) -> bool {
        false
    }

    /// `action` ("next", "last", "replace" or "now") on `hashes`, through `POST /api/queue`.
    async fn queue_call(
        gw: &FakeGateway,
        action: &str,
        hashes: serde_json::Value,
    ) -> axum::response::Response {
        send_post_json(
            test_app_with(gw.addr()),
            "/api/queue",
            &json!({"action": action, "hashes": hashes}),
        )
        .await
    }

    #[tokio::test]
    async fn play_now_while_something_is_loaded_is_409_and_sends_nothing_else() {
        let gw = playback_script(Some(X), &[], never);
        let res = queue_call(&gw, "now", json!([A, B])).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await, json!({"error": "busy"}));
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn start_while_something_is_loaded_is_409_and_sends_nothing_else() {
        let gw = playback_script(Some(X), &[A], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/start", &json!({})).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await, json!({"error": "busy"}));
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn an_unintelligible_now_playing_answer_does_not_pass_the_guard() {
        let gw = FakeGateway::start(|_| playback_ok(None));
        let res = send_post_json(test_app_with(gw.addr()), "/api/start", &json!({})).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn play_now_when_idle_queues_the_set_in_order_then_plays() {
        let gw = playback_script(None, &[A, B, C], never);
        let res = queue_call(&gw, "now", json!([A, B, C])).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queue"], json!([A, B, C]));
        // An Undo cannot stop the track `now` starts, so there is nothing to undo to.
        assert!(body.get("previous").is_none());
        assert_eq!(body["queued"], 3);
        assert_eq!(
            described(&gw),
            [
                "now",
                &format!("next {C}"),
                &format!("next {B}"),
                &format!("next {A}"),
                "now",
                "play",
                "list"
            ]
        );
    }

    #[tokio::test]
    async fn play_now_that_becomes_busy_while_queueing_is_409_and_never_plays() {
        let mut nows = 0;
        let gw = FakeGateway::start(move |req| match req {
            GatewayRequest::Playback {
                request: Command::NowPlaying,
            } => {
                nows += 1;
                let now = (nows > 1).then(|| hash(X));
                playback_ok(Some(ResponseData::NowPlaying(now)))
            }
            GatewayRequest::Playback {
                request: Command::QueueList,
            } => playback_ok(Some(ResponseData::Queue(vec![hash(A)]))),
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => track_ok(hash.as_str()),
            _ => playback_ok(None),
        });
        let res = queue_call(&gw, "now", json!([A])).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(res).await,
            json!({"error": "busy", "queued": 1, "queue": [A]})
        );
        assert_eq!(described(&gw), ["now", &format!("next {A}"), "now", "list"]);
    }

    #[tokio::test]
    async fn play_now_whose_start_fails_says_so_with_what_was_queued() {
        let gw = playback_script(None, &[A], |c| matches!(c, Command::PlayQueue));
        let res = queue_call(&gw, "now", json!([A])).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            body_json(res).await,
            json!({"error": "boom", "queued": 1, "stage": "start", "queue": [A]})
        );
    }

    #[tokio::test]
    async fn pause_sends_pause_and_answers_with_what_the_transport_says() {
        let gw = playback_script(Some(X), &[A], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/pause", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        // The fake transport says playing whatever was sent: the console reports the node's
        // answer and keeps no memory of its own.
        assert_eq!(
            body_json(res).await,
            json!({
                "state": "playing", "now": X, "queue": [A], "output": null,
                "position_ms": 1000, "duration_ms": 200_000, "preview": null
            })
        );
        assert!(gw
            .commands()
            .iter()
            .any(|c| matches!(c, Command::Pause { deck: Deck::A })));
    }

    #[tokio::test]
    async fn resume_sends_resume() {
        let gw = playback_script(Some(X), &[], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/resume", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(gw
            .commands()
            .iter()
            .any(|c| matches!(c, Command::Resume { deck: Deck::A })));
    }

    #[tokio::test]
    async fn a_refused_pause_is_409_with_playbacks_words() {
        let gw = playback_script(Some(X), &[], |c| matches!(c, Command::Pause { .. }));
        let res = send_post_json(test_app_with(gw.addr()), "/api/pause", &json!({})).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await, json!({"error": "boom"}));
    }

    #[tokio::test]
    async fn an_unreadable_pause_answer_is_503() {
        let gw = FakeGateway::start_unreadable_when(
            |req| {
                matches!(
                    req,
                    GatewayRequest::Playback {
                        request: Command::Pause { .. }
                    }
                )
            },
            |_| playback_ok(None),
        );
        let res = send_post_json(test_app_with(gw.addr()), "/api/pause", &json!({})).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn start_when_idle_sends_play_queue() {
        let gw = playback_script(None, &[A], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/start", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(described(&gw), ["now", "play", "list"]);
    }

    #[tokio::test]
    async fn queue_next_sends_the_set_in_reverse() {
        let gw = playback_script(Some(X), &[A, B], never);
        let res = queue_call(&gw, "next", json!([A, B])).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queue"], json!([A, B]));
        assert_eq!(
            described(&gw),
            ["list", &format!("next {B}"), &format!("next {A}"), "list"]
        );
        // Every hash is resolved by the library, full ones too (no pass-through).
        let looked_up: Vec<String> = gw
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                GatewayRequest::Library {
                    request: LibraryRequest::GetTrack { hash },
                } => Some(hash.as_str().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(looked_up, [A, B]);
    }

    #[tokio::test]
    async fn a_full_hash_the_library_does_not_know_is_422_and_nothing_is_sent() {
        // X is a well-formed full hash, but the library has no such track.
        let gw = playback_script(Some(X), &[], never);
        let res = queue_call(&gw, "next", json!([A, X])).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"].as_str().unwrap().contains(X));
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn queue_next_resolves_a_short_hash_and_sends_the_full_one() {
        let gw = playback_script(Some(X), &[A], never);
        let res = queue_call(&gw, "next", json!([SHORT_A])).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(described(&gw), ["list", &format!("next {A}"), "list"]);
    }

    #[tokio::test]
    async fn every_action_resolves_short_hashes() {
        let gw = playback_script(None, &[A], never);
        for action in ["last", "now", "replace"] {
            let res = queue_call(&gw, action, json!([SHORT_A])).await;
            assert_eq!(res.status(), StatusCode::OK, "{action}");
        }
        let sent: Vec<String> = described(&gw)
            .into_iter()
            .filter(|c| {
                ["append", "next", "replace"]
                    .iter()
                    .any(|verb| c.starts_with(verb))
            })
            .collect();
        assert_eq!(
            sent,
            [
                format!("append {A}"),
                format!("next {A}"),
                format!("replace {A}")
            ]
        );
    }

    #[tokio::test]
    async fn an_unknown_hash_is_422_naming_it_and_nothing_is_sent() {
        // The unknown hash sits in the middle, so every route (including the reversed ones)
        // would have sent a resolved neighbour before reaching it if it did not resolve first.
        for (action, playback) in [
            ("next", vec![]),
            ("last", vec![]),
            ("replace", vec![]),
            // Play-now's idle guard is the only playback command.
            ("now", vec!["now"]),
        ] {
            let gw = playback_script(None, &[], never);
            let res = queue_call(&gw, action, json!([A, "sha256:dead", B])).await;
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{action}");
            let body = body_json(res).await;
            assert!(
                body["error"].as_str().unwrap().contains("sha256:dead"),
                "{action}: {body}"
            );
            assert_eq!(described(&gw), playback, "{action}");
        }
    }

    #[tokio::test]
    async fn a_library_failing_behind_the_gateway_is_502_on_an_api_route_and_nothing_is_sent() {
        // The gateway's "library unreachable" arrives as `Internal`: a 502 until it is typed
        // (#100), the one policy of `library_failure_status`.
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library { .. } => GatewayResponse::Error {
                message: "library unavailable".into(),
            },
            _ => playback_ok(None),
        });
        let res = queue_call(&gw, "next", json!([SHORT_A])).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_library_answer_is_503_on_an_api_route_and_nothing_is_sent() {
        let gw = FakeGateway::start_unreadable_when(
            |req| matches!(req, GatewayRequest::Library { .. }),
            |_| playback_ok(None),
        );
        let res = queue_call(&gw, "next", json!([SHORT_A])).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn play_now_on_a_busy_player_is_409_before_any_hash_is_resolved() {
        let gw = playback_script(Some(X), &[], never);
        let res = queue_call(&gw, "now", json!([SHORT_A])).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert!(gw
            .requests()
            .iter()
            .all(|r| !matches!(r, GatewayRequest::Library { .. })));
    }

    #[tokio::test]
    async fn queue_next_partial_failure_is_207_with_the_truth() {
        // Reverse order a, b, c is sent c, b, a; b fails, so only c got in.
        let gw = playback_script(
            None,
            &[C],
            |c| matches!(c, Command::QueueNext { hash, .. } if hash.as_str() == B),
        );
        let res = queue_call(&gw, "next", json!([A, B, C])).await;
        assert_eq!(res.status(), StatusCode::MULTI_STATUS);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 1);
        assert_eq!(body["error"], "boom");
        assert_eq!(body["queue"], json!([C]));
        assert_eq!(
            described(&gw),
            ["list", &format!("next {C}"), &format!("next {B}"), "list"]
        );
    }

    #[tokio::test]
    async fn queue_next_total_refusal_is_502() {
        let gw = playback_script(None, &[], |c| matches!(c, Command::QueueNext { .. }));
        let res = queue_call(&gw, "next", json!([A])).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(body_json(res).await["queued"], 0);
    }

    #[tokio::test]
    async fn queue_last_appends_in_order() {
        let gw = playback_script(Some(X), &[A, B], never);
        let res = queue_call(&gw, "last", json!([A, B])).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            described(&gw),
            [
                "list",
                &format!("append {A}"),
                &format!("append {B}"),
                "list"
            ]
        );
    }

    #[tokio::test]
    async fn an_empty_set_is_a_400_and_sends_nothing() {
        // Also on a busy player: the 400 comes before the 409 of `now`.
        for now in [None, Some(X)] {
            let gw = playback_script(now, &[], never);
            for action in ["next", "last", "replace", "now"] {
                let res = queue_call(&gw, action, json!([])).await;
                assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{action} {now:?}");
            }
            assert!(gw.commands().is_empty(), "{now:?}");
        }
    }

    #[tokio::test]
    async fn an_unknown_action_is_422_and_sends_nothing() {
        let gw = playback_script(None, &[], never);
        let res = queue_call(&gw, "explode", json!([A])).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn undo_gets_the_queue_read_just_before_the_change() {
        // The queue the console reads is the queue the change replaced, whatever the browser held.
        // (`now` has none: see `play_now_when_idle_queues_the_set_in_order_then_plays`.)
        for action in ["next", "last", "replace"] {
            let gw = playback_script(None, &[C], never);
            let res = queue_call(&gw, action, json!([A])).await;
            assert_eq!(res.status(), StatusCode::OK, "{action}");
            assert_eq!(body_json(res).await["previous"], json!([C]), "{action}");
            let sent = described(&gw);
            let first_change = sent
                .iter()
                .position(|c| {
                    ["next", "append", "replace"]
                        .iter()
                        .any(|v| c.starts_with(v))
                })
                .unwrap();
            assert_eq!(sent[first_change - 1], "list", "{action}: {sent:?}");
        }
    }

    #[tokio::test]
    async fn queue_edit_with_a_stale_expectation_is_409_and_replaces_nothing() {
        let gw = playback_script(Some(X), &[B], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [A, B], "queue": [B, A]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await["queue"], json!([B]));
        assert_eq!(described(&gw), ["list"]);
    }

    #[tokio::test]
    async fn queue_edit_with_a_matching_expectation_replaces_the_queue() {
        let gw = playback_script(Some(X), &[A, B], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [A, B], "queue": [B]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(described(&gw), ["list", &format!("replace {B}"), "list"]);
        // The fake playback lists the same scripted queue after the replace; `previous` is the
        // queue the edit replaced.
        assert_eq!(
            body_json(res).await,
            json!({"queue": [A, B], "previous": [A, B]})
        );
    }

    #[tokio::test]
    async fn an_identity_queue_edit_sends_no_replace_and_has_no_previous() {
        // The second case names the same track by a short hash: equal after resolution.
        for requested in [json!([A, B]), json!([SHORT_A, B])] {
            let gw = playback_script(Some(X), &[A, B], never);
            let res = send_post_json(
                test_app_with(gw.addr()),
                "/api/queue/edit",
                &json!({"expect": [A, B], "queue": requested.clone()}),
            )
            .await;
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(body_json(res).await, json!({"queue": [A, B]}));
            assert_eq!(described(&gw), ["list"], "{requested}");
        }
    }

    #[tokio::test]
    async fn queue_edit_resolves_a_short_hash() {
        let gw = playback_script(Some(X), &[B], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [B], "queue": [SHORT_A, B]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            described(&gw),
            ["list", &format!("replace {A},{B}"), "list"]
        );
    }

    #[tokio::test]
    async fn queue_edit_with_an_unknown_hash_is_422_and_replaces_nothing() {
        let gw = playback_script(Some(X), &[B], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [B], "queue": [B, "sha256:dead"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("sha256:dead"));
        assert_eq!(described(&gw), ["list"]);
    }

    /// The queue holds an entry the library does not know (`X`): it may stay, a new unknown one
    /// may not. The edit resolves against the live queue, not an empty one.
    #[tokio::test]
    async fn queue_edit_keeps_a_queued_entry_the_library_does_not_know() {
        let gw = playback_script(Some(A), &[X], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [X], "queue": [X, A]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            described(&gw),
            ["list", &format!("replace {X},{A}"), "list"]
        );
    }

    /// A queued entry that starts like `SHORT_A` but is another track than the library's `A`.
    const STALE_A: &str = concat!(
        "sha256:",
        "aaaabbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );

    #[tokio::test]
    async fn queue_edit_with_a_prefix_naming_different_tracks_in_queue_and_library_is_422() {
        let gw = playback_script(Some(X), &[STALE_A], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [STALE_A], "queue": [SHORT_A]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(described(&gw), ["list"]);
    }

    #[tokio::test]
    async fn queue_edit_whose_replace_playback_refuses_is_502() {
        let gw = playback_script(Some(X), &[A], |c| matches!(c, Command::QueueReplace { .. }));
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [A], "queue": [B]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(body_json(res).await, json!({"error": "boom"}));
    }

    #[tokio::test]
    async fn queue_edit_with_an_unreadable_queue_read_is_503_and_sends_nothing() {
        let gw = FakeGateway::start_unreadable_when(
            |req| {
                matches!(
                    req,
                    GatewayRequest::Playback {
                        request: Command::QueueList
                    }
                )
            },
            |_| playback_ok(None),
        );
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": [A], "queue": [B]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        // Only the read that could not be understood.
        assert_eq!(described(&gw), ["list"]);
    }

    #[tokio::test]
    async fn player_reports_state_now_and_queue() {
        let gw = playback_script(Some(X), &[A], never);
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "state": "playing", "now": X, "queue": [A], "output": null,
                "position_ms": 1000, "duration_ms": 200_000, "preview": null
            })
        );

        let idle = playback_script(None, &[], never);
        let res = send_get(test_app_with(idle.addr()), "/api/player", None).await;
        assert_eq!(
            body_json(res).await,
            json!({
                "state": "idle", "now": null, "queue": [], "output": null,
                "position_ms": null, "duration_ms": null, "preview": null
            })
        );
    }

    /// A playback service whose transport is `transport`, and nothing queued.
    fn transport_script(transport: TransportStatus) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Playback {
                request: Command::GetTransport { .. },
            } => playback_ok(Some(ResponseData::Transport(transport.clone()))),
            GatewayRequest::Playback {
                request: Command::QueueList,
            } => playback_ok(Some(ResponseData::Queue(vec![]))),
            _ => playback_ok(None),
        })
    }

    #[tokio::test]
    async fn a_paused_track_is_reported_paused_by_the_transport_not_guessed() {
        let gw = transport_script(loaded(A, Transport::Paused, None));
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(body_json(res).await["state"], "paused");
    }

    #[tokio::test]
    async fn a_preview_is_reported_with_what_it_goes_back_to() {
        let held = Held {
            hash: hash(B),
            at: PositionMs::new(151_000),
            transport: Transport::Paused,
        };
        let gw = transport_script(loaded(
            A,
            Transport::Playing,
            Some(PreviewStatus {
                back_to: Some(held),
            }),
        ));
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        let body = body_json(res).await;
        assert_eq!(body["now"], A);
        assert_eq!(
            body["preview"],
            json!({"back_to": B, "back_at_ms": 151_000, "back_transport": "paused"})
        );
    }

    #[tokio::test]
    async fn a_preview_over_nothing_has_no_back_to() {
        let gw = transport_script(loaded(
            A,
            Transport::Playing,
            Some(PreviewStatus { back_to: None }),
        ));
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(
            body_json(res).await["preview"],
            json!({"back_to": null, "back_at_ms": null, "back_transport": null})
        );
    }

    #[tokio::test]
    async fn a_position_the_source_cannot_say_is_null() {
        let gw = transport_script(TransportStatus::Loaded {
            transport: Transport::Playing,
            now: hash(A),
            position_ms: None,
            duration_ms: None,
            preview: None,
        });
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        let body = body_json(res).await;
        assert!(body["position_ms"].is_null());
        assert!(body["duration_ms"].is_null());
    }

    #[tokio::test]
    async fn a_node_that_does_not_know_get_transport_is_502() {
        let gw = playback_script(Some(X), &[], |c| matches!(c, Command::GetTransport { .. }));
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    // ---- preview ----

    /// `POST /api/preview` with `body`.
    async fn preview_call(gw: &FakeGateway, body: serde_json::Value) -> axum::response::Response {
        send_post_json(test_app_with(gw.addr()), "/api/preview", &body).await
    }

    #[tokio::test]
    async fn previewing_resolves_the_hash_sends_preview_on_deck_a_and_answers_like_the_player() {
        let gw = playback_script(Some(A), &[B], never);
        let res = preview_call(&gw, json!({"hash": SHORT_A})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["now"], A);
        assert_eq!(body["queue"], json!([B]));
        let previews: Vec<_> = gw
            .commands()
            .into_iter()
            .filter(|c| matches!(c, Command::Preview { .. }))
            .collect();
        assert!(
            matches!(&previews[..], [Command::Preview { deck: Deck::A, hash }] if hash.as_str() == A),
            "{previews:?}"
        );
    }

    #[tokio::test]
    async fn previewing_without_a_hash_is_400_and_sends_nothing() {
        let gw = playback_script(Some(X), &[], never);
        for body in [json!({}), json!({"hash": null})] {
            let res = preview_call(&gw, body).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn previewing_an_unknown_hash_is_422_and_sends_nothing_to_playback() {
        let gw = playback_script(Some(A), &[], never);
        let res = preview_call(&gw, json!({"hash": X})).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"].as_str().unwrap().contains(X));
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn a_preview_playback_refuses_is_409_with_its_words() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Playback {
                request: Command::Preview { .. },
            } => playback_err("Cannot preview: the source does not say where the playing track is"),
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => track_ok(hash.as_str()),
            _ => playback_ok(None),
        });
        let res = preview_call(&gw, json!({"hash": A})).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(res).await,
            json!({"error": "Cannot preview: the source does not say where the playing track is"})
        );
    }

    #[tokio::test]
    async fn a_gateway_that_does_not_know_preview_is_502() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Playback {
                request: Command::Preview { .. },
            } => GatewayResponse::Error {
                message: "unknown command".into(),
            },
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => track_ok(hash.as_str()),
            _ => playback_ok(None),
        });
        let res = preview_call(&gw, json!({"hash": A})).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn a_preview_answer_that_cannot_be_read_is_503() {
        let gw = FakeGateway::start_unreadable_when(
            |req| {
                matches!(
                    req,
                    GatewayRequest::Playback {
                        request: Command::Preview { .. }
                    }
                )
            },
            |req| match req {
                GatewayRequest::Library {
                    request: LibraryRequest::GetTrack { hash },
                } => track_ok(hash.as_str()),
                _ => playback_ok(None),
            },
        );
        let res = preview_call(&gw, json!({"hash": A})).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn previewing_with_no_gateway_is_503() {
        let res = send_post_json(
            crate::test_support::test_app(),
            "/api/preview",
            &json!({"hash": A}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ---- back ----

    #[tokio::test]
    async fn back_sends_end_preview_on_deck_a_and_answers_like_the_player() {
        let gw = playback_script(Some(B), &[A], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/preview/back", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["now"], B);
        assert!(gw
            .commands()
            .iter()
            .any(|c| matches!(c, Command::EndPreview { deck: Deck::A })));
    }

    #[tokio::test]
    async fn a_refused_back_is_409_and_a_dead_one_503() {
        let gw = playback_script(Some(B), &[], |c| matches!(c, Command::EndPreview { .. }));
        let res = send_post_json(test_app_with(gw.addr()), "/api/preview/back", &json!({})).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await, json!({"error": "boom"}));

        let res = send_post_json(
            crate::test_support::test_app(),
            "/api/preview/back",
            &json!({}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ---- seek ----

    use library_ipc_client::{BeatGridInfo, Bpm};

    /// 120 BPM: a beat every 500 ms from 100 ms.
    fn grid() -> BeatGridInfo {
        BeatGridInfo {
            first_beat_ms: 100,
            bpm: Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        }
    }

    /// A node on which `A` plays at 1 s of 200 s (or nothing does), whose library holds `A` with
    /// `grid`. `Seek` answers the transport at the position it was sent.
    fn seek_script(playing: bool, grid: Option<BeatGridInfo>) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Playback {
                request: Command::GetTransport { .. },
            } => playback_ok(Some(ResponseData::Transport(if playing {
                loaded(A, Transport::Playing, None)
            } else {
                TransportStatus::Idle
            }))),
            GatewayRequest::Playback {
                request: Command::Seek { position_ms, .. },
            } if playing => playback_ok(Some(ResponseData::Transport(TransportStatus::Loaded {
                transport: Transport::Playing,
                now: hash(A),
                position_ms: Some(*position_ms),
                duration_ms: Some(PositionMs::new(200_000)),
                preview: None,
            }))),
            GatewayRequest::Playback {
                request: Command::Seek { .. },
            } => playback_err("nothing loaded"),
            GatewayRequest::Playback {
                request: Command::QueueList,
            } => playback_ok(Some(ResponseData::Queue(vec![]))),
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => {
                let mut track = crate::test_support::bare_track(hash.as_str());
                track.beat_grid = grid.clone();
                library_ok(LibraryResponse::Track(Box::new(track)))
            }
            _ => playback_ok(None),
        })
    }

    async fn seek_call(gw: &FakeGateway, body: serde_json::Value) -> axum::response::Response {
        send_post_json(test_app_with(gw.addr()), "/api/seek", &body).await
    }

    /// The positions of the `Seek` commands the node received.
    fn seeks(gw: &FakeGateway) -> Vec<u64> {
        gw.commands()
            .into_iter()
            .filter_map(|c| match c {
                Command::Seek { position_ms, .. } => Some(position_ms.as_millis()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_free_seek_goes_where_asked_without_asking_the_library() {
        let gw = seek_script(true, Some(grid()));
        let res = seek_call(&gw, json!({"to_ms": 1230, "placement": "free"})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(seeks(&gw), [1230]);
        assert!(gw
            .requests()
            .iter()
            .all(|r| !matches!(r, GatewayRequest::Library { .. })));
        assert_eq!(body_json(res).await["now"], A);
    }

    #[tokio::test]
    async fn a_snapped_seek_goes_to_the_nearest_beat() {
        let gw = seek_script(true, Some(grid()));
        let res = seek_call(&gw, json!({"to_ms": 1230, "placement": "snap"})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(seeks(&gw), [1100]);
    }

    #[tokio::test]
    async fn a_snapped_seek_on_a_track_without_a_grid_goes_where_asked() {
        let gw = seek_script(true, None);
        let res = seek_call(&gw, json!({"to_ms": 1230, "placement": "snap"})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(seeks(&gw), [1230]);
    }

    #[tokio::test]
    async fn a_beat_step_snaps_steps_and_seeks() {
        // At 1000 ms the nearest beat is 1100; one beat on is 1600.
        let gw = seek_script(true, Some(grid()));
        let res = seek_call(&gw, json!({"by_beats": 1})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(seeks(&gw), [1600]);
        let res = seek_call(&gw, json!({"by_beats": -1})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(seeks(&gw), [1600, 600]);
    }

    #[tokio::test]
    async fn a_beat_step_without_a_grid_is_422_and_seeks_nothing() {
        let gw = seek_script(true, None);
        let res = seek_call(&gw, json!({"by_beats": 4})).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("beat grid"));
        assert!(seeks(&gw).is_empty());
    }

    #[tokio::test]
    async fn seeking_while_idle_is_409_whatever_the_request() {
        let gw = seek_script(false, Some(grid()));
        for body in [
            json!({"to_ms": 5000, "placement": "free"}),
            json!({"to_ms": 5000, "placement": "snap"}),
            json!({"by_beats": 4}),
        ] {
            let res = seek_call(&gw, body.clone()).await;
            assert_eq!(res.status(), StatusCode::CONFLICT, "{body}");
        }
    }

    #[tokio::test]
    async fn a_seek_that_names_neither_a_time_nor_beats_is_422_and_sends_nothing() {
        let gw = seek_script(true, Some(grid()));
        for body in [
            json!({}),
            json!({"to_ms": 5000}),
            json!({"to_ms": 5000, "placement": "sideways"}),
            json!({"to_ms": 5000, "placement": "free", "by_beats": 1}),
        ] {
            let res = seek_call(&gw, body.clone()).await;
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn a_gateway_that_does_not_know_seek_is_502_not_409() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Playback {
                request: Command::Seek { .. },
            } => GatewayResponse::Error {
                message: "unknown command".into(),
            },
            _ => playback_ok(None),
        });
        let res = seek_call(&gw, json!({"to_ms": 5000, "placement": "free"})).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn seeking_with_no_gateway_is_503() {
        let res = send_post_json(
            crate::test_support::test_app(),
            "/api/seek",
            &json!({"to_ms": 5000, "placement": "free"}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_seek_through_a_library_that_is_down_is_502() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Playback {
                request: Command::GetTransport { .. },
            } => playback_ok(Some(ResponseData::Transport(loaded(
                A,
                Transport::Playing,
                None,
            )))),
            GatewayRequest::Library { .. } => GatewayResponse::Error {
                message: "library unavailable".into(),
            },
            _ => playback_ok(None),
        });
        let res = seek_call(&gw, json!({"by_beats": 1})).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert!(seeks(&gw).is_empty());
    }

    #[tokio::test]
    async fn player_reports_output_from_get_output_status() {
        let gw = playback_script_with_output(Some(X), &[], never, Some(OutputHealth::Stalled));
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["output"], "stalled");
    }

    #[tokio::test]
    async fn player_on_an_old_node_has_null_output() {
        // An old node answers the unknown command with an error: still a 200.
        let gw = playback_script(Some(X), &[A], |c| matches!(c, Command::GetOutputStatus));
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert!(body["output"].is_null());
        assert_eq!(body["now"], X);
    }

    #[tokio::test]
    async fn pause_and_resume_replies_carry_the_output() {
        let gw = playback_script_with_output(Some(X), &[], never, Some(OutputHealth::Running));
        let app = test_app_with(gw.addr());
        for path in ["/api/pause", "/api/resume"] {
            let res = send_post_json(app.clone(), path, &json!({})).await;
            assert_eq!(body_json(res).await["output"], "running", "{path}");
        }
    }

    #[tokio::test]
    async fn player_without_a_gateway_is_503() {
        let res = send_get(crate::test_support::test_app(), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ---- replace ----

    #[tokio::test]
    async fn queue_replace_replaces_up_next_without_touching_what_plays() {
        let gw = playback_script(Some(X), &[C], never);
        let res = queue_call(&gw, "replace", json!([A, B])).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 2);
        assert_eq!(body["previous"], json!([C]));
        // The read for `previous`, one atomic replace and the read of the result: no stop, no
        // play, no idle check.
        assert_eq!(
            described(&gw),
            [
                "list".to_string(),
                format!("replace {A},{B}"),
                "list".into()
            ]
        );
    }

    #[tokio::test]
    async fn queue_replace_that_playback_refuses_is_502_and_queued_nothing() {
        let gw = playback_script(Some(X), &[C], |c| matches!(c, Command::QueueReplace { .. }));
        let res = queue_call(&gw, "replace", json!([A])).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            body_json(res).await,
            json!({"error": "boom", "queued": 0, "previous": [C]})
        );
    }

    // ---- /api/playlists/:name/queue ----

    async fn queue_playlist(
        gw: &FakeGateway,
        name: &str,
        action: &str,
    ) -> axum::response::Response {
        send_post_json(
            test_app_with(gw.addr()),
            &format!("/api/playlists/{name}/queue"),
            &json!({ "action": action }),
        )
        .await
    }

    /// The `skipped` of "set": its one unavailable line, the second (0-based position 1).
    fn set_skipped() -> serde_json::Value {
        json!([{"position": 1, "error": {"Unknown": {"hash": "dead"}}}])
    }

    #[tokio::test]
    async fn queueing_a_playlist_next_sends_its_playable_entries_in_reverse_and_reports_the_rest() {
        let gw = playback_script(Some(X), &[A, B], never);
        let res = queue_playlist(&gw, "set", "next").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 2);
        assert_eq!(body["skipped"], set_skipped());
        assert_eq!(body["queue"], json!([A, B]));
        assert_eq!(body["previous"], json!([A, B]));
        assert_eq!(
            described(&gw),
            [
                "list".to_string(),
                format!("next {B}"),
                format!("next {A}"),
                "list".into()
            ]
        );
    }

    #[tokio::test]
    async fn queueing_a_playlist_last_appends_after_what_is_queued_and_never_clears() {
        let gw = playback_script(Some(X), &[C], never);
        let res = queue_playlist(&gw, "set", "last").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 2);
        assert_eq!(body["skipped"], set_skipped());
        assert_eq!(body["previous"], json!([C]));
        assert_eq!(
            described(&gw),
            [
                "list".to_string(),
                format!("append {A}"),
                format!("append {B}"),
                "list".into()
            ]
        );
    }

    #[tokio::test]
    async fn replacing_the_queue_with_a_playlist_is_one_atomic_replace_that_leaves_playback_alone()
    {
        let gw = playback_script(Some(X), &[C], never);
        let res = queue_playlist(&gw, "set", "replace").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 2);
        assert_eq!(body["skipped"], set_skipped());
        assert_eq!(
            described(&gw),
            [
                "list".to_string(),
                format!("replace {A},{B}"),
                "list".into()
            ]
        );
    }

    #[tokio::test]
    async fn a_playlist_replace_that_playback_refuses_queued_nothing() {
        let gw = playback_script(Some(X), &[C], |c| matches!(c, Command::QueueReplace { .. }));
        let res = queue_playlist(&gw, "set", "replace").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 0);
        assert_eq!(body["skipped"], set_skipped());
        assert_eq!(
            described(&gw),
            ["list".to_string(), format!("replace {A},{B}")]
        );
    }

    #[tokio::test]
    async fn playing_a_playlist_now_queues_the_playable_entries_at_the_front_then_starts() {
        let gw = playback_script(None, &[A, B], never);
        let res = queue_playlist(&gw, "set", "now").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 2);
        assert_eq!(body["skipped"], set_skipped());
        assert_eq!(
            described(&gw),
            [
                "now".to_string(),
                format!("next {B}"),
                format!("next {A}"),
                "now".into(),
                "play".into(),
                "list".into()
            ]
        );
    }

    #[tokio::test]
    async fn playing_a_playlist_now_while_something_plays_is_409_and_changes_nothing() {
        let gw = playback_script(Some(X), &[C], never);
        let res = queue_playlist(&gw, "set", "now").await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await["error"], "busy");
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn skipped_positions_are_zero_based_among_all_the_playlists_entries() {
        let gw = playback_script(Some(X), &[], never);
        let res = queue_playlist(&gw, "holes", "last").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 2);
        assert_eq!(
            body["skipped"],
            json!([
                {"position": 0, "error": {"Unknown": {"hash": "dead"}}},
                {"position": 2, "error": {"Unknown": {"hash": "beef"}}},
            ])
        );
    }

    #[tokio::test]
    async fn an_empty_set_never_changes_up_next_whatever_the_action_or_the_player() {
        // Idle and busy: a `now` on nothing must not start what is already queued either.
        for now in [None, Some(X)] {
            for action in ["next", "last", "replace", "now"] {
                let gw = playback_script(now, &[C], never);
                let res = queue_playlist(&gw, "gone", action).await;
                assert_eq!(res.status(), StatusCode::OK, "{action} {now:?}");
                assert_eq!(
                    body_json(res).await,
                    json!({
                        "queued": 0,
                        "queue": [C],
                        "skipped": [{"position": 0, "error": {"Unknown": {"hash": "dead"}}}],
                    }),
                    "{action} {now:?}"
                );
                // Only the read that says what Up next is: nothing changed, nothing started.
                assert_eq!(described(&gw), ["list"], "{action} {now:?}");
            }
        }
    }

    #[tokio::test]
    async fn queueing_a_missing_playlist_is_404_and_sends_nothing() {
        let gw = playback_script(Some(X), &[], never);
        let res = queue_playlist(&gw, "nope", "last").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn queueing_with_an_unknown_action_or_a_bad_name_is_refused_and_sends_nothing() {
        let gw = playback_script(Some(X), &[], never);
        let res = queue_playlist(&gw, "set", "explode").await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let res = queue_playlist(&gw, "bad%20name", "last").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn queueing_a_playlist_with_the_library_down_sends_nothing() {
        let refusing = FakeGateway::start(|req| match req {
            GatewayRequest::Library { .. } => GatewayResponse::Error {
                message: "library unavailable".into(),
            },
            _ => playback_ok(None),
        });
        for action in ["next", "last", "replace", "now"] {
            let res = queue_playlist(&refusing, "set", action).await;
            assert_eq!(res.status(), StatusCode::BAD_GATEWAY, "{action}");
        }
        assert!(refusing.commands().is_empty());

        let unreadable = FakeGateway::start_unreadable_when(
            |req| matches!(req, GatewayRequest::Library { .. }),
            |_| playback_ok(None),
        );
        let res = queue_playlist(&unreadable, "set", "replace").await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(unreadable.commands().is_empty());
    }

    #[tokio::test]
    async fn queueing_a_playlist_with_no_gateway_is_503() {
        let res = send_post_json(
            crate::test_support::test_app(),
            "/api/playlists/set/queue",
            &json!({"action": "last"}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
