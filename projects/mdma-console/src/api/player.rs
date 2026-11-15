//! Player wire types for the new web UI.
//!
//! `ApiPlayer` is the shape of `GET /api/player` and the `{queue}` every `/api/queue/*`
//! mutation returns. `static/js/core.js` reads exactly these keys.

//!
//! Every playback command sent here is queue-level, which the protocol routes to deck A and
//! which carries no `deck` field yet (ADR-017 phase 1); nothing here targets another deck.

use super::{error_reply, fail, gw, json_or_500, respond, send, Reply};
use crate::play_now::{
    queue_last_commands, queue_next_commands, require_idle, start_command, Busy,
};
use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Json, Response},
};
use event_protocol::PlaybackEvent;
use gateway_client::{Command, GatewayClient};
use media_protocol::{ContentHash, Deck, ResponseData, SourceName};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Transport state. `idle` means nothing is loaded; paused and loading both count as busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerState {
    Idle,
    Playing,
    Paused,
    /// Reserved for a transport status command (G6); not produced yet.
    #[allow(dead_code)]
    Loading,
}

/// What the console knows about pause, from the event stream. `NowPlaying` answers `Some` for
/// playing and paused alike, so this is the only way to tell them apart until the playback
/// service reports it. A cold console sees a paused track as playing until the next event.
#[derive(Debug, Default)]
pub struct TransportCache {
    paused: Mutex<Option<String>>,
}

impl TransportCache {
    /// Fold one playback event in: pause marks the track paused, anything else about a track
    /// (resumed, started, stopped, ended) clears it.
    pub fn apply(&self, event: &PlaybackEvent) {
        let mut paused = self.paused.lock().unwrap_or_else(|p| p.into_inner());
        match event {
            PlaybackEvent::TrackPaused { hash } => *paused = Some(hash.as_str().to_owned()),
            PlaybackEvent::TrackResumed { .. }
            | PlaybackEvent::TrackStarted { .. }
            | PlaybackEvent::TrackStopped { .. }
            | PlaybackEvent::TrackEnded { .. } => *paused = None,
            _ => {}
        }
    }

    /// Remember that `hash` is paused (the console just paused it itself).
    pub fn mark_paused(&self, hash: &ContentHash) {
        let mut paused = self.paused.lock().unwrap_or_else(|p| p.into_inner());
        *paused = Some(hash.as_str().to_owned());
    }

    /// Forget any pause (the console just resumed).
    pub fn clear(&self) {
        let mut paused = self.paused.lock().unwrap_or_else(|p| p.into_inner());
        *paused = None;
    }

    /// Transport state given what `NowPlaying` reported.
    pub fn state_for(&self, now: Option<&ContentHash>) -> PlayerState {
        let paused = self.paused.lock().unwrap_or_else(|p| p.into_inner());
        match now {
            None => PlayerState::Idle,
            Some(hash) if paused.as_deref() == Some(hash.as_str()) => PlayerState::Paused,
            Some(_) => PlayerState::Playing,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiPlayer {
    pub state: PlayerState,
    /// Hash of the loaded track, or null when idle.
    pub now: Option<String>,
    /// Hashes in Up next, next track first.
    pub queue: Vec<String>,
}

/// The loaded track. An answer that is not a `NowPlaying` is an error, never "idle": the idle
/// guard must not pass on a reply it does not understand.
fn now_of(client: &GatewayClient) -> Result<Option<ContentHash>, String> {
    match send(client, &Command::NowPlaying)? {
        Some(ResponseData::NowPlaying(now)) => Ok(now),
        other => Err(format!("Unexpected NowPlaying response: {other:?}")),
    }
}

fn queue_of(client: &GatewayClient) -> Result<Vec<ContentHash>, String> {
    match send(client, &Command::QueueList)? {
        Some(ResponseData::Queue(queue)) => Ok(queue),
        other => Err(format!("Unexpected QueueList response: {other:?}")),
    }
}

fn strings(hashes: &[ContentHash]) -> Vec<String> {
    hashes.iter().map(|h| h.as_str().to_owned()).collect()
}

/// Send `commands` in order, stopping at the first failure. Returns how many succeeded and the
/// failure message, if any.
fn send_all(client: &GatewayClient, commands: &[Command]) -> (usize, Option<String>) {
    for (done, command) in commands.iter().enumerate() {
        if let Err(message) = send(client, command) {
            return (done, Some(message));
        }
    }
    (commands.len(), None)
}

/// Report `queued` insertions and the queue as it now is. No failure: 200 `{queued, queue}`.
/// Some done: 207 `{queued, error, queue}`. None done: 500 `{error, queued: 0}`.
fn batch_reply(client: &GatewayClient, queued: usize, failure: Option<String>) -> Reply {
    match failure {
        None => match queue_of(client) {
            Ok(queue) => (
                StatusCode::OK,
                serde_json::json!({ "queued": queued, "queue": strings(&queue) }),
            ),
            Err(message) => error_reply(StatusCode::INTERNAL_SERVER_ERROR, message),
        },
        Some(error) if queued == 0 => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": error, "queued": 0 }),
        ),
        Some(error) => {
            let queue = queue_of(client).map(|q| strings(&q)).unwrap_or_default();
            (
                StatusCode::MULTI_STATUS,
                serde_json::json!({ "queued": queued, "error": error, "queue": queue }),
            )
        }
    }
}

/// Send `commands` and report the outcome (see [`batch_reply`]).
fn run_batch(client: &GatewayClient, commands: &[Command]) -> Reply {
    let (done, failure) = send_all(client, commands);
    batch_reply(client, done, failure)
}

/// Everything was queued but the track could not be started: 500
/// `{error, queued, stage: "start", queue}`.
fn start_failed_reply(client: &GatewayClient, queued: usize, error: String) -> Reply {
    let queue = queue_of(client).map(|q| strings(&q)).unwrap_or_default();
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({ "error": error, "queued": queued, "stage": "start", "queue": queue }),
    )
}

/// The player as `GET /api/player` reports it, for a `NowPlaying`/`QueueList` snapshot.
fn api_player(
    transport: &TransportCache,
    now: Option<ContentHash>,
    queue: &[ContentHash],
) -> ApiPlayer {
    ApiPlayer {
        state: transport.state_for(now.as_ref()),
        now: now.map(|h| h.as_str().to_owned()),
        queue: strings(queue),
    }
}

fn snapshot(client: &GatewayClient) -> Result<(Option<ContentHash>, Vec<ContentHash>), String> {
    Ok((now_of(client)?, queue_of(client)?))
}

#[derive(Debug, Deserialize)]
pub struct HashesRequest {
    hashes: Vec<String>,
}

#[allow(clippy::result_large_err)]
fn parse_hashes(hashes: Vec<String>) -> Result<Vec<ContentHash>, Response> {
    if hashes.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, "no hashes given".to_string()));
    }
    Ok(hashes.into_iter().map(ContentHash::new).collect())
}

/// `GET /api/player`: `{state, now, queue}`.
pub async fn player(State(state): State<Arc<AppState>>) -> Response {
    let read = gw(&state, snapshot).await;
    json_or_500(read.map(|r| r.map(|(now, queue)| api_player(&state.transport, now, &queue))))
}

/// `POST /api/pause`: pause deck A, then answer like `GET /api/player`. The console remembers
/// the pause (a cold console cannot tell paused from playing otherwise).
pub async fn pause(State(state): State<Arc<AppState>>) -> Response {
    let read = gw(&state, |client| {
        send(client, &Command::Pause { deck: Deck::A })?;
        snapshot(client)
    })
    .await;
    json_or_500(read.map(|r| {
        r.map(|(now, queue)| {
            if let Some(hash) = &now {
                state.transport.mark_paused(hash);
            }
            api_player(&state.transport, now, &queue)
        })
    }))
}

/// `POST /api/resume`: resume deck A, then answer like `GET /api/player`.
pub async fn resume(State(state): State<Arc<AppState>>) -> Response {
    let read = gw(&state, |client| {
        send(client, &Command::Resume { deck: Deck::A })?;
        snapshot(client)
    })
    .await;
    json_or_500(read.map(|r| {
        r.map(|(now, queue)| {
            state.transport.clear();
            api_player(&state.transport, now, &queue)
        })
    }))
}

/// `POST /api/queue/next {hashes}`: the set goes to the front of Up next, in the order given.
pub async fn queue_next(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HashesRequest>,
) -> Response {
    let hashes = match parse_hashes(req.hashes) {
        Ok(h) => h,
        Err(response) => return response,
    };
    respond(
        gw(&state, move |client| {
            run_batch(client, &queue_next_commands(&hashes))
        })
        .await,
    )
}

/// `POST /api/queue/last {hashes}`: the set goes to the end of Up next, in the order given.
pub async fn queue_last(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HashesRequest>,
) -> Response {
    let hashes = match parse_hashes(req.hashes) {
        Ok(h) => h,
        Err(response) => return response,
    };
    respond(
        gw(&state, move |client| {
            run_batch(client, &queue_last_commands(&hashes))
        })
        .await,
    )
}

fn busy_reply() -> Reply {
    error_reply(StatusCode::CONFLICT, "busy")
}

/// `POST /api/play-now {hashes}`: queue the set at the front, then start it, but only while
/// nothing is loaded; the guard is checked before queueing and again before `PlayQueue`
/// (something may have started in between).
///
/// Replies: 200 `{queued, queue}`; 409 `{error: "busy"}` (nothing sent) or 409
/// `{error: "busy", queued, queue}` (became busy after queueing, so not started); 207/500 as
/// for `/api/queue/next` when queueing fails; 500 `{error, queued, stage: "start", queue}` when
/// everything was queued but the start failed.
pub async fn play_now(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HashesRequest>,
) -> Response {
    let hashes = match parse_hashes(req.hashes) {
        Ok(h) => h,
        Err(response) => return response,
    };
    respond(
        gw(&state, move |client| {
            let now = match now_of(client) {
                Ok(now) => now,
                Err(message) => return error_reply(StatusCode::INTERNAL_SERVER_ERROR, message),
            };
            if require_idle(now).is_err() {
                return busy_reply();
            }
            let (queued, failure) = send_all(client, &queue_next_commands(&hashes));
            if failure.is_some() {
                return batch_reply(client, queued, failure);
            }
            let again = match now_of(client) {
                Ok(now) => now,
                Err(message) => return start_failed_reply(client, queued, message),
            };
            match require_idle(again) {
                Ok(idle) => match send(client, &start_command(idle)) {
                    Ok(_) => batch_reply(client, queued, None),
                    Err(message) => start_failed_reply(client, queued, message),
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
        .await,
    )
}

/// `POST /api/start`: start the queue, only when nothing is loaded; 409 otherwise.
pub async fn start(State(state): State<Arc<AppState>>) -> Response {
    respond(
        gw(&state, |client| {
            let now = match now_of(client) {
                Ok(now) => now,
                Err(message) => return error_reply(StatusCode::INTERNAL_SERVER_ERROR, message),
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
/// `expect`; otherwise 409 `{error, queue: current}` and nothing is touched.
pub async fn queue_edit(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EditRequest>,
) -> Response {
    respond(
        gw(&state, move |client| {
            let current = match queue_of(client) {
                Ok(q) => strings(&q),
                Err(message) => return error_reply(StatusCode::INTERNAL_SERVER_ERROR, message),
            };
            if current != req.expect {
                return (
                    StatusCode::CONFLICT,
                    serde_json::json!({ "error": "stale", "queue": current }),
                );
            }
            let entries = req
                .queue
                .into_iter()
                .map(|h| (ContentHash::new(h), SourceName::audio()))
                .collect();
            if let Err(message) = send(client, &Command::QueueReplace { entries }) {
                return error_reply(StatusCode::INTERNAL_SERVER_ERROR, message);
            }
            match queue_of(client) {
                Ok(queue) => (
                    StatusCode::OK,
                    serde_json::json!({ "queue": strings(&queue) }),
                ),
                Err(message) => error_reply(StatusCode::INTERNAL_SERVER_ERROR, message),
            }
        })
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_json_has_exactly_state_now_queue() {
        let p = ApiPlayer {
            state: PlayerState::Paused,
            now: Some("sha256:a".into()),
            queue: vec!["sha256:b".into()],
        };
        let json = serde_json::to_value(&p).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["now", "queue", "state"]);
        assert_eq!(json["state"], "paused");
        assert_eq!(json["queue"][0], "sha256:b");
    }

    #[test]
    fn idle_player_has_null_now_and_every_state_is_lowercase() {
        let p = ApiPlayer {
            state: PlayerState::Idle,
            now: None,
            queue: vec![],
        };
        let json = serde_json::to_value(&p).unwrap();
        assert!(json["now"].is_null());
        for (s, want) in [
            (PlayerState::Idle, "idle"),
            (PlayerState::Playing, "playing"),
            (PlayerState::Paused, "paused"),
            (PlayerState::Loading, "loading"),
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), want);
        }
    }

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    #[test]
    fn transport_cache_tracks_pause_by_hash() {
        let cache = TransportCache::default();
        let a = hash("sha256:a");
        assert_eq!(cache.state_for(None), PlayerState::Idle);
        assert_eq!(cache.state_for(Some(&a)), PlayerState::Playing);

        cache.apply(&PlaybackEvent::TrackPaused { hash: a.clone() });
        assert_eq!(cache.state_for(Some(&a)), PlayerState::Paused);
        // A pause remembered for another track says nothing about this one.
        assert_eq!(
            cache.state_for(Some(&hash("sha256:b"))),
            PlayerState::Playing
        );
        assert_eq!(cache.state_for(None), PlayerState::Idle);

        cache.apply(&PlaybackEvent::TrackResumed { hash: a.clone() });
        assert_eq!(cache.state_for(Some(&a)), PlayerState::Playing);
    }

    #[test]
    fn transport_cache_clears_on_start_stop_and_end() {
        for event in [
            PlaybackEvent::TrackStarted {
                hash: hash("sha256:a"),
            },
            PlaybackEvent::TrackStopped {
                hash: hash("sha256:a"),
            },
            PlaybackEvent::TrackEnded {
                hash: hash("sha256:a"),
            },
        ] {
            let cache = TransportCache::default();
            cache.apply(&PlaybackEvent::TrackPaused {
                hash: hash("sha256:a"),
            });
            cache.apply(&event);
            assert_eq!(
                cache.state_for(Some(&hash("sha256:a"))),
                PlayerState::Playing,
                "{event:?}"
            );
        }
    }

    // ---- handlers, against a fake gateway ----

    use crate::test_support::{
        body_json, described, playback_err, playback_ok, send_get, send_post_json, test_app_with,
        FakeGateway,
    };
    use gateway_client::{GatewayRequest, GatewayResponse};
    use serde_json::json;

    /// A playback service with `now` loaded and `queue` queued; everything else succeeds, except
    /// commands for which `fails` returns true.
    fn playback_script(
        now: Option<&'static str>,
        queue: &'static [&'static str],
        fails: fn(&Command) -> bool,
    ) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Playback { request } if fails(request) => playback_err("boom"),
            GatewayRequest::Playback {
                request: Command::NowPlaying,
            } => playback_ok(Some(ResponseData::NowPlaying(now.map(hash)))),
            GatewayRequest::Playback {
                request: Command::QueueList,
            } => playback_ok(Some(ResponseData::Queue(
                queue.iter().map(|h| hash(h)).collect(),
            ))),
            GatewayRequest::Playback { .. } => playback_ok(None),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    fn never(_: &Command) -> bool {
        false
    }

    #[tokio::test]
    async fn play_now_while_something_is_loaded_is_409_and_sends_nothing_else() {
        let gw = playback_script(Some("sha256:x"), &[], never);
        let res = crate::test_support::send_post_json(
            test_app_with(gw.addr()),
            "/api/play-now",
            &json!({"hashes": ["a", "b"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await, json!({"error": "busy"}));
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn start_while_something_is_loaded_is_409_and_sends_nothing_else() {
        let gw = playback_script(Some("sha256:x"), &["sha256:q"], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/start", &json!({})).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await, json!({"error": "busy"}));
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn an_unintelligible_now_playing_answer_does_not_pass_the_guard() {
        let gw = FakeGateway::start(|_| playback_ok(None));
        let res = send_post_json(test_app_with(gw.addr()), "/api/start", &json!({})).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(described(&gw), ["now"]);
    }

    #[tokio::test]
    async fn play_now_when_idle_queues_the_set_in_order_then_plays() {
        let gw = playback_script(None, &["sha256:a", "sha256:b", "sha256:c"], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/play-now",
            &json!({"hashes": ["sha256:a", "sha256:b", "sha256:c"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queue"], json!(["sha256:a", "sha256:b", "sha256:c"]));
        assert_eq!(body["queued"], 3);
        assert_eq!(
            described(&gw),
            [
                "now",
                "next sha256:c",
                "next sha256:b",
                "next sha256:a",
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
                let now = (nows > 1).then(|| hash("sha256:x"));
                playback_ok(Some(ResponseData::NowPlaying(now)))
            }
            GatewayRequest::Playback {
                request: Command::QueueList,
            } => playback_ok(Some(ResponseData::Queue(vec![hash("sha256:a")]))),
            _ => playback_ok(None),
        });
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/play-now",
            &json!({"hashes": ["sha256:a"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(res).await,
            json!({"error": "busy", "queued": 1, "queue": ["sha256:a"]})
        );
        assert_eq!(described(&gw), ["now", "next sha256:a", "now", "list"]);
    }

    #[tokio::test]
    async fn play_now_whose_start_fails_says_so_with_what_was_queued() {
        let gw = playback_script(None, &["sha256:a"], |c| matches!(c, Command::PlayQueue));
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/play-now",
            &json!({"hashes": ["sha256:a"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body_json(res).await,
            json!({"error": "boom", "queued": 1, "stage": "start", "queue": ["sha256:a"]})
        );
    }

    #[tokio::test]
    async fn pause_on_a_cold_console_is_remembered_as_paused() {
        let gw = playback_script(Some("sha256:x"), &["sha256:a"], never);
        let app = test_app_with(gw.addr());
        let res = send_post_json(app.clone(), "/api/pause", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({"state": "paused", "now": "sha256:x", "queue": ["sha256:a"]})
        );
        let res = send_get(app, "/api/player", None).await;
        assert_eq!(body_json(res).await["state"], "paused");
        assert!(gw
            .commands()
            .iter()
            .any(|c| matches!(c, Command::Pause { deck: Deck::A })));
    }

    #[tokio::test]
    async fn resume_clears_the_pause() {
        let gw = playback_script(Some("sha256:x"), &[], never);
        let app = test_app_with(gw.addr());
        send_post_json(app.clone(), "/api/pause", &json!({})).await;
        let res = send_post_json(app.clone(), "/api/resume", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["state"], "playing");
        let res = send_get(app, "/api/player", None).await;
        assert_eq!(body_json(res).await["state"], "playing");
        assert!(gw
            .commands()
            .iter()
            .any(|c| matches!(c, Command::Resume { deck: Deck::A })));
    }

    #[tokio::test]
    async fn pause_failure_is_500_and_leaves_the_cache_alone() {
        let gw = playback_script(Some("sha256:x"), &[], |c| {
            matches!(c, Command::Pause { .. })
        });
        let app = test_app_with(gw.addr());
        let res = send_post_json(app.clone(), "/api/pause", &json!({})).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let res = send_get(app, "/api/player", None).await;
        assert_eq!(body_json(res).await["state"], "playing");
    }

    #[tokio::test]
    async fn start_when_idle_sends_play_queue() {
        let gw = playback_script(None, &["sha256:a"], never);
        let res = send_post_json(test_app_with(gw.addr()), "/api/start", &json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(described(&gw), ["now", "play", "list"]);
    }

    #[tokio::test]
    async fn queue_next_sends_the_set_in_reverse() {
        let gw = playback_script(Some("sha256:x"), &["sha256:a", "sha256:b"], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/next",
            &json!({"hashes": ["sha256:a", "sha256:b"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["queue"], json!(["sha256:a", "sha256:b"]));
        assert_eq!(described(&gw), ["next sha256:b", "next sha256:a", "list"]);
    }

    #[tokio::test]
    async fn queue_next_partial_failure_is_207_with_the_truth() {
        // Reverse order a, b, c is sent c, b, a; b fails, so only c got in.
        let gw = playback_script(
            None,
            &["sha256:c"],
            |c| matches!(c, Command::QueueNext { hash, .. } if hash.as_str() == "sha256:b"),
        );
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/next",
            &json!({"hashes": ["sha256:a", "sha256:b", "sha256:c"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::MULTI_STATUS);
        let body = body_json(res).await;
        assert_eq!(body["queued"], 1);
        assert_eq!(body["error"], "boom");
        assert_eq!(body["queue"], json!(["sha256:c"]));
        assert_eq!(described(&gw), ["next sha256:c", "next sha256:b", "list"]);
    }

    #[tokio::test]
    async fn queue_next_total_failure_is_an_error_status() {
        let gw = playback_script(None, &[], |c| matches!(c, Command::QueueNext { .. }));
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/next",
            &json!({"hashes": ["sha256:a"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_json(res).await["queued"], 0);
    }

    #[tokio::test]
    async fn queue_last_appends_in_order() {
        let gw = playback_script(Some("sha256:x"), &["sha256:a", "sha256:b"], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/last",
            &json!({"hashes": ["sha256:a", "sha256:b"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            described(&gw),
            ["append sha256:a", "append sha256:b", "list"]
        );
    }

    #[tokio::test]
    async fn an_empty_set_is_a_400_and_sends_nothing() {
        let gw = playback_script(None, &[], never);
        for path in ["/api/queue/next", "/api/queue/last", "/api/play-now"] {
            let res = send_post_json(test_app_with(gw.addr()), path, &json!({"hashes": []})).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{path}");
        }
        assert!(gw.commands().is_empty());
    }

    #[tokio::test]
    async fn queue_edit_with_a_stale_expectation_is_409_and_replaces_nothing() {
        let gw = playback_script(Some("sha256:x"), &["sha256:b"], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": ["sha256:a", "sha256:b"], "queue": ["sha256:b", "sha256:a"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(res).await["queue"], json!(["sha256:b"]));
        assert_eq!(described(&gw), ["list"]);
    }

    #[tokio::test]
    async fn queue_edit_with_a_matching_expectation_replaces_the_queue() {
        let gw = playback_script(Some("sha256:x"), &["sha256:a", "sha256:b"], never);
        let res = send_post_json(
            test_app_with(gw.addr()),
            "/api/queue/edit",
            &json!({"expect": ["sha256:a", "sha256:b"], "queue": ["sha256:b"]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(described(&gw), ["list", "replace sha256:b", "list"],);
        assert!(body_json(res).await["queue"].is_array());
    }

    #[tokio::test]
    async fn player_reports_state_now_and_queue() {
        let gw = playback_script(Some("sha256:x"), &["sha256:a"], never);
        let res = send_get(test_app_with(gw.addr()), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({"state": "playing", "now": "sha256:x", "queue": ["sha256:a"]})
        );

        let idle = playback_script(None, &[], never);
        let res = send_get(test_app_with(idle.addr()), "/api/player", None).await;
        assert_eq!(
            body_json(res).await,
            json!({"state": "idle", "now": null, "queue": []})
        );
    }

    #[tokio::test]
    async fn player_without_a_gateway_is_503() {
        let res = send_get(crate::test_support::test_app(), "/api/player", None).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
