//! MDMA Console - Web interface for package, inbox, and bandcamp management

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use clap::Parser;
use color_eyre::Result;
use futures::stream::Stream;
use gateway_client::{Command, GatewayClient};
use library_ipc_client::{ContentHash, LibraryClient, TrackInfo, TrackQuery};
use media_protocol::{Deck, ResponseData};
use nng::options::Options;
use source_protocol::{DownloadState, SourceRequest, SourceResponse};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

mod packages;
mod types;

use packages::{AvailableUpdate, InstalledPackage};
use types::PackageName;

// =============================================================================
// CLI
// =============================================================================

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Console - Web interface for music management"
)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "3000")]
    port: u16,

    /// Library IPC socket address
    #[arg(long, default_value = "ipc:///run/mdma/library.sock")]
    library_socket: String,

    /// Gateway address (routes source requests through the gateway)
    #[arg(
        long,
        env = "MDMA_GATEWAY",
        default_value = "ipc:///run/mdma/gateway.sock"
    )]
    gateway: String,

    /// Source name to use for bandcamp operations
    #[arg(long, default_value = "bandcamp")]
    source_name: String,

    /// Event socket address for SSE bridge
    #[arg(long, default_value = "tcp://mdma-909.local:5556")]
    event_socket: String,

    /// Root path of the music library on the filesystem
    #[arg(long, default_value = "/music")]
    music_root: std::path::PathBuf,
}

// =============================================================================
// App State
// =============================================================================

struct AppState {
    /// Cached list of installed packages
    packages: Mutex<Vec<InstalledPackage>>,
    /// Cached list of available updates
    updates: Mutex<Vec<AvailableUpdate>>,
    /// Library socket address
    library_socket: String,
    /// Gateway address
    gateway: String,
    /// Source name for bandcamp operations
    source_name: String,
    /// Broadcast channel for SSE events
    event_tx: broadcast::Sender<String>,
    /// Root path of the music library
    music_root: std::path::PathBuf,
}

impl AppState {
    fn new(
        library_socket: String,
        gateway: String,
        source_name: String,
        event_tx: broadcast::Sender<String>,
        music_root: std::path::PathBuf,
    ) -> Self {
        Self {
            packages: Mutex::new(Vec::new()),
            updates: Mutex::new(Vec::new()),
            library_socket,
            gateway,
            source_name,
            event_tx,
            music_root,
        }
    }

    /// Get library client (creates new connection each time)
    fn library_client(&self) -> Option<LibraryClient> {
        LibraryClient::connect(&self.library_socket).ok()
    }

    /// Get gateway client (creates new connection each time)
    fn gateway_client(&self) -> Option<GatewayClient> {
        GatewayClient::connect(&self.gateway).ok()
    }
}

// =============================================================================
// Templates
// =============================================================================

/// Package view for template (includes pre-computed update info)
struct PackageView {
    name: String,
    version: String,
    update_version: Option<String>,
}

impl PackageView {
    fn from_installed(pkg: &InstalledPackage, updates: &[AvailableUpdate]) -> Self {
        let update = updates.iter().find(|u| u.name == pkg.name);
        Self {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            update_version: update.map(|u| u.new_version.clone()),
        }
    }

    fn has_update(&self) -> bool {
        self.update_version.is_some()
    }
}

/// Bandcamp status view for template
#[derive(serde::Serialize)]
struct BandcampView {
    connected: bool,
    authenticated: bool,
    downloads_active: usize,
    downloads_queued: usize,
    downloads_completed: usize,
    downloads_failed: usize,
    paused: bool,
}

impl BandcampView {
    fn from_source_response(response: Option<SourceResponse>) -> Self {
        match response {
            Some(SourceResponse::Status(status)) => Self {
                connected: true,
                authenticated: status.authenticated,
                downloads_active: status.downloads_active,
                downloads_queued: status.downloads_queued,
                downloads_completed: status.downloads_completed,
                downloads_failed: status.downloads_failed,
                paused: status.paused,
            },
            None => Self {
                connected: false,
                authenticated: false,
                downloads_active: 0,
                downloads_queued: 0,
                downloads_completed: 0,
                downloads_failed: 0,
                paused: false,
            },
            Some(other) => {
                tracing::warn!(
                    variant = ?other,
                    "Unexpected source response variant in BandcampView"
                );
                Self {
                    connected: false,
                    authenticated: false,
                    downloads_active: 0,
                    downloads_queued: 0,
                    downloads_completed: 0,
                    downloads_failed: 0,
                    paused: false,
                }
            }
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    version: String,
    packages: Vec<PackageView>,
    inbox: Vec<String>,
    bandcamp: BandcampView,
}

// =============================================================================
// Handlers
// =============================================================================

async fn index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Refresh package list
    let installed = match packages::list_installed().await {
        Ok(pkgs) => {
            *state.packages.lock().await = pkgs.clone();
            pkgs
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list packages");
            state.packages.lock().await.clone()
        }
    };

    // Get inbox from library
    let inbox = if let Some(client) = state.library_client() {
        client
            .inbox_queue()
            .map(|paths| paths.into_iter().map(|p| p.to_string()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let updates = state.updates.lock().await.clone();

    // Build package views with pre-computed update info
    let packages: Vec<PackageView> = installed
        .iter()
        .map(|pkg| PackageView::from_installed(pkg, &updates))
        .collect();

    // Get bandcamp status via gateway
    let bandcamp_response = state.gateway_client().and_then(|client| {
        client
            .source_request(&state.source_name, &SourceRequest::GetStatus)
            .ok()
    });
    let bandcamp = BandcampView::from_source_response(bandcamp_response);

    let template = IndexTemplate {
        version: env!("CARGO_PKG_VERSION").to_string(),
        packages,
        inbox,
        bandcamp,
    };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Template render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
        }
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn check_updates(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match packages::check_updates().await {
        Ok(updates) => {
            let count = updates.len();
            *state.updates.lock().await = updates;
            tracing::info!(count, "Checked for updates");
            StatusCode::OK
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to check updates");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn update_package(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Validate package name
    let pkg = match PackageName::new(&name) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(name = %name, error = %e, "Invalid package name");
            return (StatusCode::BAD_REQUEST, format!("Invalid package: {}", e)).into_response();
        }
    };

    // Update the package
    if let Err(e) = packages::update_package(&pkg).await {
        tracing::error!(package = %pkg, error = %e, "Failed to update package");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Update failed: {}", e),
        )
            .into_response();
    }

    // Try to restart the corresponding service
    if let Some(svc) = packages::package_to_service(&pkg) {
        if let Err(e) = packages::restart_service(&svc).await {
            tracing::warn!(service = %svc, error = %e, "Failed to restart service");
            // Don't fail - package update succeeded
        }
    }

    // Clear cached updates
    state.updates.lock().await.clear();

    StatusCode::OK.into_response()
}

#[derive(serde::Serialize)]
struct IngestResultJson {
    path: String,
    success: bool,
    message: String,
    hash: Option<String>,
}

async fn ingest_all(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match state.library_client() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "Library service not available"})),
            )
                .into_response()
        }
    };

    match client.ingest_all() {
        Ok(results) => {
            let json_results: Vec<IngestResultJson> = results
                .into_iter()
                .map(|item| IngestResultJson {
                    path: item.path.to_string(),
                    success: item.result.success,
                    message: item.result.message,
                    hash: item.result.hash.map(|h| h.0),
                })
                .collect();

            Json(json_results).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// =============================================================================
// Shared helpers
// =============================================================================

#[allow(clippy::result_large_err)]
fn require_gateway(state: &AppState) -> Result<GatewayClient, axum::response::Response> {
    state.gateway_client().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Gateway not available"})),
        )
            .into_response()
    })
}

fn playback_success_or_error(
    client: &GatewayClient,
    command: &Command,
) -> axum::response::Response {
    match client.playback_command(command) {
        Ok(resp) if resp.success => Json(serde_json::json!({"success": true})).into_response(),
        Ok(resp) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": resp.error_message})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[allow(clippy::result_large_err)]
fn source_request_or_error(
    state: &AppState,
    request: &SourceRequest,
) -> Result<SourceResponse, axum::response::Response> {
    let client = state.gateway_client().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Gateway not available"})),
        )
            .into_response()
    })?;
    match client.source_request(&state.source_name, request) {
        Ok(SourceResponse::Error(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response()),
        Ok(resp) => Ok(resp),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response()),
    }
}

// =============================================================================
// Bandcamp Handlers
// =============================================================================

async fn bandcamp_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.gateway_client() {
        Some(client) => {
            match client.source_request(&state.source_name, &SourceRequest::GetStatus) {
                Ok(SourceResponse::Status(status)) => Json(BandcampView {
                    connected: true,
                    authenticated: status.authenticated,
                    downloads_active: status.downloads_active,
                    downloads_queued: status.downloads_queued,
                    downloads_completed: status.downloads_completed,
                    downloads_failed: status.downloads_failed,
                    paused: status.paused,
                })
                .into_response(),
                Ok(SourceResponse::Error(e)) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response(),
                Ok(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "Unexpected response from source"})),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        None => Json(BandcampView::from_source_response(None)).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct SyncRequest {
    // accepted for backward compatibility but ignored;
    // the source service handles authentication internally
    #[allow(dead_code)]
    username: Option<String>,
}

async fn bandcamp_sync(
    State(state): State<Arc<AppState>>,
    Json(_req): Json<SyncRequest>,
) -> impl IntoResponse {
    match source_request_or_error(&state, &SourceRequest::Sync) {
        Ok(SourceResponse::SyncStarted {
            total_items,
            new_items,
        }) => Json(serde_json::json!({
            "success": true,
            "total_items": total_items,
            "new_items": new_items
        }))
        .into_response(),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Unexpected response from source"})),
        )
            .into_response(),
        Err(e) => e,
    }
}

#[derive(serde::Serialize)]
struct DownloadJson {
    id: String,
    artist: String,
    title: String,
    state: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
}

async fn bandcamp_downloads(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match source_request_or_error(&state, &SourceRequest::ListDownloads) {
        Ok(SourceResponse::Downloads(downloads)) => {
            let json_downloads: Vec<DownloadJson> = downloads
                .into_iter()
                .map(|d| {
                    let error = match &d.state {
                        DownloadState::Failed { message } => Some(message.clone()),
                        _ => None,
                    };
                    DownloadJson {
                        id: d.id,
                        artist: d.artist,
                        title: d.title,
                        state: d.state.to_string(),
                        downloaded_bytes: d.downloaded_bytes,
                        total_bytes: d.total_bytes,
                        error,
                    }
                })
                .collect();
            Json(json_downloads).into_response()
        }
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Unexpected response from source"})),
        )
            .into_response(),
        Err(e) => e,
    }
}

async fn bandcamp_pause(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match source_request_or_error(&state, &SourceRequest::PauseAll) {
        Ok(SourceResponse::Paused) => Json(serde_json::json!({"success": true})).into_response(),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Unexpected response from source"})),
        )
            .into_response(),
        Err(e) => e,
    }
}

async fn bandcamp_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match source_request_or_error(&state, &SourceRequest::ResumeAll) {
        Ok(SourceResponse::Resumed) => Json(serde_json::json!({"success": true})).into_response(),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Unexpected response from source"})),
        )
            .into_response(),
        Err(e) => e,
    }
}

// =============================================================================
// Player Types
// =============================================================================

#[derive(serde::Serialize)]
struct TrackInfoJson {
    content_hash: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    duration: Option<u32>,
    bpm: Option<f32>,
    key: Option<String>,
}

impl TrackInfoJson {
    fn from_track_info(t: &TrackInfo) -> Self {
        Self {
            content_hash: t.content_hash.0.clone(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration: t.duration.map(|d| d.0),
            bpm: t.bpm.map(|b| b.as_f32()),
            key: t.key.map(|k| k.to_string()),
        }
    }
}

// =============================================================================
// Player Handlers
// =============================================================================

async fn player_now_playing(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    match client.playback_command(&Command::NowPlaying) {
        Ok(resp) => match resp.data {
            Some(ResponseData::NowPlaying(Some(hash))) => {
                let track = state
                    .library_client()
                    .and_then(|lib| lib.get_track(&hash).ok());
                match track {
                    Some(t) => Json(serde_json::json!({
                        "playing": true,
                        "track": TrackInfoJson::from_track_info(&t)
                    }))
                    .into_response(),
                    None => Json(serde_json::json!({
                        "playing": true,
                        "track": { "content_hash": hash.0 }
                    }))
                    .into_response(),
                }
            }
            Some(ResponseData::NowPlaying(None)) | None => {
                Json(serde_json::json!({"playing": false})).into_response()
            }
            _ => Json(serde_json::json!({"playing": false})).into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn player_queue(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    match client.playback_command(&Command::QueueList) {
        Ok(resp) => match resp.data {
            Some(ResponseData::Queue(hashes)) => {
                let tracks: Vec<serde_json::Value> = hashes
                    .iter()
                    .map(|hash| {
                        let track = state
                            .library_client()
                            .and_then(|lib| lib.get_track(hash).ok());
                        match track {
                            Some(t) => serde_json::to_value(TrackInfoJson::from_track_info(&t))
                                .unwrap_or(serde_json::json!({"content_hash": hash.0})),
                            None => serde_json::json!({"content_hash": hash.0}),
                        }
                    })
                    .collect();
                Json(tracks).into_response()
            }
            _ => Json(serde_json::json!([])).into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct QueueAppendRequest {
    hash: String,
}

async fn player_queue_append(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QueueAppendRequest>,
) -> impl IntoResponse {
    let hash = ContentHash(req.hash);

    // Resolve the blob path via library
    let lib_client = match state.library_client() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "Library not available"})),
            )
                .into_response()
        }
    };

    let track = match lib_client.get_track(&hash) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let rel = match &track.blob_path {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "Track has no blob path"})),
            )
                .into_response()
        }
    };

    let path = state.music_root.join(&rel);

    let gw = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&gw, &Command::QueueAppend { hash, path })
}

#[derive(serde::Deserialize)]
struct QueueRemoveRequest {
    hashes: Vec<String>,
}

async fn player_queue_remove(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QueueRemoveRequest>,
) -> impl IntoResponse {
    let hashes: Vec<ContentHash> = req.hashes.into_iter().map(ContentHash).collect();

    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::QueueRemove { hashes })
}

async fn player_queue_clear(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::QueueClear)
}

async fn player_play_queue(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::PlayQueue)
}

async fn player_stop(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::Stop { deck: Deck::A })
}

/// Skip: play the next item in the queue (PlayQueue pops head and starts playing)
async fn player_skip(state: State<Arc<AppState>>) -> impl IntoResponse {
    player_play_queue(state).await
}

async fn player_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_tx.subscribe();
    let stream = async_stream::stream! {
        yield Ok(Event::default().data(r#"{"type":"Connected"}"#));
        loop {
            match rx.recv().await {
                Ok(json) => yield Ok(Event::default().data(json)),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "SSE client lagged");
                    yield Ok(Event::default().data(format!(r#"{{"type":"Lagged","skipped":{n}}}"#)));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// =============================================================================
// Library Search Handler
// =============================================================================

#[derive(serde::Deserialize)]
struct LibrarySearchParams {
    q: Option<String>,
    artist: Option<String>,
    bpm: Option<String>,
    key: Option<String>,
}

async fn library_search_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LibrarySearchParams>,
) -> impl IntoResponse {
    let mut query = TrackQuery::default();

    if let Some(q) = params.q.filter(|s| !s.is_empty()) {
        query.any_text = Some(library_search::parse_string_query(&q));
    }
    if let Some(artist) = params.artist.filter(|s| !s.is_empty()) {
        query.artist = Some(library_search::parse_string_query(&artist));
    }
    if let Some(bpm) = params.bpm.filter(|s| !s.is_empty()) {
        match library_search::parse_numeric_query(&bpm) {
            Ok(nq) => query.bpm = Some(nq),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response()
            }
        }
    }
    if let Some(key) = params.key.filter(|s| !s.is_empty()) {
        match library_search::parse_key_query(&key) {
            Ok(kq) => query.key = Some(kq),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response()
            }
        }
    }

    if query.is_empty() {
        return Json(serde_json::json!([])).into_response();
    }

    // Use the gateway client to route library search through the gateway
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    use gateway_client::{LibraryRequest, LibraryResponse};
    match client.library_request(&LibraryRequest::Search { query }) {
        Ok(LibraryResponse::SearchResults(tracks)) => {
            let json_tracks: Vec<TrackInfoJson> =
                tracks.iter().map(TrackInfoJson::from_track_info).collect();
            Json(json_tracks).into_response()
        }
        Ok(LibraryResponse::Error(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Unexpected library response"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// =============================================================================
// Event Bridge
// =============================================================================

fn spawn_event_bridge(event_socket: String, event_tx: broadcast::Sender<String>) {
    use event_protocol::{from_topic_message, TOPIC_PLAYBACK};
    std::thread::spawn(move || {
        let sub = match nng::Socket::new(nng::Protocol::Sub0) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "SSE: failed to create Sub0 socket");
                return;
            }
        };
        if let Err(e) = sub.set_opt::<nng::options::protocol::pubsub::Subscribe>(
            TOPIC_PLAYBACK.as_bytes().to_vec(),
        ) {
            tracing::error!(error = %e, "SSE: subscribe failed");
            return;
        }
        loop {
            let resolved = match nng_transport::resolve_tcp_hostname(&event_socket) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(error = %e, addr = %event_socket, "SSE: resolve failed, retrying in 5s");
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };
            if let Err(e) = sub.dial(&resolved) {
                tracing::error!(addr = %resolved, error = %e, "SSE: dial failed, retrying in 5s");
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
            tracing::info!(address = %event_socket, "Event bridge connected");
            loop {
                match sub.recv() {
                    Ok(msg) => {
                        if let Ok((_topic, event)) = from_topic_message(msg.as_slice()) {
                            if let Ok(json) = serde_json::to_string(&event) {
                                let _ = event_tx.send(json);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SSE recv error, reconnecting in 5s");
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        break;
                    }
                }
            }
        }
    });
}

// =============================================================================
// Main
// =============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_console=info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();

    let (event_tx, _) = broadcast::channel::<String>(64);
    spawn_event_bridge(args.event_socket, event_tx.clone());

    let state = Arc::new(AppState::new(
        args.library_socket,
        args.gateway,
        args.source_name,
        event_tx,
        args.music_root,
    ));

    // Initial package list load
    if let Ok(pkgs) = packages::list_installed().await {
        *state.packages.lock().await = pkgs;
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/check-updates", post(check_updates))
        .route("/update/{name}", post(update_package))
        .route("/ingest-all", post(ingest_all))
        // Bandcamp routes
        .route("/bandcamp/status", get(bandcamp_status))
        .route("/bandcamp/sync", post(bandcamp_sync))
        .route("/bandcamp/downloads", get(bandcamp_downloads))
        .route("/bandcamp/pause", post(bandcamp_pause))
        .route("/bandcamp/resume", post(bandcamp_resume))
        // Player routes
        .route("/player/now-playing", get(player_now_playing))
        .route("/player/queue", get(player_queue))
        .route("/player/queue/append", post(player_queue_append))
        .route("/player/queue/remove", post(player_queue_remove))
        .route("/player/queue/clear", post(player_queue_clear))
        .route("/player/play-queue", post(player_play_queue))
        .route("/player/stop", post(player_stop))
        .route("/player/skip", post(player_skip))
        .route("/player/events", get(player_events))
        // Library search
        .route("/library/search", get(library_search_handler))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("MDMA Console listening on http://0.0.0.0:{}", args.port);

    axum::serve(listener, app).await?;

    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::{Bpm, ContentHash, DurationSeconds, Key};
    use source_protocol::{SourceResponse, SourceStatus};

    fn make_source_status() -> SourceStatus {
        SourceStatus {
            name: "bandcamp".to_string(),
            version: "0.1.0".to_string(),
            authenticated: true,
            downloads_active: 2,
            downloads_queued: 3,
            downloads_completed: 10,
            downloads_failed: 1,
            uptime_seconds: 3600,
            paused: false,
        }
    }

    #[test]
    fn bandcamp_view_from_none_is_disconnected() {
        let view = BandcampView::from_source_response(None);
        assert!(!view.connected);
        assert!(!view.authenticated);
        assert_eq!(view.downloads_active, 0);
        assert_eq!(view.downloads_queued, 0);
        assert_eq!(view.downloads_completed, 0);
        assert_eq!(view.downloads_failed, 0);
        assert!(!view.paused);
    }

    #[test]
    fn bandcamp_view_from_status_maps_correctly() {
        let status = make_source_status();
        let view = BandcampView::from_source_response(Some(SourceResponse::Status(status)));
        assert!(view.connected);
        assert!(view.authenticated);
        assert_eq!(view.downloads_active, 2);
        assert_eq!(view.downloads_queued, 3);
        assert_eq!(view.downloads_completed, 10);
        assert_eq!(view.downloads_failed, 1);
        assert!(!view.paused);
    }

    #[test]
    fn bandcamp_view_from_unexpected_variant_is_disconnected() {
        let view = BandcampView::from_source_response(Some(SourceResponse::Pong));
        assert!(!view.connected);
        assert!(!view.authenticated);
    }

    #[test]
    fn track_info_json_from_full_track() {
        let track = TrackInfo {
            content_hash: ContentHash("sha256:abc123".to_string()),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            duration: Some(DurationSeconds(180)),
            bpm: Some(Bpm::from_u32(128).unwrap()),
            key: Some(Key::from_traditional("C Major").unwrap()),
            blob_path: Some("ab/abc123.flac".to_string()),
        };
        let json = TrackInfoJson::from_track_info(&track);
        assert_eq!(json.content_hash, "sha256:abc123");
        assert_eq!(json.title, Some("Test Track".to_string()));
        assert_eq!(json.artist, Some("Test Artist".to_string()));
        assert_eq!(json.album, Some("Test Album".to_string()));
        assert_eq!(json.duration, Some(180));
        assert!(json.bpm.is_some());
        assert!(json.key.is_some());
    }

    #[test]
    fn track_info_json_from_minimal_track() {
        let track = TrackInfo {
            content_hash: ContentHash("sha256:deadbeef".to_string()),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
        };
        let json = TrackInfoJson::from_track_info(&track);
        assert_eq!(json.content_hash, "sha256:deadbeef");
        assert!(json.title.is_none());
        assert!(json.artist.is_none());
        assert!(json.album.is_none());
        assert!(json.duration.is_none());
        assert!(json.bpm.is_none());
        assert!(json.key.is_none());
    }

    #[test]
    fn package_view_without_update() {
        let pkg = InstalledPackage {
            name: "mdma-console".to_string(),
            version: "0.1.0".to_string(),
        };
        let view = PackageView::from_installed(&pkg, &[]);
        assert_eq!(view.name, "mdma-console");
        assert_eq!(view.version, "0.1.0");
        assert!(view.update_version.is_none());
        assert!(!view.has_update());
    }

    #[test]
    fn package_view_with_update() {
        let pkg = InstalledPackage {
            name: "mdma-console".to_string(),
            version: "0.1.0".to_string(),
        };
        let updates = vec![AvailableUpdate {
            name: "mdma-console".to_string(),
            current_version: "0.1.0".to_string(),
            new_version: "0.2.0".to_string(),
        }];
        let view = PackageView::from_installed(&pkg, &updates);
        assert_eq!(view.update_version, Some("0.2.0".to_string()));
        assert!(view.has_update());
    }
}
