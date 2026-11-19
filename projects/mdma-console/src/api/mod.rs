//! JSON API for the new web UI. Each submodule registers its own routes here.

use crate::AppState;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use gateway_client::Response as PlaybackResponse;
use gateway_client::{Command, GatewayClient, LibraryRequest, LibraryResponse};
use library_ipc_client::{CanonicalHash, ClientError, ContentHash, ProtocolError, ResolveError};
use mdma_client::{map_gw_to_lib_error, LibraryBackend};
use media_protocol::{ResponseData, SourceName};
use serde::Serialize;
use std::sync::Arc;

pub mod bin;
pub mod energy;
pub mod library;
pub mod marks;
pub mod player;
pub mod playlists;
pub mod system;
pub mod timeline;

/// All `/api/...` routes.
///
/// The rule: nothing static may sit directly under `/api/playlists/`, because `:name` there is a
/// user's playlist name and a static segment would shadow the playlist of that name. Routes
/// about no one playlist live elsewhere (`/api/bandcamp/...`, `/api/bin`); routes about one
/// playlist are `/api/playlists/:name/...`.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/tracks", get(library::tracks))
        .route("/api/keys", get(library::keys))
        .route("/api/tracks/match", get(library::tracks_match))
        .route("/api/tracks/:hash/timeline", get(timeline::timeline))
        .route("/api/library/match", get(library::library_match))
        .route("/api/marks", post(marks::marks))
        .route("/api/marks/config", get(marks::config))
        .route("/api/energy", post(energy::energy))
        .route(
            "/api/playlists",
            get(library::playlists).post(playlists::create),
        )
        .route(
            "/api/playlists/:name",
            get(library::playlist).delete(playlists::delete),
        )
        .route(
            "/api/bandcamp/playlists",
            get(playlists::bandcamp_playlists),
        )
        .route("/api/bandcamp/playlist-links", get(playlists::links))
        .route("/api/bandcamp/playlist-sync", get(playlists::sync_job))
        .route(
            "/api/playlists/:name/link",
            post(playlists::link).delete(playlists::unlink),
        )
        .route("/api/playlists/:name/sync", post(playlists::sync))
        .route("/api/inbox", get(system::inbox))
        .route("/api/bin", get(bin::list).delete(bin::empty))
        .route("/api/bin/:id", delete(bin::purge))
        .route("/api/bin/:id/restore", post(bin::restore))
        .route("/api/playlists/:name/edit", post(playlists::edit))
        .route("/api/playlists/:name/revert", post(playlists::revert))
        .route("/api/playlists/:name/queue", post(player::playlist_queue))
        .route("/api/player", get(player::player))
        .route("/api/pause", post(player::pause))
        .route("/api/resume", post(player::resume))
        .route("/api/preview", post(player::preview))
        .route("/api/preview/back", post(player::preview_back))
        .route("/api/seek", post(player::seek))
        .route("/api/queue", post(player::queue))
        .route("/api/queue/edit", post(player::queue_edit))
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

/// 503 `{error: "Gateway not available"}`: the one reply for a gateway that cannot be reached.
pub fn gateway_unavailable() -> Reply {
    error_reply(StatusCode::SERVICE_UNAVAILABLE, "Gateway not available")
}

/// Why a playback command did not give its answer.
#[derive(Debug)]
pub enum PlaybackFailure {
    /// The gateway or playback service could not be reached, or its answer was unreadable.
    Unreachable(gateway_client::ClientError),
    /// The playback service refused the command.
    Refused(String),
    /// The gateway answered with an error, or the answer was not the one asked for (an old node).
    Unexpected(String),
}

impl std::fmt::Display for PlaybackFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaybackFailure::Unreachable(e) => write!(f, "{e}"),
            PlaybackFailure::Refused(message) | PlaybackFailure::Unexpected(message) => {
                f.write_str(message)
            }
        }
    }
}

impl From<gateway_client::ClientError> for PlaybackFailure {
    fn from(e: gateway_client::ClientError) -> Self {
        match e {
            gateway_client::ClientError::Service(message) => PlaybackFailure::Unexpected(message),
            gateway_client::ClientError::Refused(message) => PlaybackFailure::Refused(message),
            other => PlaybackFailure::Unreachable(other),
        }
    }
}

/// The one policy for a playback command that failed, shared by every route:
/// - the playback service cannot be reached: 503;
/// - it refused the command, or answered something unexpected: 502.
pub fn playback_failure_status(failure: &PlaybackFailure) -> StatusCode {
    match failure {
        PlaybackFailure::Unreachable(e) => gateway_failure_status(e),
        PlaybackFailure::Refused(_) | PlaybackFailure::Unexpected(_) => StatusCode::BAD_GATEWAY,
    }
}

/// The one policy for a gateway call that failed in transit, whichever service it was for: the
/// gateway answered with an error or refused: 502; it cannot be reached or its answer is
/// unreadable: 503.
pub fn gateway_failure_status(error: &gateway_client::ClientError) -> StatusCode {
    use gateway_client::ClientError as E;
    match error {
        E::Service(_) | E::Refused(_) => StatusCode::BAD_GATEWAY,
        E::Connection(_) | E::Nng(_) | E::Serialization(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// The one policy for a source that refused a request, shared by every source route:
/// - not authenticated: 401; a job that is running, or a playlist that is not linked: 409;
/// - rate limited: 429; no such item: 404;
/// - a request the source cannot use (`InvalidRequest`): 422;
/// - a source that failed (sync, download, check, internal) or does not support the request: 502.
pub fn source_failure_status(error: &source_protocol::SourceError) -> StatusCode {
    use source_protocol::SourceError as E;
    match error {
        E::NotAuthenticated { .. } => StatusCode::UNAUTHORIZED,
        E::ImportInProgress { .. }
        | E::CheckInProgress { .. }
        | E::SyncInProgress { .. }
        | E::PlaylistNotLinked { .. } => StatusCode::CONFLICT,
        E::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        E::ItemNotFound { .. } => StatusCode::NOT_FOUND,
        E::InvalidRequest { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        E::SyncFailed { .. }
        | E::DownloadFailed { .. }
        | E::Internal { .. }
        | E::CheckFailed { .. }
        | E::Unsupported { .. } => StatusCode::BAD_GATEWAY,
    }
}

/// A playback or library call that failed; each is classified by its own policy.
#[derive(Debug)]
pub enum Failure {
    Playback(PlaybackFailure),
    Library(ClientError),
}

impl From<PlaybackFailure> for Failure {
    fn from(f: PlaybackFailure) -> Self {
        Failure::Playback(f)
    }
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Self {
        Failure::Library(e)
    }
}

impl Failure {
    /// The status and `{error}` body this failure answers with.
    pub fn reply(&self) -> Reply {
        match self {
            Failure::Playback(f) => playback_failed(f),
            Failure::Library(e) => library_failed(e),
        }
    }
}

/// The reply for a playback command that failed ([`playback_failure_status`]).
pub fn playback_failed(failure: &PlaybackFailure) -> Reply {
    error_reply(playback_failure_status(failure), failure.to_string())
}

/// The reply for a library call that failed ([`library_failure_status`]).
pub fn library_failed(error: &ClientError) -> Reply {
    error_reply(library_failure_status(error), error.to_string())
}

/// A failed call as a response, for the handlers that answer with a `Response`.
pub fn failure_response(failure: impl Into<Failure>) -> Response {
    reply_response(failure.into().reply())
}

/// 200 with `value` as JSON; a failed playback or library call is answered by its classifier
/// ([`Failure::reply`]); an unreachable gateway is the response `gw` already built.
pub fn json_or_failure<T: Serialize, E: Into<Failure>>(
    reply: Result<Result<T, E>, Response>,
) -> Response {
    match reply {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(failure)) => failure_response(failure),
        Err(response) => response,
    }
}

/// Connect with `connect` and run `f` against the connection, on a blocking thread so the NNG
/// round trips do not hold up an async worker. A connection that cannot be made is the reply
/// `connect` gives.
#[allow(clippy::result_large_err)]
async fn blocking<C: 'static, T, F>(
    state: &Arc<AppState>,
    connect: fn(&AppState) -> Result<C, Reply>,
    f: F,
) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&C) -> T + Send + 'static,
{
    let state = Arc::clone(state);
    match tokio::task::spawn_blocking(move || connect(&state).map(|c| f(&c))).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(reply)) => Err(reply_response(reply)),
        Err(e) => Err(fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// Run `f` against a fresh gateway connection. 503 when the gateway cannot be reached.
#[allow(clippy::result_large_err)]
pub async fn gw<T, F>(state: &Arc<AppState>, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&GatewayClient) -> T + Send + 'static,
{
    blocking(
        state,
        |s| s.gateway_client().ok_or_else(gateway_unavailable),
        f,
    )
    .await
}

/// Run `f` against the library as [`library_backend`] reaches it. 503 when the gateway cannot be
/// reached.
#[allow(clippy::result_large_err)]
pub async fn lib<T, F>(state: &Arc<AppState>, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&LibraryBackend) -> T + Send + 'static,
{
    blocking(state, library_backend, f).await
}

/// One playback command; the playback service's answer, or why there is none.
pub fn send(
    client: &GatewayClient,
    command: &Command,
) -> Result<Option<ResponseData>, PlaybackFailure> {
    match client.playback_command(command)? {
        PlaybackResponse::Ok { data } => Ok(data),
        PlaybackResponse::Err { message } => Err(PlaybackFailure::Refused(message)),
    }
}

/// A reply as a response (for the handlers that answer with a `Response`).
pub fn reply_response(reply: Reply) -> Response {
    respond(Ok(reply))
}

/// The library, reached through the gateway by mdma_client's one gateway path (so a gateway that
/// rejects a request maps the same here as in the CLI). A gateway that cannot be reached is a 503.
pub fn library_backend(state: &AppState) -> Result<LibraryBackend, Reply> {
    LibraryBackend::connect_gateway(&state.gateway).map_err(|_| gateway_unavailable())
}

/// The one policy for a library call that failed, shared by every route:
/// - transport (the gateway or library cannot be reached): 503;
/// - no such playlist or bin entry: 404;
/// - a playlist that already exists: 409;
/// - input the library refused (an unknown or ambiguous hash, ...): 422;
/// - anything else (`InvalidRequest` from an old node, `Internal`): 502.
///
/// A gateway's "library service unreachable" still arrives as `Internal`, so it is a 502 rather
/// than a 503 until it is typed (#100).
pub fn library_failure_status(error: &ClientError) -> StatusCode {
    match error {
        ClientError::Transport(_) => StatusCode::SERVICE_UNAVAILABLE,
        ClientError::Protocol(
            ProtocolError::PlaylistNotFound { .. } | ProtocolError::BinEntryNotFound { .. },
        ) => StatusCode::NOT_FOUND,
        ClientError::Protocol(
            ProtocolError::AmbiguousHash { .. }
            | ProtocolError::HashNotCanonical { .. }
            | ProtocolError::TrackNotFound { .. }
            | ProtocolError::InboxFileNotFound { .. }
            | ProtocolError::InvalidPlaylistName { .. }
            | ProtocolError::InvalidEdit { .. },
        ) => StatusCode::UNPROCESSABLE_ENTITY,
        ClientError::Protocol(ProtocolError::PlaylistAlreadyExists { .. }) => StatusCode::CONFLICT,
        ClientError::Protocol(
            ProtocolError::IngestionFailed { .. }
            | ProtocolError::Internal { .. }
            | ProtocolError::InvalidRequest { .. },
        ) => StatusCode::BAD_GATEWAY,
    }
}

/// The reply for a hash that did not resolve: a library lookup that failed is classified by
/// [`library_failure_status`]; any other refusal (not in the queue, ambiguous in or across the
/// queue and library, not full) is a 422.
pub fn resolve_failed(e: ResolveError) -> Reply {
    let status = match &e {
        ResolveError::Lookup { source, .. } => library_failure_status(source),
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    error_reply(status, e.to_string())
}

/// The hashes of a request as given, or the 400 response for none.
#[allow(clippy::result_large_err)]
pub fn parse_hashes(hashes: Vec<String>) -> Result<Vec<ContentHash>, Response> {
    if hashes.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, "no hashes given".to_string()));
    }
    Ok(hashes.into_iter().map(ContentHash::new).collect())
}

/// The canonical hash of each of `hashes`, for adding to the queue: the one resolver for
/// everything bound for playback (ADR-026). Every hash is looked up in the library, full ones
/// too; the first it cannot resolve fails the lot with `{error}` naming it ([`resolve_failed`]),
/// before anything is sent.
pub fn resolve_for_playback(
    state: &AppState,
    hashes: &[ContentHash],
) -> Result<Vec<CanonicalHash>, Reply> {
    library_backend(state)?
        .resolve_for_playback(hashes)
        .map_err(resolve_failed)
}

/// Like [`resolve_for_playback`], for making `hashes` the whole `queue` (an edit or a replace):
/// entries the queue holds stay even when their track has left the library, new ones must exist
/// in it.
pub fn resolve_for_queue_replace(
    state: &AppState,
    queue: &[ContentHash],
    hashes: &[ContentHash],
) -> Result<Vec<CanonicalHash>, Reply> {
    library_backend(state)?
        .resolve_for_queue_replace(queue, hashes)
        .map_err(resolve_failed)
}

/// Queue entries for resolved `hashes`, all from the audio source.
pub fn queue_entries(hashes: Vec<CanonicalHash>) -> Vec<(ContentHash, SourceName)> {
    hashes
        .into_iter()
        .map(|h| (h.into(), SourceName::audio()))
        .collect()
}

/// One library request, mapped as mdma_client maps it everywhere: the library's own error
/// answer is a protocol error, a gateway that rejects the request is one too, and only a failed
/// transport is a transport error.
pub fn library(
    client: &GatewayClient,
    request: &LibraryRequest,
) -> Result<LibraryResponse, ClientError> {
    match client
        .library_request(request)
        .map_err(map_gw_to_lib_error)?
    {
        LibraryResponse::Error(e) => Err(ClientError::Protocol(e)),
        response => Ok(response),
    }
}

/// How a write's answer becomes a reply: `None` for an answer that is not one of the write's
/// outcomes.
pub type Interpret = fn(LibraryResponse) -> Option<Reply>;

/// Send `request` and say what the library answered, as `interpret` reads it. Any other answer
/// is a 502, as is every library failure ([`library_failed`]).
pub fn written(client: &GatewayClient, request: &LibraryRequest, interpret: Interpret) -> Reply {
    match library(client, request) {
        Ok(response) => {
            interpret(response).unwrap_or_else(|| library_failed(&unexpected_library_response()))
        }
        Err(e) => library_failed(&e),
    }
}

/// Send `request` over one gateway connection and answer as `interpret` reads the library's
/// answer. 503 when the gateway cannot be reached.
pub async fn forward(
    state: &Arc<AppState>,
    request: LibraryRequest,
    interpret: Interpret,
) -> Response {
    respond(gw(state, move |client| written(client, &request, interpret)).await)
}

/// The library answered with something other than the answer to the request.
pub fn unexpected_library_response() -> ClientError {
    ClientError::Protocol(ProtocolError::Internal {
        message: "Unexpected library response".to_string(),
    })
}
