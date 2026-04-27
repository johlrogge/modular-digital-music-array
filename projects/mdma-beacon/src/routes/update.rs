use crate::server::AppState;
use askama::Template;
use axum::{extract::State, response::Html};
use tracing::info;

use super::{spawn_with_start_signal, AppError};

/// Template for the beacon update progress page
#[derive(Template)]
#[template(path = "update.html")]
struct UpdatePageTemplate {}

/// Handler for beacon self-update request
pub async fn update_beacon(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    info!("Received beacon update request");

    // Spawn update in background, waiting for start signal from JavaScript
    spawn_with_start_signal(&state.provision_start, || async move {
        match crate::update::update_beacon_from_repo().await {
            Ok(()) => {
                info!("Beacon update completed successfully");
            }
            Err(e) => {
                tracing::error!("Beacon update failed: {}", e);
            }
        }
    })
    .await;

    let html = UpdatePageTemplate {}
        .render()
        .map_err(|e| AppError::Template(e.to_string()))?;

    Ok(Html(html))
}
