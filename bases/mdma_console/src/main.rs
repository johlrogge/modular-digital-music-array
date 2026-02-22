//! MDMA Console - Web interface for package, inbox, and bandcamp management

use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use clap::Parser;
use color_eyre::Result;
use gateway_client::GatewayClient;
use library_ipc_client::LibraryClient;
use source_protocol::{DownloadState, SourceRequest, SourceResponse};
use std::sync::Arc;
use tokio::sync::Mutex;

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
}

impl AppState {
    fn new(library_socket: String, gateway: String, source_name: String) -> Self {
        Self {
            packages: Mutex::new(Vec::new()),
            updates: Mutex::new(Vec::new()),
            library_socket,
            gateway,
            source_name,
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
    has_update: bool,
}

impl PackageView {
    fn from_installed(pkg: &InstalledPackage, updates: &[AvailableUpdate]) -> Self {
        let update = updates.iter().find(|u| u.name == pkg.name);
        Self {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            update_version: update.map(|u| u.new_version.clone()),
            has_update: update.is_some(),
        }
    }
}

/// Bandcamp status view for template
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
            _ => Self {
                connected: false,
                authenticated: false,
                downloads_active: 0,
                downloads_queued: 0,
                downloads_completed: 0,
                downloads_failed: 0,
                paused: false,
            },
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
// Bandcamp Handlers
// =============================================================================

#[derive(serde::Serialize)]
struct BandcampStatusJson {
    connected: bool,
    authenticated: bool,
    downloads_active: usize,
    downloads_queued: usize,
    downloads_completed: usize,
    downloads_failed: usize,
    paused: bool,
}

async fn bandcamp_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.gateway_client() {
        Some(client) => {
            match client.source_request(&state.source_name, &SourceRequest::GetStatus) {
                Ok(SourceResponse::Status(status)) => Json(BandcampStatusJson {
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
        None => Json(BandcampStatusJson {
            connected: false,
            authenticated: false,
            downloads_active: 0,
            downloads_queued: 0,
            downloads_completed: 0,
            downloads_failed: 0,
            paused: false,
        })
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
    match state.gateway_client() {
        Some(client) => match client.source_request(&state.source_name, &SourceRequest::Sync) {
            Ok(SourceResponse::SyncStarted {
                total_items,
                new_items,
            }) => Json(serde_json::json!({
                "success": true,
                "total_items": total_items,
                "new_items": new_items
            }))
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
        },
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Gateway not available"})),
        )
            .into_response(),
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
    match state.gateway_client() {
        Some(client) => {
            match client.source_request(&state.source_name, &SourceRequest::ListDownloads) {
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
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Gateway not available"})),
        )
            .into_response(),
    }
}

async fn bandcamp_pause(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.gateway_client() {
        Some(client) => match client.source_request(&state.source_name, &SourceRequest::PauseAll) {
            Ok(SourceResponse::Paused) => {
                Json(serde_json::json!({"success": true})).into_response()
            }
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
        },
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Gateway not available"})),
        )
            .into_response(),
    }
}

async fn bandcamp_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.gateway_client() {
        Some(client) => {
            match client.source_request(&state.source_name, &SourceRequest::ResumeAll) {
                Ok(SourceResponse::Resumed) => {
                    Json(serde_json::json!({"success": true})).into_response()
                }
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
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Gateway not available"})),
        )
            .into_response(),
    }
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

    let state = Arc::new(AppState::new(
        args.library_socket,
        args.gateway,
        args.source_name,
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
        .with_state(state);

    let addr = format!("0.0.0.0:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("MDMA Console listening on http://0.0.0.0:{}", args.port);

    axum::serve(listener, app).await?;

    Ok(())
}
