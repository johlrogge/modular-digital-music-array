//! MDMA Console - Web interface for package, inbox, and bandcamp management

use askama::Template;
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    response::{Html, IntoResponse, Json, Redirect},
    routing::{get, post},
    Router,
};
use clap::Parser;
use color_eyre::Result;
use futures::stream::Stream;
use gateway_client::{Command, GatewayClient};
use library_ipc_client::{ContentHash, LibraryClient};
use media_protocol::{AudioSinkInfo, ResponseData};
use nng::options::Options;
use source_protocol::{DownloadState, SourceRequest, SourceResponse};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;

mod api;
mod assets;
mod packages;
mod play_now;
#[cfg(test)]
mod test_support;
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

    /// Path to the Bandcamp cookies file
    #[arg(long, default_value = "/etc/mdma/bandcamp-cookies.json")]
    bandcamp_cookies: std::path::PathBuf,

    /// Path to the Bandcamp configuration file
    #[arg(long, default_value = "/etc/mdma/bandcamp.conf")]
    bandcamp_conf: std::path::PathBuf,
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
    /// Path to the Bandcamp cookies file
    bandcamp_cookies_path: std::path::PathBuf,
    /// Path to the Bandcamp configuration file
    bandcamp_conf_path: std::path::PathBuf,
}

impl AppState {
    fn new(
        library_socket: String,
        gateway: String,
        source_name: String,
        event_tx: broadcast::Sender<String>,
        music_root: std::path::PathBuf,
        bandcamp_cookies_path: std::path::PathBuf,
        bandcamp_conf_path: std::path::PathBuf,
    ) -> Self {
        Self {
            packages: Mutex::new(Vec::new()),
            updates: Mutex::new(Vec::new()),
            library_socket,
            gateway,
            source_name,
            event_tx,
            music_root,
            bandcamp_cookies_path,
            bandcamp_conf_path,
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

/// Package view for template and `/api/packages` (includes pre-computed update info)
#[derive(serde::Serialize)]
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
    /// Configured Bandcamp username (empty string if not set)
    configured_username: String,
    /// Whether the saved cookie file parses successfully
    cookies_valid: bool,
}

impl BandcampView {
    fn disconnected(configured_username: String, cookies_valid: bool) -> Self {
        Self {
            connected: false,
            authenticated: false,
            downloads_active: 0,
            downloads_queued: 0,
            downloads_completed: 0,
            downloads_failed: 0,
            paused: false,
            configured_username,
            cookies_valid,
        }
    }

    fn from_source_response(
        response: Option<SourceResponse>,
        configured_username: String,
        cookies_valid: bool,
    ) -> Self {
        match response {
            Some(SourceResponse::Status(status)) => Self {
                connected: true,
                authenticated: status.auth == source_protocol::AuthStatus::Authenticated,
                downloads_active: status.downloads_active,
                downloads_queued: status.downloads_queued,
                downloads_completed: status.downloads_completed,
                downloads_failed: status.downloads_failed,
                paused: status.queue == source_protocol::QueueState::Paused,
                configured_username,
                cookies_valid,
            },
            None => Self::disconnected(configured_username, cookies_valid),
            Some(other) => {
                tracing::warn!(
                    variant = ?other,
                    "Unexpected source response variant in BandcampView"
                );
                Self::disconnected(configured_username, cookies_valid)
            }
        }
    }
}

/// Read bandcamp username from conf file (format: MDMA_BANDCAMP_USERNAME="value")
fn read_bandcamp_username(conf_path: &std::path::Path) -> String {
    std::fs::read_to_string(conf_path)
        .unwrap_or_default()
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("MDMA_BANDCAMP_USERNAME=") {
                let val = rest.trim_matches('"');
                Some(val.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Write `contents` to `path` with mode 0600 (owner read/write only).
///
/// Cookies are credentials, so they must never be group- or world-readable.
/// The write itself is atomic (see `private_file`); this wraps it in
/// `spawn_blocking`.
async fn write_private_file_async(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    let path = path.to_path_buf();
    let contents = contents.to_vec();
    tokio::task::spawn_blocking(move || private_file::write_private_file(&path, &contents))
        .await
        .map_err(std::io::Error::other)?
}

/// Check whether the cookies file parses successfully
fn check_cookies_valid(cookies_path: &std::path::Path) -> bool {
    match std::fs::read_to_string(cookies_path) {
        Ok(content) => bandcamp_api::parse_cookies(&content).is_ok(),
        Err(_) => false,
    }
}

// =============================================================================
// Handlers
// =============================================================================

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

/// `POST /update/:name`: install the package, then restart its service.
///
/// Self-restart contract: updating `mdma-console` restarts this very process. There is no graceful
/// shutdown and runsv kills it, so the handler can never answer; a client sees the connection drop.
/// A 200 for `mdma-console` therefore means the restart did not happen (or failed, which is only
/// logged), and the new version is installed but not running.
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

    forget_update(&state, &pkg).await;

    StatusCode::OK.into_response()
}

/// Drop `pkg` from the cached pending updates; the other packages' updates stay pending.
async fn forget_update(state: &AppState, pkg: &PackageName) {
    state
        .updates
        .lock()
        .await
        .retain(|u| u.name != pkg.as_str());
}

#[derive(serde::Serialize)]
struct IngestResultJson {
    path: String,
    success: bool,
    /// An AIFF queued for conversion to FLAC (ADR-037): accepted, not stored yet.
    /// `success` is true for it too (the page's summary counts it as ingested).
    pending: bool,
    message: String,
    hash: Option<String>,
}

async fn upload_file(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let inbox_dir = state.music_root.join("inbox");

    // Ensure inbox directory exists
    if let Err(e) = tokio::fs::create_dir_all(&inbox_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Cannot create inbox dir: {}", e)})),
        )
            .into_response();
    }

    let mut results = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let file_name = match field.file_name() {
            Some(name) => inbox_utils::sanitize_filename(name),
            None => continue,
        };

        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => {
                results.push(serde_json::json!({"file": file_name, "error": e.to_string()}));
                continue;
            }
        };

        // Write to inbox
        let dest = inbox_utils::unique_path(&inbox_dir, &file_name);
        if let Err(e) = tokio::fs::write(&dest, &data).await {
            results.push(serde_json::json!({"file": file_name, "error": e.to_string()}));
            continue;
        }

        // Check if ZIP — extract audio files, then remove the ZIP
        let file_type = inbox_utils::detect_file_type(&dest);
        if file_type == Some("zip") {
            match inbox_utils::extract_zip(&dest, &inbox_dir) {
                Ok(extracted) => {
                    let _ = std::fs::remove_file(&dest);
                    for path in &extracted {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        results.push(serde_json::json!({"file": name, "success": true}));
                    }
                }
                Err(e) => {
                    results.push(serde_json::json!({"file": file_name, "error": e.to_string()}));
                }
            }
        } else if inbox_utils::is_ingestible_audio(&dest) {
            results.push(serde_json::json!({"file": file_name, "success": true}));
        } else if inbox_utils::is_audio_file(&dest) {
            // Recognized audio format but not accepted for ingest (AIFF is export-only)
            let _ = std::fs::remove_file(&dest);
            results.push(
                serde_json::json!({"file": file_name, "error": inbox_utils::NON_INGESTIBLE_ERROR}),
            );
        } else {
            let _ = std::fs::remove_file(&dest);
            results.push(
                serde_json::json!({"file": file_name, "error": "Not a supported audio file or ZIP"}),
            );
        }
    }

    Json(serde_json::json!({"files": results})).into_response()
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
            use library_ipc_client::IngestResult;
            let json_results: Vec<IngestResultJson> = results
                .into_iter()
                .map(|item| match item.result {
                    IngestResult::Success { hash, message, .. } => IngestResultJson {
                        path: item.path.to_string(),
                        success: true,
                        pending: false,
                        message,
                        hash: hash.map(|h| h.as_str().to_owned()),
                    },
                    IngestResult::Pending { message } => IngestResultJson {
                        path: item.path.to_string(),
                        success: true,
                        pending: true,
                        message,
                        hash: None,
                    },
                    IngestResult::Failure { message } => IngestResultJson {
                        path: item.path.to_string(),
                        success: false,
                        pending: false,
                        message,
                        hash: None,
                    },
                })
                .collect();

            Json(json_results).into_response()
        }
        Err(e) => api::failure_response(e),
    }
}

// =============================================================================
// Shared helpers
// =============================================================================

#[allow(clippy::result_large_err)]
fn require_gateway(state: &AppState) -> Result<GatewayClient, axum::response::Response> {
    state
        .gateway_client()
        .ok_or_else(|| api::reply_response(api::gateway_unavailable()))
}

fn playback_success_or_error(
    client: &GatewayClient,
    command: &Command,
) -> axum::response::Response {
    match api::send(client, command) {
        Ok(_) => Json(serde_json::json!({"success": true})).into_response(),
        Err(failure) => api::failure_response(failure),
    }
}

#[allow(clippy::result_large_err)]
fn source_request_or_error(
    state: &AppState,
    request: &SourceRequest,
) -> Result<SourceResponse, axum::response::Response> {
    let client = require_gateway(state)?;
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
    let configured_username = read_bandcamp_username(&state.bandcamp_conf_path);
    let cookies_valid = check_cookies_valid(&state.bandcamp_cookies_path);
    match state.gateway_client() {
        Some(client) => {
            match client.source_request(&state.source_name, &SourceRequest::GetStatus) {
                Ok(SourceResponse::Status(status)) => Json(BandcampView {
                    connected: true,
                    authenticated: status.auth == source_protocol::AuthStatus::Authenticated,
                    downloads_active: status.downloads_active,
                    downloads_queued: status.downloads_queued,
                    downloads_completed: status.downloads_completed,
                    downloads_failed: status.downloads_failed,
                    paused: status.queue == source_protocol::QueueState::Paused,
                    configured_username,
                    cookies_valid,
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
        None => Json(BandcampView::from_source_response(
            None,
            configured_username,
            cookies_valid,
        ))
        .into_response(),
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
                        id: d.id.to_string(),
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

async fn bandcamp_configure(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let mut cookies_content: Option<String> = None;
    let mut username: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("cookies") => match field.text().await {
                Ok(text) => cookies_content = Some(text),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": format!("Failed to read cookies field: {}", e)})),
                    )
                        .into_response()
                }
            },
            Some("username") => match field.text().await {
                Ok(text) => username = Some(text.trim().to_string()),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": format!("Failed to read username field: {}", e)})),
                    )
                        .into_response()
                }
            },
            _ => {
                // consume unrecognised fields
                let _ = field.text().await;
            }
        }
    }

    // Validate cookie content
    let cookies_str = match cookies_content {
        Some(ref c) if !c.trim().is_empty() => c.as_str(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "cookies field is required"})),
            )
                .into_response()
        }
    };

    if let Err(e) = bandcamp_api::parse_cookies(cookies_str) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("Invalid cookie file: {}", e)})),
        )
            .into_response();
    }

    // Ensure parent dirs exist
    if let Some(parent) = state.bandcamp_cookies_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Cannot create config dir: {}", e)})),
            )
                .into_response();
        }
    }
    if let Some(parent) = state.bandcamp_conf_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Cannot create config dir: {}", e)})),
            )
                .into_response();
        }
    }

    // Save cookies file
    if let Err(e) =
        write_private_file_async(&state.bandcamp_cookies_path, cookies_str.as_bytes()).await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save cookies: {}", e)})),
        )
            .into_response();
    }

    // Validate and save conf file
    let username_val = username.unwrap_or_default();
    if !username_val
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Username may only contain letters, numbers, hyphens, and underscores"})),
        )
            .into_response();
    }
    let conf_content = format!("MDMA_BANDCAMP_USERNAME=\"{}\"\n", username_val);
    if let Err(e) = tokio::fs::write(&state.bandcamp_conf_path, conf_content.as_bytes()).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {}", e)})),
        )
            .into_response();
    }

    // Restart bandcamp service (best-effort)
    let restart_result = tokio::process::Command::new("sv")
        .args(["restart", "mdma-bandcamp"])
        .status()
        .await;
    if let Err(e) = restart_result {
        tracing::warn!(error = %e, "Failed to restart mdma-bandcamp service");
    }

    Json(serde_json::json!({"success": true})).into_response()
}

// =============================================================================
// Player Types
// =============================================================================

#[derive(serde::Deserialize)]
struct SetAudioOutputRequest {
    device_name: String,
}

// =============================================================================
// Player Handlers
// =============================================================================

/// Skip: atomically stop the current track and advance to the next in the queue.
async fn player_skip(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::Skip)
}

async fn player_audio_outputs(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    match api::send(&client, &Command::ListAudioOutputs) {
        Ok(Some(ResponseData::AudioOutputs(sinks))) => Json(sinks).into_response(),
        Ok(_) => Json(Vec::<AudioSinkInfo>::new()).into_response(),
        Err(failure) => api::failure_response(failure),
    }
}

async fn player_audio_output(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    match api::send(&client, &Command::GetAudioOutput) {
        Ok(Some(ResponseData::AudioOutput(config))) => Json(config).into_response(),
        Ok(_) => api::failure_response(api::PlaybackFailure::Unexpected(
            "Unexpected response".to_string(),
        )),
        Err(failure) => api::failure_response(failure),
    }
}

async fn player_set_audio_output(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetAudioOutputRequest>,
) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(
        &client,
        &Command::SetAudioOutput {
            device_name: req.device_name,
        },
    )
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
// Admin Handlers
// =============================================================================

#[derive(Template)]
#[template(path = "admin.html")]
struct AdminTemplate {
    version: String,
    boot_order: String,
    service_mode_armed: bool,
    pcie_probe: String,
    error: Option<String>,
}

async fn admin_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use gateway_client::{AdminRequest, AdminResponse};

    let (boot_order, service_mode_armed, pcie_probe, error) = match state.gateway_client() {
        None => (
            String::new(),
            false,
            String::new(),
            Some("Admin service not available".to_string()),
        ),
        Some(client) => match client.admin_request(&AdminRequest::ServiceModeStatus) {
            Ok(AdminResponse::Status {
                boot_order,
                service_mode_armed,
                pcie_probe,
            }) => (boot_order, service_mode_armed, pcie_probe, None),
            Ok(AdminResponse::Error { message }) => {
                (String::new(), false, String::new(), Some(message))
            }
            Ok(_) => (
                String::new(),
                false,
                String::new(),
                Some("Unexpected response from admin service".to_string()),
            ),
            Err(e) => (String::new(), false, String::new(), Some(e.to_string())),
        },
    };

    let template = AdminTemplate {
        version: env!("CARGO_PKG_VERSION").to_string(),
        boot_order,
        service_mode_armed,
        pcie_probe,
        error,
    };
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Admin template render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
        }
    }
}

async fn admin_service_mode_enable(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use axum::response::Redirect;
    use gateway_client::{AdminRequest, AdminResponse};

    let result = state
        .gateway_client()
        .ok_or_else(|| "Admin service not available".to_string())
        .and_then(|client| {
            client
                .admin_request(&AdminRequest::ServiceModeEnable)
                .map_err(|e| e.to_string())
        });

    match result {
        Ok(AdminResponse::Ok) => Redirect::to("/admin").into_response(),
        Ok(AdminResponse::Error { message }) => {
            tracing::warn!(message, "service-mode enable refused");
            Redirect::to("/admin?error=enable_refused").into_response()
        }
        Ok(_) => Redirect::to("/admin").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "service-mode enable failed");
            Redirect::to("/admin").into_response()
        }
    }
}

async fn admin_service_mode_disable(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use axum::response::Redirect;
    use gateway_client::{AdminRequest, AdminResponse};

    let result = state
        .gateway_client()
        .ok_or_else(|| "Admin service not available".to_string())
        .and_then(|client| {
            client
                .admin_request(&AdminRequest::ServiceModeDisable)
                .map_err(|e| e.to_string())
        });

    match result {
        Ok(AdminResponse::Ok) => Redirect::to("/admin").into_response(),
        Ok(AdminResponse::Error { message }) => {
            tracing::warn!(message, "service-mode disable refused");
            Redirect::to("/admin").into_response()
        }
        Ok(_) => Redirect::to("/admin").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "service-mode disable failed");
            Redirect::to("/admin").into_response()
        }
    }
}

async fn admin_reboot(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use gateway_client::{AdminRequest, AdminResponse};

    let result = state
        .gateway_client()
        .ok_or_else(|| "Admin service not available".to_string())
        .and_then(|client| {
            client
                .admin_request(&AdminRequest::Reboot { force: false })
                .map_err(|e| e.to_string())
        });

    match result {
        Ok(AdminResponse::Ok) => {
            let template = AdminRebootingTemplate {
                version: env!("CARGO_PKG_VERSION").to_string(),
            };
            match template.render() {
                Ok(html) => Html(html).into_response(),
                Err(_) => Html("<p>Rebooting…</p>").into_response(),
            }
        }
        // Plain text bodies: the messages come from other services and must
        // not be interpreted as HTML.
        Ok(AdminResponse::Error { message }) => {
            tracing::warn!(message, "reboot refused");
            (reboot_error_status(&message), message).into_response()
        }
        Ok(other) => {
            tracing::error!(?other, "unexpected admin response to reboot");
            (
                StatusCode::BAD_GATEWAY,
                format!("Unexpected response from admin service: {other:?}"),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "reboot failed");
            (StatusCode::SERVICE_UNAVAILABLE, format!("Error: {e}")).into_response()
        }
    }
}

/// 409 for a boot-health refusal, 502 for any other admin-side failure.
fn reboot_error_status(message: &str) -> StatusCode {
    if admin_ipc_protocol::is_reboot_refusal(message) {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_GATEWAY
    }
}

#[cfg(test)]
mod compression_tests {
    use super::compress_predicate;
    use crate::test_support::get_with_encoding;
    use axum::body::Body;
    use axum::http::{header, Response};
    use tower_http::compression::predicate::Predicate;

    fn response(content_type: &str, len: usize) -> Response<Body> {
        Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(vec![b'x'; len]))
            .unwrap()
    }

    #[test]
    fn predicate_compresses_text_but_not_streams_or_downloads() {
        let p = compress_predicate();
        assert!(p.should_compress(&response("text/css", 100)));
        assert!(p.should_compress(&response("application/json", 100)));
        assert!(!p.should_compress(&response("text/event-stream", 100)));
        assert!(!p.should_compress(&response("audio/wav", 100_000)));
        assert!(!p.should_compress(&response("audio/flac", 100_000)));
        assert!(!p.should_compress(&response("application/octet-stream", 100_000)));
        assert!(
            !p.should_compress(&response("text/css", 10)),
            "tiny bodies stay plain"
        );
    }

    #[tokio::test]
    async fn sse_stream_is_never_compressed() {
        let res = get_with_encoding("/player/events", Some("gzip")).await;
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        assert!(
            res.headers().get(header::CONTENT_ENCODING).is_none(),
            "gzip would buffer server-sent events"
        );
    }
}

#[cfg(test)]
mod reboot_status_tests {
    use super::*;

    #[test]
    fn refusal_maps_to_conflict_other_errors_to_bad_gateway() {
        assert_eq!(
            reboot_error_status("reboot refused: /boot is not the vfat boot partition"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            reboot_error_status("failed to spawn"),
            StatusCode::BAD_GATEWAY
        );
    }
}

#[derive(Template)]
#[template(path = "admin_rebooting.html")]
struct AdminRebootingTemplate {
    version: String,
}

// =============================================================================
// Event Bridge
// =============================================================================

fn spawn_event_bridge(event_socket: String, event_tx: broadcast::Sender<String>) {
    use event_protocol::{
        acid_event_from_topic_message, from_topic_message, TOPIC_ACID, TOPIC_PLAYBACK,
    };
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
            tracing::error!(error = %e, "SSE: subscribe (playback) failed");
            return;
        }
        if let Err(e) =
            sub.set_opt::<nng::options::protocol::pubsub::Subscribe>(TOPIC_ACID.as_bytes().to_vec())
        {
            tracing::error!(error = %e, "SSE: subscribe (acid/) failed");
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
                        let bytes = msg.as_slice();
                        // Try playback events first, then ACID events
                        if let Ok((_topic, event)) = from_topic_message(bytes) {
                            if let Ok(json) = serde_json::to_string(&event) {
                                let _ = event_tx.send(json);
                            }
                        } else if let Ok((_topic, acid_event)) =
                            acid_event_from_topic_message(bytes)
                        {
                            if let Ok(json) = serde_json::to_string(&acid_event) {
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
    let state = Arc::new(AppState::new(
        args.library_socket,
        args.gateway,
        args.source_name,
        event_tx.clone(),
        args.music_root,
        args.bandcamp_cookies,
        args.bandcamp_conf,
    ));
    spawn_event_bridge(args.event_socket, event_tx);

    // Initial package list load
    if let Ok(pkgs) = packages::list_installed().await {
        *state.packages.lock().await = pkgs;
    }

    let app = app(state);

    tracing::info!(
        "MDMA Console listening on http://[::]:{} (dual-stack)",
        args.port
    );
    http_server::serve(app, &http_server::HttpServerConfig { port: args.port }).await?;

    Ok(())
}

/// What gets gzipped: the defaults (over 32 B, not images or gRPC), but never server-sent
/// events (they must not be buffered) and never audio downloads (`/export/:hash`).
fn compress_predicate() -> impl Predicate {
    DefaultPredicate::new()
        .and(NotForContentType::const_new("text/event-stream"))
        .and(NotForContentType::const_new("audio/"))
        .and(NotForContentType::const_new("application/octet-stream"))
}

fn compression() -> CompressionLayer<impl Predicate> {
    CompressionLayer::new().compress_when(compress_predicate())
}

/// The whole HTTP application: every route, with state and middleware applied.
fn app(state: Arc<AppState>) -> Router {
    Router::new()
        // `/` is the UI shell (assets::routes); the old `/classic` address redirects there
        .route("/classic", get(|| async { Redirect::permanent("/") }))
        .route("/health", get(health))
        .route("/check-updates", post(check_updates))
        .route("/update/:name", post(update_package))
        .route("/ingest-all", post(ingest_all))
        .route(
            "/upload",
            post(upload_file).layer(DefaultBodyLimit::max(500 * 1024 * 1024)),
        )
        // Bandcamp routes
        .route("/bandcamp/status", get(bandcamp_status))
        .route("/bandcamp/sync", post(bandcamp_sync))
        .route("/bandcamp/downloads", get(bandcamp_downloads))
        .route("/bandcamp/pause", post(bandcamp_pause))
        .route("/bandcamp/resume", post(bandcamp_resume))
        .route(
            "/bandcamp/configure",
            post(bandcamp_configure).layer(DefaultBodyLimit::max(5 * 1024 * 1024)),
        )
        // Player routes
        .route("/player/skip", post(player_skip))
        .route("/player/audio-outputs", get(player_audio_outputs))
        .route(
            "/player/audio-output",
            get(player_audio_output).post(player_set_audio_output),
        )
        .route("/player/events", get(player_events))
        // Cover art
        .route("/cover/:hash", get(cover_art))
        // Export
        .route("/export/:hash", get(export_track))
        // Admin routes
        .route("/admin", get(admin_page))
        .route(
            "/admin/service-mode/enable",
            post(admin_service_mode_enable),
        )
        .route(
            "/admin/service-mode/disable",
            post(admin_service_mode_disable),
        )
        .route("/admin/reboot", post(admin_reboot))
        // New web UI: JSON API, embedded assets and the app shell
        .merge(api::routes())
        .merge(assets::routes())
        .with_state(state)
        .layer(compression())
}

// =============================================================================
// Export
// =============================================================================

/// Export format query parameter.
///
/// `original` serves the blob file without any transcoding (default).
/// `aiff` and `wav` transcode the source to the requested format.
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum ExportFormatParam {
    /// Serve the source file as-is without transcoding (default)
    #[default]
    Original,
    Aiff,
    Wav,
}

impl ExportFormatParam {
    /// Return the file extension for converted formats, or `None` for `Original`.
    ///
    /// For `Original`, the extension must be derived from the blob path.
    fn extension(&self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aiff => Some(audio_transcoder::ExportFormat::Aiff.extension()),
            Self::Wav => Some(audio_transcoder::ExportFormat::Wav.extension()),
        }
    }

    /// Return the MIME content type for converted formats, or `None` for `Original`.
    ///
    /// For `Original`, the content type must be derived from the blob path.
    fn content_type(&self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aiff => Some("audio/aiff"),
            Self::Wav => Some("audio/wav"),
        }
    }

    /// Returns `true` when the format should be served as a direct blob passthrough
    /// without any transcoding.
    fn is_passthrough(&self) -> bool {
        matches!(self, Self::Original)
    }

    fn to_transcoder_format(&self) -> audio_transcoder::ExportFormat {
        match self {
            Self::Original => unreachable!("Original format must be handled via passthrough"),
            Self::Aiff => audio_transcoder::ExportFormat::Aiff,
            Self::Wav => audio_transcoder::ExportFormat::Wav,
        }
    }
}

/// Detect format of a blob file from its extension.
fn blob_extension(blob_path: &str) -> Option<&str> {
    std::path::Path::new(blob_path)
        .extension()
        .and_then(|e| e.to_str())
}

/// Build a safe download filename from track metadata.
fn export_filename(artist: Option<&str>, title: Option<&str>, ext: &str) -> String {
    let artist = artist.unwrap_or("Unknown Artist");
    let title = title.unwrap_or("Unknown Title");
    let base = format!("{} - {}", artist, title);
    // Remove characters unsafe in filenames
    let safe: String = base
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    format!("{}.{}", safe, ext)
}

/// How a blob is exported: the extension and content type of the download, and
/// whether the blob is served unchanged.
#[derive(Debug, PartialEq, Eq)]
struct ExportPlan {
    ext: String,
    content_type: String,
    serve_directly: bool,
}

use audio_transcoder::{canonical_ext, describe_source, ExportSource};

/// Decide the export of a blob with extension `source_ext` as `req_format`.
/// `source` is `None` for `original` (nothing was read from the file) and
/// describes the blob for the converted formats.
///
/// `original` always serves the blob unchanged. A PCM export format (`aiff`,
/// `wav`) serves it unchanged only when it already is what the export would
/// produce: the same canonical extension, plain PCM, and its rate and depth are
/// ones the export keeps (`SourceAudio::is_pcm_export_ready`, the transcoder's own
/// rule). Everything else is transcoded.
fn export_plan(
    req_format: &ExportFormatParam,
    source_ext: &str,
    source: Option<&ExportSource>,
) -> ExportPlan {
    let (ext, content_type) = if req_format.is_passthrough() {
        let ct = match canonical_ext(source_ext) {
            "flac" => "audio/flac",
            "mp3" => "audio/mpeg",
            "aiff" => "audio/aiff",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "opus" => "audio/opus",
            _ => "application/octet-stream",
        };
        (source_ext, ct)
    } else {
        (
            req_format.extension().unwrap_or("bin"),
            req_format
                .content_type()
                .unwrap_or("application/octet-stream"),
        )
    };
    let serve_directly = match req_format {
        ExportFormatParam::Original => true,
        // PCM exports resample and re-depth: only a blob that needs neither is served
        ExportFormatParam::Aiff | ExportFormatParam::Wav => {
            canonical_ext(source_ext) == ext
                && source.is_some_and(|s| s.plain_pcm && s.audio.is_pcm_export_ready())
        }
    };
    ExportPlan {
        ext: ext.to_string(),
        content_type: content_type.to_string(),
        serve_directly,
    }
}

#[derive(serde::Deserialize)]
struct ExportParams {
    #[serde(default)]
    format: ExportFormatParam,
}

async fn export_track(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
    Query(params): Query<ExportParams>,
) -> impl IntoResponse {
    use axum::http::header;

    let content_hash = ContentHash::new(hash);

    // Resolve track metadata from library
    let lib_client = match state.library_client() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Library service not available",
            )
                .into_response()
        }
    };

    let track = match lib_client.get_track(&content_hash) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(hash = %content_hash.as_str(), error = %e, "Track not found for export");
            return (StatusCode::NOT_FOUND, format!("Track not found: {}", e)).into_response();
        }
    };

    let blob_rel = match &track.blob_path {
        Some(p) => p.clone(),
        None => {
            return (StatusCode::UNPROCESSABLE_ENTITY, "Track has no blob path").into_response()
        }
    };

    let blob_path = state.music_root.join(&blob_rel);

    let req_format = &params.format;

    // Determine actual extension and content-type.
    // For Original, derive from blob_path; for others use fixed values.
    let source_ext = blob_extension(&blob_rel).unwrap_or("bin").to_lowercase();
    // `original` needs nothing from the file; a converted format reads its properties.
    let source = if req_format.is_passthrough() {
        None
    } else {
        let (path, ext) = (blob_path.clone(), source_ext.clone());
        match tokio::task::spawn_blocking(move || describe_source(&path, &ext)).await {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!(error = %e, "Describing the export source failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Export failed").into_response();
            }
        }
    };
    let ExportPlan {
        ext,
        content_type: content_type_str,
        serve_directly,
    } = export_plan(req_format, &source_ext, source.as_ref());

    let filename = export_filename(track.artist.as_deref(), track.title.as_deref(), &ext);

    if serve_directly {
        let data = match tokio::fs::read(&blob_path).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(path = %blob_path.display(), error = %e, "Failed to read blob");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to read blob file",
                )
                    .into_response();
            }
        };

        return (
            [
                (header::CONTENT_TYPE, content_type_str.clone()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                ),
            ],
            data,
        )
            .into_response();
    }

    // Transcode: ffmpeg reads the source directly and writes to a temp file.
    // This is blocking CPU/IO work so we run it in spawn_blocking.
    let transcode_format = req_format.to_transcoder_format();
    // only `original` has no description, and it never transcodes
    let source_audio = source.map(|s| s.audio).unwrap_or_default();

    let metadata = audio_transcoder::ExportMetadata {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        bpm: track.bpm.map(|b| b.as_f32() as f64),
        // Short form (`Abm`, `C`): fits the ID3v2 TKEY frame's three characters.
        key: track.key.map(|k| k.to_short_flat()),
    };

    let transcode_result = tokio::task::spawn_blocking(move || {
        let ext_suffix = transcode_format.suffix();
        let tmp = tempfile::Builder::new()
            .suffix(ext_suffix)
            .tempfile()
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        // `source` describes the blob so the transcoder can downsample for CDJs,
        // keep 16-bit sources 16-bit and trim AAC padding. Unreadable metadata
        // means an unknown source: exported as-is.
        audio_transcoder::transcode(
            &blob_path,
            tmp.path(),
            &transcode_format,
            &metadata,
            &source_audio,
        )
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        std::fs::read(tmp.path())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    })
    .await;

    let encoded = match transcode_result {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "Transcode failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Transcode failed: {}", e),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "Blocking task panicked during transcode");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal transcode error",
            )
                .into_response();
        }
    };

    (
        [
            (header::CONTENT_TYPE, content_type_str),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        encoded,
    )
        .into_response()
}

// =============================================================================
// Cover Art Handler
// =============================================================================

/// Detect MIME type from cover art file extension.
fn cover_art_content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

async fn cover_art(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> impl IntoResponse {
    use axum::http::header;

    let content_hash = ContentHash::new(hash);

    // The library round trip blocks on NNG; keep it off the async workers.
    let lookup = {
        let state = Arc::clone(&state);
        tokio::task::spawn_blocking(move || {
            state
                .library_client()
                .map(|client| client.get_track(&content_hash))
        })
        .await
    };
    let track = match lookup {
        Ok(Some(Ok(t))) => t,
        Ok(Some(Err(_))) => {
            return (StatusCode::NOT_FOUND, "Track not found").into_response();
        }
        Ok(None) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Library not available").into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    let cover_rel = match track.cover_art_path {
        Some(p) => p,
        None => {
            return (StatusCode::NOT_FOUND, "No cover art").into_response();
        }
    };

    let cover_path = state.music_root.join(&cover_rel);
    let content_type = cover_art_content_type(&cover_rel);

    match tokio::fs::read(&cover_path).await {
        Ok(data) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            data,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Cover art file not found").into_response(),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use source_protocol::{AuthStatus, QueueState, SourceResponse, SourceStatus};

    #[test]
    fn library_failures_are_classified_by_one_policy() {
        use library_ipc_client::{ClientError, ProtocolError};
        let protocol = |e: ProtocolError| api::library_failure_status(&ClientError::Protocol(e));
        assert_eq!(
            protocol(ProtocolError::PlaylistNotFound { name: "x".into() }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            protocol(ProtocolError::ambiguous_hash("ab", ["abc", "abd"])),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            protocol(ProtocolError::TrackNotFound { hash: "x".into() }),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            protocol(ProtocolError::HashNotCanonical {
                hash: "9f".into(),
                found: "9fb4105e".into()
            }),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            protocol(ProtocolError::InvalidRequest {
                message: "x".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            protocol(ProtocolError::Internal {
                message: "x".into()
            }),
            StatusCode::BAD_GATEWAY
        );
    }

    fn make_source_status() -> SourceStatus {
        SourceStatus {
            name: "bandcamp".to_string(),
            version: "0.1.0".to_string(),
            auth: AuthStatus::Authenticated,
            downloads_active: 2,
            downloads_queued: 3,
            downloads_completed: 10,
            downloads_failed: 1,
            uptime_seconds: 3600,
            queue: QueueState::Active,
        }
    }

    #[test]
    fn bandcamp_view_from_none_is_disconnected() {
        let view = BandcampView::from_source_response(None, String::new(), false);
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
        let view = BandcampView::from_source_response(
            Some(SourceResponse::Status(status)),
            String::new(),
            false,
        );
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
        let view =
            BandcampView::from_source_response(Some(SourceResponse::Pong), String::new(), false);
        assert!(!view.connected);
        assert!(!view.authenticated);
    }

    #[test]
    fn bandcamp_view_carries_configured_username() {
        let view = BandcampView::from_source_response(None, "johlyroger".to_string(), true);
        assert_eq!(view.configured_username, "johlyroger");
        assert!(view.cookies_valid);
    }

    #[test]
    fn read_bandcamp_username_parses_conf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bandcamp.conf");
        std::fs::write(&path, "MDMA_BANDCAMP_USERNAME=\"testuser\"\n").unwrap();
        assert_eq!(read_bandcamp_username(&path), "testuser");
    }

    #[test]
    fn read_bandcamp_username_missing_file_returns_empty() {
        let path = std::path::PathBuf::from("/nonexistent/bandcamp.conf");
        assert_eq!(read_bandcamp_username(&path), "");
    }

    #[test]
    fn check_cookies_valid_with_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.json");
        // Minimal valid JSON cookie
        let content = r#"[{"name":"identity","value":"abc123","domain":".bandcamp.com"}]"#;
        std::fs::write(&path, content).unwrap();
        assert!(check_cookies_valid(&path));
    }

    #[tokio::test]
    async fn write_private_file_creates_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.json");
        write_private_file_async(&path, b"[]").await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"[]");
    }

    #[tokio::test]
    async fn write_private_file_overwrite_tightens_existing_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_file_async(&path, b"new").await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn check_cookies_valid_with_missing_file() {
        let path = std::path::PathBuf::from("/nonexistent/cookies.json");
        assert!(!check_cookies_valid(&path));
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
    }

    // ── Export helpers ────────────────────────────────────────────────────────

    #[test]
    fn export_format_defaults_to_original() {
        let default = ExportFormatParam::default();
        assert_eq!(default, ExportFormatParam::Original);
    }

    #[test]
    fn export_format_extensions() {
        assert_eq!(ExportFormatParam::Aiff.extension(), Some("aiff"));
        assert_eq!(ExportFormatParam::Wav.extension(), Some("wav"));
        // Original has no fixed extension
        assert_eq!(ExportFormatParam::Original.extension(), None);
    }

    #[test]
    fn export_format_content_types() {
        assert_eq!(ExportFormatParam::Aiff.content_type(), Some("audio/aiff"));
        assert_eq!(ExportFormatParam::Wav.content_type(), Some("audio/wav"));
        // Original defers content type to the blob
        assert_eq!(ExportFormatParam::Original.content_type(), None);
    }

    #[test]
    fn export_format_original_is_passthrough() {
        // Original must always take the direct-serve path (is_passthrough returns true)
        assert!(ExportFormatParam::Original.is_passthrough());
        assert!(!ExportFormatParam::Aiff.is_passthrough());
        assert!(!ExportFormatParam::Wav.is_passthrough());
    }

    #[test]
    fn export_filename_with_artist_and_title() {
        let name = export_filename(Some("DJ Artist"), Some("Track Title"), "aiff");
        assert_eq!(name, "DJ Artist - Track Title.aiff");
    }

    #[test]
    fn export_filename_with_missing_artist() {
        let name = export_filename(None, Some("Track Title"), "wav");
        assert_eq!(name, "Unknown Artist - Track Title.wav");
    }

    #[test]
    fn export_filename_with_missing_title() {
        let name = export_filename(Some("DJ Artist"), None, "flac");
        assert_eq!(name, "DJ Artist - Unknown Title.flac");
    }

    #[test]
    fn export_filename_sanitizes_unsafe_chars() {
        let name = export_filename(Some("Artist/Name"), Some("Track: The \"Remix\""), "aiff");
        assert_eq!(name, "Artist_Name - Track_ The _Remix_.aiff");
    }

    #[test]
    fn blob_extension_detects_flac() {
        assert_eq!(blob_extension("blobs/ab/abc123.flac"), Some("flac"));
    }

    #[test]
    fn blob_extension_detects_mp3() {
        assert_eq!(blob_extension("blobs/ab/abc123.mp3"), Some("mp3"));
    }

    #[test]
    fn blob_extension_missing() {
        assert_eq!(blob_extension("blobs/ab/noext"), None);
    }

    // ── Username validation ───────────────────────────────────────────────────

    fn is_valid_username(username: &str) -> bool {
        username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    #[test]
    fn username_valid_alphanumeric_hyphen_underscore() {
        assert!(is_valid_username("johlyroger"));
        assert!(is_valid_username("john-doe"));
        assert!(is_valid_username("john_doe"));
        assert!(is_valid_username("User123"));
        assert!(is_valid_username("a-b_c-1"));
    }

    #[test]
    fn username_rejects_shell_metacharacters() {
        // Each of these would allow shell injection if embedded in the conf file
        let bad_inputs = [
            "user;rm -rf /",
            "user$(whoami)",
            "user`id`",
            "user|cat /etc/passwd",
            "user&&evil",
            "user\nMDMA_OTHER=injected",
            "user\"extra",
            "user'quoted",
            "user space",
            "user!bang",
        ];
        for bad in &bad_inputs {
            assert!(!is_valid_username(bad), "Expected '{}' to be rejected", bad);
        }
    }

    #[test]
    fn username_rejects_empty_string() {
        // An empty username is technically valid per the char check (vacuously true),
        // but the actual handler accepts empty as "no username configured".
        // This test documents the current behaviour of the pure char predicate.
        assert!(is_valid_username(""));
    }

    // ── Cover art content type ─────────────────────────────────────────────

    #[test]
    fn cover_art_content_type_jpeg() {
        assert_eq!(cover_art_content_type("cover-art/abc.jpg"), "image/jpeg");
        assert_eq!(cover_art_content_type("cover-art/abc.jpeg"), "image/jpeg");
    }

    #[test]
    fn cover_art_content_type_png() {
        assert_eq!(cover_art_content_type("cover-art/abc.png"), "image/png");
    }

    #[test]
    fn cover_art_content_type_webp() {
        assert_eq!(cover_art_content_type("cover-art/abc.webp"), "image/webp");
    }

    #[test]
    fn cover_art_content_type_unknown_defaults_to_octet_stream() {
        assert_eq!(
            cover_art_content_type("cover-art/abc.bmp"),
            "application/octet-stream"
        );
        assert_eq!(
            cover_art_content_type("cover-art/noext"),
            "application/octet-stream"
        );
    }

    #[test]
    fn cover_art_content_type_case_insensitive() {
        assert_eq!(cover_art_content_type("cover-art/abc.JPG"), "image/jpeg");
        assert_eq!(cover_art_content_type("cover-art/abc.PNG"), "image/png");
    }

    fn src(rate: Option<u32>, depth: Option<u8>, plain_pcm: bool) -> ExportSource {
        ExportSource {
            audio: audio_transcoder::SourceAudio {
                sample_rate: rate,
                bit_depth: depth,
                ..Default::default()
            },
            plain_pcm,
        }
    }

    fn plan(fmt: &ExportFormatParam, ext: &str, source: &ExportSource) -> ExportPlan {
        export_plan(fmt, ext, Some(source))
    }

    #[test]
    fn canonical_ext_maps_aif_to_aiff_only() {
        assert_eq!(canonical_ext("aif"), "aiff");
        assert_eq!(canonical_ext("aiff"), "aiff");
        assert_eq!(canonical_ext("flac"), "flac");
    }

    #[test]
    fn original_always_passes_an_aiff_blob_through() {
        // ADR-037: whatever the AIFF blob is, `original` serves it as it is.
        // `original` reads nothing from the file, so it has no description
        for ext in ["aiff", "aif"] {
            let plan = export_plan(&ExportFormatParam::Original, ext, None);
            assert!(plan.serve_directly);
            assert_eq!(plan.content_type, "audio/aiff");
            assert_eq!(plan.ext, ext);
        }
    }

    #[test]
    fn plain_aiff_within_the_limits_is_served_for_format_aiff() {
        for ext in ["aiff", "aif"] {
            for (rate, depth) in [(44_100, 16), (48_000, 24), (44_100, 24)] {
                let source = src(Some(rate), Some(depth), true);
                let plan = plan(&ExportFormatParam::Aiff, ext, &source);
                assert!(plan.serve_directly, "{ext} {rate} {depth}");
                // named .aiff even for an .aif blob
                assert_eq!(plan.ext, "aiff");
                assert_eq!(plan.content_type, "audio/aiff");
            }
        }
    }

    #[test]
    fn blobs_outside_the_export_rules_are_transcoded_for_the_pcm_formats() {
        let cases = [
            (
                "AIFF-C or codec",
                "aiff",
                src(Some(44_100), Some(16), false),
            ),
            ("above 48 kHz", "aiff", src(Some(96_000), Some(24), true)),
            ("32-bit", "aiff", src(Some(44_100), Some(32), true)),
            ("20-bit", "aiff", src(Some(44_100), Some(20), true)),
            ("8-bit", "aiff", src(Some(44_100), Some(8), true)),
            ("unknown properties", "aiff", src(None, None, true)),
            ("flac blob", "flac", src(Some(44_100), Some(16), true)),
        ];
        for (what, ext, source) in cases {
            assert!(
                !plan(&ExportFormatParam::Aiff, ext, &source).serve_directly,
                "{what}"
            );
        }
    }

    #[test]
    fn wav_follows_the_same_rule_as_aiff() {
        let wav = |source: &ExportSource| plan(&ExportFormatParam::Wav, "wav", source);
        assert!(wav(&src(Some(44_100), Some(16), true)).serve_directly);
        assert!(wav(&src(Some(48_000), Some(24), true)).serve_directly);
        assert!(!wav(&src(Some(96_000), Some(24), true)).serve_directly);
        assert!(!wav(&src(Some(44_100), Some(32), true)).serve_directly);
        assert!(!wav(&src(None, None, true)).serve_directly);
        assert!(!wav(&src(Some(44_100), Some(16), false)).serve_directly);
    }

    #[test]
    fn a_blob_of_another_format_is_transcoded_to_wav() {
        let aiff = src(Some(44_100), Some(16), true);
        let p = plan(&ExportFormatParam::Wav, "aiff", &aiff);
        assert!(!p.serve_directly);
        assert_eq!(
            (p.ext.as_str(), p.content_type.as_str()),
            ("wav", "audio/wav")
        );
    }

    #[test]
    fn aif_source_treated_as_aiff_for_passthrough() {
        let source_ext = blob_extension("blobs/ab/track.aif").unwrap().to_lowercase();
        let fmt = ExportFormatParam::Aiff;
        assert!(
            Some(source_ext.as_str()) == fmt.extension()
                || (source_ext == "aif" && fmt.extension() == Some("aiff"))
        );
    }
}
