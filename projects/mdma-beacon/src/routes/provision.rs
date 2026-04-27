use crate::hardware::HardwareInfo;
use crate::provisioning;
use crate::provisioning::types::{ProvisionConfig, UnitType};
use crate::server::AppState;
use crate::types::{Hostname, SshPublicKey};
use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html,
    },
    Form,
};
use futures::stream::Stream;
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tracing::info;

use super::{spawn_with_start_signal, AppError};

/// Main template for the welcome page
#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    hardware: HardwareInfo,
    version: String,
    build_time: &'static str,
}

/// Template for the provisioning progress page
#[derive(Template)]
#[template(path = "provision.html")]
struct ProvisionPageTemplate {
    is_dry_run: bool,
}

/// Provisioning form submission
#[derive(Debug, Deserialize)]
pub(crate) struct ProvisionForm {
    unit_type: String,
    hostname: String,
    ssh_key: String,
}

/// Handler for the main page
pub async fn index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let hardware = state.hardware.lock().await;
    let template = IndexTemplate {
        hardware: hardware.clone(),
        version: crate::update::full_version(),
        build_time: crate::update::build_timestamp(),
    };

    let html = template
        .render()
        .map_err(|e| AppError::Template(e.to_string()))?;

    Ok(Html(html))
}

/// TEST endpoint - Simple SSE stream that sends messages every second
pub async fn test_stream() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        for i in 1..=10 {
            yield Ok(Event::default().data(format!("Test message {}", i)));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        yield Ok(Event::default().data("Stream complete!"));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// SSE endpoint for streaming provisioning logs — tails the on-disk log file.
pub async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = crate::log_tail::follow(state.log_path.clone(), 5000)
        .map(|line| Ok::<Event, Infallible>(Event::default().data(line)));

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Handler for provisioning request
pub async fn provision(
    State(state): State<AppState>,
    Form(form): Form<ProvisionForm>,
) -> Result<Html<String>, AppError> {
    info!("Received provisioning request: {:?}", form);

    // Parse and validate inputs using newtype constructors
    let unit_type = form.unit_type.parse::<UnitType>().map_err(AppError::from)?;
    let hostname = Hostname::new(form.hostname)?;
    let ssh_key = SshPublicKey::new(form.ssh_key)?;

    let config = ProvisionConfig {
        ssh_key,
        unit_type,
        hostname,
    };

    let hardware = state.hardware.lock().await.clone();
    let execution_mode = state.config.execution_mode;

    // Spawn provisioning in background, waiting for start signal from JavaScript
    spawn_with_start_signal(&state.provision_start, move || async move {
        match provisioning::provision_system(config, hardware, execution_mode).await {
            Ok(provisioned) => {
                info!("Provisioning completed successfully {provisioned:?}");
            }
            Err(e) => {
                tracing::error!("Provisioning failed: {}", e);
            }
        }
    })
    .await;

    let is_dry_run = execution_mode == crate::actions::ExecutionMode::DryRun;
    let html = ProvisionPageTemplate { is_dry_run }
        .render()
        .map_err(|e| AppError::Template(e.to_string()))?;

    Ok(Html(html))
}

/// Handler for provision start signal from JavaScript
pub async fn provision_start(State(state): State<AppState>) -> Result<StatusCode, AppError> {
    // Take the sender out of the mutex and signal start
    if let Some(tx) = state.provision_start.lock().await.take() {
        let _ = tx.send(());
        info!("Provisioning start signal received");
        Ok(StatusCode::OK)
    } else {
        tracing::warn!("Provision start called but no provisioning task waiting");
        Err(AppError::Validation(
            "No provisioning task waiting".to_string(),
        ))
    }
}
