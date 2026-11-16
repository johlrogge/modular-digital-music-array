//! JSON API for the new web UI. Each submodule registers its own routes here.

use crate::AppState;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use gateway_client::Response as PlaybackResponse;
use gateway_client::{Command, GatewayClient, LibraryRequest, LibraryResponse};
use library_ipc_client::{CanonicalHash, ContentHash};
use media_protocol::ResponseData;
use serde::Serialize;
use std::sync::Arc;

pub mod library;
pub mod player;

/// All `/api/...` routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/tracks", get(library::tracks))
        .route("/api/tracks/match", get(library::tracks_match))
        .route("/api/library/match", get(library::library_match))
        .route("/api/playlists", get(library::playlists))
        .route("/api/player", get(player::player))
        .route("/api/pause", post(player::pause))
        .route("/api/resume", post(player::resume))
        .route("/api/queue/next", post(player::queue_next))
        .route("/api/queue/last", post(player::queue_last))
        .route("/api/queue/edit", post(player::queue_edit))
        .route("/api/play-now", post(player::play_now))
        .route("/api/start", post(player::start))
}

/// Status and JSON body of a reply, built on a blocking thread and turned into a response after.
pub type Reply = (StatusCode, serde_json::Value);

/// Turn a built reply (or the gateway-unavailable response) into a response.
pub fn respond(reply: Result<Reply, Response>) -> Response {
    match reply {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(response) => response,
    }
}

/// The one source of error JSON: `{"error": message}`.
pub fn error_reply(status: StatusCode, message: impl Into<String>) -> Reply {
    (status, serde_json::json!({ "error": message.into() }))
}

/// A JSON error response: `{"error": message}`.
pub fn fail(status: StatusCode, message: String) -> Response {
    respond(Ok(error_reply(status, message)))
}

/// 200 with `value` as JSON; a playback/library error is a 500 `{error}`; an unreachable
/// gateway is the response `gw` already built.
pub fn json_or_500<T: Serialize>(reply: Result<Result<T, String>, Response>) -> Response {
    match reply {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(message)) => fail(StatusCode::INTERNAL_SERVER_ERROR, message),
        Err(response) => response,
    }
}

/// Run `f` against a fresh gateway connection on a blocking thread, so the NNG round trips do
/// not hold up an async worker. 503 when the gateway cannot be reached.
#[allow(clippy::result_large_err)]
pub async fn gw<T, F>(state: &Arc<AppState>, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&GatewayClient) -> T + Send + 'static,
{
    let state = Arc::clone(state);
    match tokio::task::spawn_blocking(move || state.gateway_client().map(|client| f(&client))).await
    {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Gateway not available".to_string(),
        )),
        Err(e) => Err(fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// One playback command; a service error or a transport failure is an `Err` message.
pub fn send(client: &GatewayClient, command: &Command) -> Result<Option<ResponseData>, String> {
    match client.playback_command(command) {
        Ok(PlaybackResponse::Ok { data }) => Ok(data),
        Ok(PlaybackResponse::Err { message }) => Err(message),
        Err(e) => Err(e.to_string()),
    }
}

/// A track by hash (or prefix), asked of the library through the gateway.
fn gateway_get_track(
    gateway: &GatewayClient,
    hash: &ContentHash,
) -> Result<library_ipc_client::TrackInfo, library_ipc_client::ClientError> {
    use library_ipc_client::{ClientError, ProtocolError};
    match gateway.library_request(&LibraryRequest::GetTrack { hash: hash.clone() })? {
        LibraryResponse::Track(track) => Ok(*track),
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        _ => Err(ClientError::Protocol(ProtocolError::Internal {
            message: "Unexpected response to GetTrack".to_string(),
        })),
    }
}

/// The canonical hash of each of `hashes`: the one resolver for everything bound for playback
/// (ADR-026). Canonical hashes pass through without a lookup; the first hash the library cannot
/// resolve fails the lot with 422 `{error}` naming it, or 503 when the library cannot be
/// reached, before anything is sent.
pub fn resolve_for_playback(
    gateway: &GatewayClient,
    hashes: &[ContentHash],
) -> Result<Vec<CanonicalHash>, Reply> {
    library_ipc_client::resolve_full_hashes(hashes, |hash| gateway_get_track(gateway, hash))
        .map_err(|e| {
            let status = if e.is_transport() {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            error_reply(status, e.to_string())
        })
}

/// One library request; a library error answer is folded into `Err`.
pub fn library(
    client: &GatewayClient,
    request: &LibraryRequest,
) -> Result<LibraryResponse, String> {
    match client.library_request(request) {
        Ok(LibraryResponse::Error(e)) => Err(e.to_string()),
        Ok(response) => Ok(response),
        Err(e) => Err(e.to_string()),
    }
}
