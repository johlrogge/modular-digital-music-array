use crate::config::Config;
use crate::hardware::HardwareInfo;
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, Mutex};
use tower_http::services::ServeDir;
use tracing::info;

use crate::routes;

/// Application state shared across handlers
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) hardware: Arc<Mutex<HardwareInfo>>,
    pub(crate) config: Config,
    /// Broadcast channel for streaming provisioning logs to clients
    pub(crate) log_tx: broadcast::Sender<String>,
    /// Oneshot sender to signal provisioning can start (protected by mutex)
    pub(crate) provision_start: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

/// Run the beacon HTTP server
pub async fn run(hardware: HardwareInfo, config: Config) -> color_eyre::Result<()> {
    // Create broadcast channel for streaming logs (100 message buffer)
    let (log_tx, _rx) = broadcast::channel(100);

    let state = AppState {
        hardware: Arc::new(Mutex::new(hardware)),
        config: config.clone(),
        log_tx,
        provision_start: Arc::new(Mutex::new(None)),
    };

    let app = Router::new()
        .route("/", get(routes::provision::index))
        .route("/provision", post(routes::provision::provision))
        .route("/provision/start", post(routes::provision::provision_start))
        .route("/update", post(routes::update::update_beacon))
        .route("/stream", get(routes::provision::stream_events))
        .route("/test-stream", get(routes::provision::test_stream))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    if config.is_check_mode() {
        info!(
            "🔍 Beacon server listening on http://localhost:{}",
            config.port
        );
        info!("   CHECK mode - no changes will be made to your system");
        info!("   Use --apply flag to actually provision");
        info!("   Test SSE: http://localhost:{}/test-stream", config.port);
    } else {
        info!("⚠️  Beacon server listening on http://welcome-to-mdma.local");
        info!("   APPLY mode - changes WILL be made!");
        info!("   Also accessible via http://0.0.0.0:{}", config.port);
    }

    http_server::serve(app, &http_server::HttpServerConfig { port: config.port }).await?;

    Ok(())
}
