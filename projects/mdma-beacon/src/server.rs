use crate::config::Config;
use crate::hardware::HardwareInfo;
use axum::{
    routing::{get, post},
    Router,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tower_http::services::ServeDir;
use tracing::{info, warn};

use crate::routes;

/// Application state shared across handlers
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) hardware: Arc<Mutex<HardwareInfo>>,
    pub(crate) config: Config,
    /// Path to the live log file (e.g. /var/log/beacon/current)
    pub(crate) log_path: PathBuf,
    /// Oneshot sender to signal provisioning can start (protected by mutex)
    pub(crate) provision_start: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

/// Run the beacon HTTP server
pub async fn run(
    hardware: HardwareInfo,
    config: Config,
    log_path: PathBuf,
) -> color_eyre::Result<()> {
    let state = AppState {
        hardware: Arc::new(Mutex::new(hardware)),
        config: config.clone(),
        log_path,
        provision_start: Arc::new(Mutex::new(None)),
    };

    let app = Router::new()
        .route("/", get(routes::provision::index))
        .route("/provision", post(routes::provision::provision))
        .route("/provision/start", post(routes::provision::provision_start))
        .route("/update", post(routes::update::update_beacon))
        .route("/stream", get(routes::provision::stream_events))
        .route("/test-stream", get(routes::provision::test_stream))
        .route("/logs", get(routes::logs::index))
        .route("/logs/:name", get(routes::logs::view))
        .route("/logs/:name/page/:n", get(routes::logs::view_page))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    if config.is_check_mode() {
        info!(
            "Beacon server listening on http://localhost:{}",
            config.port
        );
        info!("   CHECK mode - no changes will be made to your system");
        info!("   Use --apply flag to actually provision");
        info!("   Test SSE: http://localhost:{}/test-stream", config.port);
    } else {
        info!("Beacon server listening on http://welcome-to-mdma.local");
        info!("   APPLY mode - changes WILL be made!");
        info!("   Also accessible via http://0.0.0.0:{}", config.port);
    }

    // Wait for system clock to sync — TLS cert verification requires a sane
    // date before stage 4 can fetch repodata over HTTPS. chronyd is enabled
    // by the SD's runit setup; wait up to 30 tries (~60s) for it to sync.
    info!("Waiting for system clock to sync (chronyc waitsync)...");
    let sync_result = tokio::task::spawn_blocking(|| {
        std::process::Command::new("chronyc")
            .args(["waitsync", "30", "0", "0", "0"])
            .status()
    })
    .await;
    match sync_result {
        Ok(Ok(s)) if s.success() => info!("Clock synced via chronyc"),
        Ok(Ok(s)) => warn!(
            "chronyc waitsync exited {} — proceeding anyway; provisioning may fail on TLS",
            s
        ),
        Ok(Err(e)) => warn!(
            "chronyc not available ({}); proceeding without sync — TLS may fail in stage 4",
            e
        ),
        Err(e) => warn!("spawn_blocking for chronyc failed ({}); continuing", e),
    }

    http_server::serve(app, &http_server::HttpServerConfig { port: config.port }).await?;

    Ok(())
}
