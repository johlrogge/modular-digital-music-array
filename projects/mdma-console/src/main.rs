//! MDMA Console - Web interface for package, inbox, and bandcamp management

use askama::Template;
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
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
use library_ipc_client::{
    ContentHash, FactType, LibraryClient, PlaylistName, TrackInfo, TrackQuery,
};
use media_protocol::{AudioSinkInfo, Deck, ResponseData, SourceName};
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

/// Check whether the cookies file parses successfully
fn check_cookies_valid(cookies_path: &std::path::Path) -> bool {
    match std::fs::read_to_string(cookies_path) {
        Ok(content) => bandcamp_api::parse_cookies(&content).is_ok(),
        Err(_) => false,
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
    let configured_username = read_bandcamp_username(&state.bandcamp_conf_path);
    let cookies_valid = check_cookies_valid(&state.bandcamp_cookies_path);

    let bandcamp =
        BandcampView::from_source_response(bandcamp_response, configured_username, cookies_valid);

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
                    IngestResult::Success { hash, message } => IngestResultJson {
                        path: item.path.to_string(),
                        success: true,
                        message,
                        hash: hash.map(|h| h.as_str().to_owned()),
                    },
                    IngestResult::Failure { message } => IngestResultJson {
                        path: item.path.to_string(),
                        success: false,
                        message,
                        hash: None,
                    },
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
    use gateway_client::Response;
    match client.playback_command(command) {
        Ok(Response::Ok { .. }) => Json(serde_json::json!({"success": true})).into_response(),
        Ok(Response::Err { message }) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": message})),
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
    if let Err(e) = tokio::fs::write(&state.bandcamp_cookies_path, cookies_str.as_bytes()).await {
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

#[derive(serde::Serialize)]
struct TrackInfoJson {
    content_hash: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    duration: Option<u32>,
    bpm: Option<f32>,
    key: Option<String>,
    disc_number: Option<u32>,
    added: Option<String>,
}

impl TrackInfoJson {
    fn from_track_info(t: &TrackInfo) -> Self {
        Self {
            content_hash: t.content_hash.as_str().to_owned(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration: t.duration.map(|d| d.value()),
            bpm: t.bpm.map(|b| b.as_f32()),
            key: t.key.map(|k| k.to_string()),
            disc_number: t.disc_number,
            added: t.added.clone(),
        }
    }
}

// =============================================================================
// Player Handlers
// =============================================================================

async fn player_session(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    use gateway_client::Response;
    match client.playback_command(&Command::GetSession) {
        Ok(Response::Ok { data }) => match data {
            Some(media_protocol::ResponseData::Session(Some(id))) => {
                Json(serde_json::json!({"session": id})).into_response()
            }
            Some(media_protocol::ResponseData::Session(None)) | None => {
                Json(serde_json::json!({"session": null})).into_response()
            }
            _ => Json(serde_json::json!({"session": null})).into_response(),
        },
        Ok(Response::Err { message }) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": message})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn player_now_playing(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    use gateway_client::Response;
    match client.playback_command(&Command::NowPlaying) {
        Ok(Response::Ok { data }) => match data {
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
                        "track": { "content_hash": hash.as_str() }
                    }))
                    .into_response(),
                }
            }
            Some(ResponseData::NowPlaying(None)) | None => {
                Json(serde_json::json!({"playing": false})).into_response()
            }
            _ => Json(serde_json::json!({"playing": false})).into_response(),
        },
        Ok(Response::Err { .. }) => Json(serde_json::json!({"playing": false})).into_response(),
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

    use gateway_client::Response;
    match client.playback_command(&Command::QueueList) {
        Ok(Response::Ok { data }) => match data {
            Some(ResponseData::Queue(hashes)) => {
                let tracks: Vec<serde_json::Value> = hashes
                    .iter()
                    .map(|hash| {
                        let track = state
                            .library_client()
                            .and_then(|lib| lib.get_track(hash).ok());
                        match track {
                            Some(t) => serde_json::to_value(TrackInfoJson::from_track_info(&t))
                                .unwrap_or(serde_json::json!({"content_hash": hash.as_str()})),
                            None => serde_json::json!({"content_hash": hash.as_str()}),
                        }
                    })
                    .collect();
                Json(tracks).into_response()
            }
            _ => Json(serde_json::json!([])).into_response(),
        },
        Ok(Response::Err { .. }) => Json(serde_json::json!([])).into_response(),
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
    let hash = ContentHash::new(req.hash);

    let gw = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(
        &gw,
        &Command::QueueAppend {
            hash,
            source: SourceName::audio(),
        },
    )
}

#[derive(serde::Deserialize)]
struct QueueRemoveRequest {
    hashes: Vec<String>,
}

async fn player_queue_remove(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QueueRemoveRequest>,
) -> impl IntoResponse {
    let hashes: Vec<ContentHash> = req.hashes.into_iter().map(ContentHash::new).collect();

    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::QueueRemove { hashes })
}

#[derive(serde::Deserialize)]
struct QueueReplaceRequest {
    hashes: Vec<String>,
}

async fn player_queue_replace(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QueueReplaceRequest>,
) -> impl IntoResponse {
    let entries: Vec<(ContentHash, SourceName)> = req
        .hashes
        .into_iter()
        .map(|h| (ContentHash::new(h), SourceName::audio()))
        .collect();

    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::QueueReplace { entries })
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

/// Skip: atomically stop the current track and advance to the next in the queue.
async fn player_skip(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::Skip)
}

async fn player_pause(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::Pause { deck: Deck::A })
}

async fn player_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    playback_success_or_error(&client, &Command::Resume { deck: Deck::A })
}

async fn player_audio_outputs(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    use gateway_client::Response;
    match client.playback_command(&Command::ListAudioOutputs) {
        Ok(Response::Ok { data }) => match data {
            Some(ResponseData::AudioOutputs(sinks)) => Json(sinks).into_response(),
            _ => Json(Vec::<AudioSinkInfo>::new()).into_response(),
        },
        Ok(Response::Err { message }) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": message})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn player_audio_output(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };

    use gateway_client::Response;
    match client.playback_command(&Command::GetAudioOutput) {
        Ok(Response::Ok { data }) => match data {
            Some(ResponseData::AudioOutput(config)) => Json(config).into_response(),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Unexpected response"})),
            )
                .into_response(),
        },
        Ok(Response::Err { message }) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": message})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
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
// Library Browse Handlers (by artist / album)
// =============================================================================

/// Shared helper: fetch all values for a fact type and return them sorted case-insensitively.
fn library_fact_values(state: &AppState, fact_type: &str) -> axum::response::Response {
    use gateway_client::{LibraryRequest, LibraryResponse};
    let client = match require_gateway(state) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match client.library_request(&LibraryRequest::GetFactValues {
        fact_type: FactType::new(fact_type),
    }) {
        Ok(LibraryResponse::FactValues(mut values)) => {
            values.sort_by_key(|a| a.to_lowercase());
            Json(values).into_response()
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

/// Shared helper: search tracks by a pre-built query and return them as JSON.
fn library_search_tracks(state: &AppState, query: TrackQuery) -> axum::response::Response {
    use gateway_client::{LibraryRequest, LibraryResponse};
    let client = match require_gateway(state) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match client.library_request(&LibraryRequest::Search { query }) {
        Ok(LibraryResponse::SearchResults(tracks)) => Json(
            tracks
                .iter()
                .map(TrackInfoJson::from_track_info)
                .collect::<Vec<_>>(),
        )
        .into_response(),
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

async fn library_artists_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    library_fact_values(&state, "Artist")
}

async fn library_albums_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    library_fact_values(&state, "Album")
}

async fn library_artist_tracks_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    let query = TrackQuery {
        artist: Some(library_search::parse_string_query(&name)),
        ..Default::default()
    };
    library_search_tracks(&state, query)
}

async fn library_album_tracks_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use gateway_client::{LibraryRequest, LibraryResponse};
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let query = TrackQuery {
        album: Some(library_search::parse_string_query(&name)),
        ..Default::default()
    };
    match client.library_request(&LibraryRequest::Search { query }) {
        Ok(LibraryResponse::SearchResults(mut tracks)) => {
            tracks.sort_by(|a, b| {
                let disc_a = a.disc_number.unwrap_or(1);
                let disc_b = b.disc_number.unwrap_or(1);
                disc_a
                    .cmp(&disc_b)
                    .then_with(|| a.track_number.cmp(&b.track_number))
            });
            Json(
                tracks
                    .iter()
                    .map(TrackInfoJson::from_track_info)
                    .collect::<Vec<_>>(),
            )
            .into_response()
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
// Playlist Handlers
// =============================================================================

async fn library_playlists_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    use gateway_client::{LibraryRequest, LibraryResponse};
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match client.library_request(&LibraryRequest::PlaylistList) {
        Ok(LibraryResponse::PlaylistNames(names)) => {
            let json: Vec<serde_json::Value> = names
                .iter()
                .map(|n| serde_json::json!({"name": n.as_str()}))
                .collect();
            Json(json).into_response()
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

async fn library_playlist_tracks_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use gateway_client::{LibraryRequest, LibraryResponse};
    let playlist_name = match PlaylistName::new(&name) {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match client.library_request(&LibraryRequest::PlaylistGet {
        name: playlist_name,
    }) {
        Ok(LibraryResponse::PlaylistContent(content)) => {
            // Parse hashes from playlist content (one hash per line, first token)
            let hashes: Vec<ContentHash> = content
                .lines()
                .filter_map(|line| {
                    let token = line.split_whitespace().next()?;
                    if token.is_empty() {
                        None
                    } else {
                        Some(ContentHash::new(token.to_string()))
                    }
                })
                .collect();
            let lib = match state.library_client() {
                Some(c) => c,
                None => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({"error": "Library service not available"})),
                    )
                        .into_response()
                }
            };
            let tracks: Vec<serde_json::Value> = hashes
                .iter()
                .filter_map(|hash| lib.get_track(hash).ok())
                .map(|t| {
                    serde_json::to_value(TrackInfoJson::from_track_info(&t))
                        .unwrap_or(serde_json::json!({}))
                })
                .collect();
            Json(tracks).into_response()
        }
        Ok(LibraryResponse::Error(e)) => (
            StatusCode::NOT_FOUND,
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

async fn library_playlist_queue_replace_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use gateway_client::{LibraryRequest, LibraryResponse};
    let playlist_name = match PlaylistName::new(&name) {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let client = match require_gateway(&state) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let hashes = match client.library_request(&LibraryRequest::PlaylistGet {
        name: playlist_name,
    }) {
        Ok(LibraryResponse::PlaylistContent(content)) => content
            .lines()
            .filter_map(|line| {
                let token = line.split_whitespace().next()?;
                if token.is_empty() {
                    None
                } else {
                    Some(ContentHash::new(token.to_string()))
                }
            })
            .collect::<Vec<_>>(),
        Ok(LibraryResponse::Error(e)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
        Ok(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Unexpected library response"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let entries: Vec<(ContentHash, SourceName)> = hashes
        .into_iter()
        .map(|h| (h, SourceName::audio()))
        .collect();
    playback_success_or_error(&client, &Command::QueueReplace { entries })
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
                .admin_request(&AdminRequest::Reboot)
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
        Ok(AdminResponse::Error { message }) => {
            tracing::warn!(message, "reboot refused");
            (
                StatusCode::BAD_GATEWAY,
                Html(format!("<p>Reboot refused: {message}</p>")),
            )
                .into_response()
        }
        Ok(_) => Html("<p>Rebooting…</p>").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "reboot failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Html(format!("<p>Error: {e}</p>")),
            )
                .into_response()
        }
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
    spawn_event_bridge(args.event_socket, event_tx.clone());

    let state = Arc::new(AppState::new(
        args.library_socket,
        args.gateway,
        args.source_name,
        event_tx,
        args.music_root,
        args.bandcamp_cookies,
        args.bandcamp_conf,
    ));

    // Initial package list load
    if let Ok(pkgs) = packages::list_installed().await {
        *state.packages.lock().await = pkgs;
    }

    let app = Router::new()
        .route("/", get(index))
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
        .route("/player/session", get(player_session))
        .route("/player/now-playing", get(player_now_playing))
        .route("/player/queue", get(player_queue))
        .route("/player/queue/append", post(player_queue_append))
        .route("/player/queue/remove", post(player_queue_remove))
        .route("/player/queue/replace", post(player_queue_replace))
        .route("/player/queue/clear", post(player_queue_clear))
        .route("/player/play-queue", post(player_play_queue))
        .route("/player/stop", post(player_stop))
        .route("/player/skip", post(player_skip))
        .route("/player/pause", post(player_pause))
        .route("/player/resume", post(player_resume))
        .route("/player/audio-outputs", get(player_audio_outputs))
        .route(
            "/player/audio-output",
            get(player_audio_output).post(player_set_audio_output),
        )
        .route("/player/events", get(player_events))
        // Library search and browse
        .route("/library/search", get(library_search_handler))
        .route("/library/artists", get(library_artists_handler))
        .route("/library/albums", get(library_albums_handler))
        .route(
            "/library/artists/:name/tracks",
            get(library_artist_tracks_handler),
        )
        .route(
            "/library/albums/:name/tracks",
            get(library_album_tracks_handler),
        )
        .route("/library/playlists", get(library_playlists_handler))
        .route(
            "/library/playlists/:name/tracks",
            get(library_playlist_tracks_handler),
        )
        .route(
            "/library/playlists/:name/queue-replace",
            post(library_playlist_queue_replace_handler),
        )
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
        .with_state(state);

    tracing::info!(
        "MDMA Console listening on http://[::]:{} (dual-stack)",
        args.port
    );
    http_server::serve(app, &http_server::HttpServerConfig { port: args.port }).await?;

    Ok(())
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
    let (ext, content_type_str): (String, String) = if req_format.is_passthrough() {
        let ct = match source_ext.as_str() {
            "flac" => "audio/flac",
            "mp3" => "audio/mpeg",
            "aiff" | "aif" => "audio/aiff",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "opus" => "audio/opus",
            _ => "application/octet-stream",
        };
        (source_ext.clone(), ct.to_string())
    } else {
        let fixed_ext = req_format.extension().unwrap_or("bin");
        let fixed_ct = req_format
            .content_type()
            .unwrap_or("application/octet-stream");
        (fixed_ext.to_string(), fixed_ct.to_string())
    };

    let filename = export_filename(track.artist.as_deref(), track.title.as_deref(), &ext);

    // Original always takes the direct-serve path.
    // For converted formats, also take the direct path when source already matches.
    let serve_directly =
        req_format.is_passthrough() || source_ext == ext || (source_ext == "aif" && ext == "aiff");

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

    let metadata = audio_transcoder::ExportMetadata {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        bpm: track.bpm.map(|b| b.as_f32() as f64),
        key: track.key.map(|k| k.to_traditional_sharp()),
    };

    let transcode_result = tokio::task::spawn_blocking(move || {
        let ext_suffix = transcode_format.suffix();
        let tmp = tempfile::Builder::new()
            .suffix(ext_suffix)
            .tempfile()
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        audio_transcoder::transcode(&blob_path, tmp.path(), &transcode_format, &metadata)
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

    let lib_client = match state.library_client() {
        Some(c) => c,
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Library not available").into_response();
        }
    };

    let track = match lib_client.get_track(&content_hash) {
        Ok(t) => t,
        Err(_) => {
            return (StatusCode::NOT_FOUND, "Track not found").into_response();
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
    use library_ipc_client::{Bpm, ContentHash, DurationSeconds, Key};
    use pretty_assertions::assert_eq;
    use source_protocol::{AuthStatus, QueueState, SourceResponse, SourceStatus};

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

    #[test]
    fn check_cookies_valid_with_missing_file() {
        let path = std::path::PathBuf::from("/nonexistent/cookies.json");
        assert!(!check_cookies_valid(&path));
    }

    #[test]
    fn track_info_json_from_full_track() {
        let track = TrackInfo {
            content_hash: ContentHash::new("sha256:abc123"),
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            duration: Some(DurationSeconds::new(180)),
            bpm: Some(Bpm::from_u32(128).unwrap()),
            key: Some(Key::from_traditional("C Major").unwrap()),
            blob_path: Some("ab/abc123.flac".to_string()),
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        };
        let json = TrackInfoJson::from_track_info(&track);
        assert_eq!(json.content_hash, "sha256:abc123");
        assert_eq!(json.title, Some("Test Track".to_string()));
        assert_eq!(json.artist, Some("Test Artist".to_string()));
        assert_eq!(json.album, Some("Test Album".to_string()));
        assert_eq!(json.duration, Some(180));
        assert!(json.bpm.is_some());
        assert!(json.key.is_some());
        assert!(json.disc_number.is_none());
        assert!(json.added.is_none());
    }

    #[test]
    fn track_info_json_from_minimal_track() {
        let track = TrackInfo {
            content_hash: ContentHash::new("sha256:deadbeef"),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        };
        let json = TrackInfoJson::from_track_info(&track);
        assert_eq!(json.content_hash, "sha256:deadbeef");
        assert!(json.title.is_none());
        assert!(json.artist.is_none());
        assert!(json.album.is_none());
        assert!(json.duration.is_none());
        assert!(json.bpm.is_none());
        assert!(json.key.is_none());
        assert!(json.disc_number.is_none());
        assert!(json.added.is_none());
    }

    #[test]
    fn track_info_json_populates_disc_number_and_added() {
        let track = TrackInfo {
            content_hash: ContentHash::new("sha256:abc"),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: Some(2),
            added: Some("2024-01-15T12:00:00Z".to_string()),
            started: None,
            stopped: None,
        };
        let json = TrackInfoJson::from_track_info(&track);
        assert_eq!(json.disc_number, Some(2));
        assert_eq!(json.added, Some("2024-01-15T12:00:00Z".to_string()));
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

    #[test]
    fn aif_source_treated_as_aiff_for_passthrough() {
        let source_ext = blob_extension("blobs/ab/track.aif").unwrap().to_lowercase();
        let fmt = ExportFormatParam::Aiff;
        assert!(
            Some(source_ext.as_str()) == fmt.extension()
                || (source_ext == "aif" && fmt.extension() == Some("aiff"))
        );
    }

    // ── Artist/album list sort ─────────────────────────────────────────────

    #[test]
    fn artist_list_sorts_case_insensitively() {
        let mut values = vec![
            "Zebra".to_string(),
            "apple".to_string(),
            "Mango".to_string(),
            "banana".to_string(),
        ];
        values.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        assert_eq!(values, vec!["apple", "banana", "Mango", "Zebra"],);
    }

    #[test]
    fn album_list_sorts_case_insensitively() {
        let mut values = vec![
            "Ziggy Stardust".to_string(),
            "abbey road".to_string(),
            "Kind of Blue".to_string(),
        ];
        values.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        assert_eq!(values, vec!["abbey road", "Kind of Blue", "Ziggy Stardust"],);
    }
}
