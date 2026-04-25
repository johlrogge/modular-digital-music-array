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
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast;
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

/// SSE endpoint for streaming provisioning logs
pub async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.log_tx.subscribe();

    let stream = async_stream::stream! {
        // Send initial connection message
        yield Ok(Event::default().data("Connected to provisioning stream"));

        loop {
            match rx.recv().await {
                Ok(msg) => {
                    yield Ok(Event::default().data(msg));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Client fell behind, inform them
                    yield Ok(Event::default().data(format!("⚠️  Skipped {} messages (too slow)", n)));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Channel closed, end stream
                    yield Ok(Event::default().data("Stream closed"));
                    break;
                }
            }
        }
    };

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
    let log_tx = state.log_tx.clone();

    // Spawn provisioning in background, waiting for start signal from JavaScript
    spawn_with_start_signal(&state.provision_start, log_tx, move |log_tx| async move {
        match provisioning::provision_system(config, hardware, execution_mode, log_tx.clone()).await
        {
            Ok(provisioned) => {
                info!("Provisioning completed successfully {provisioned:?}");
            }
            Err(e) => {
                tracing::error!("Provisioning failed: {}", e);
                let _ = log_tx.send(format!("❌ Provisioning failed: {}", e));
            }
        }
    })
    .await;

    let mode_notice = if execution_mode == crate::actions::ExecutionMode::DryRun {
        r#"<div class='dev-notice'><strong>🔍 CHECK MODE:</strong> No changes were made to your system. Watch the log below. Run with <code>--apply</code> flag to actually provision.</div>"#
    } else {
        ""
    };

    let html = format!(
        r#"
    <!DOCTYPE html>
    <html>
    <head>
        <title>Provisioning in Progress</title>
        <meta charset="utf-8">
        <style>
            body {{ font-family: sans-serif; max-width: 800px; margin: 50px auto; padding: 20px; }}
            .success {{ color: #27ae60; }}
            .dev-notice {{ background: #fff3cd; border: 2px solid #ffc107; padding: 15px; margin: 20px 0; border-radius: 6px; }}
            #log-container {{
                background: #1e1e1e;
                color: #d4d4d4;
                font-family: 'Courier New', monospace;
                padding: 20px;
                border-radius: 6px;
                margin: 20px 0;
                height: 400px;
                overflow-y: auto;
                white-space: pre-wrap;
                border: 2px solid #333;
            }}
            .log-line {{
                margin: 4px 0;
                padding: 2px 0;
            }}
            .status {{
                margin: 10px 0;
                padding: 10px;
                background: #f0f0f0;
                border-radius: 4px;
                font-size: 0.9em;
            }}
        </style>
    </head>
    <body>
        <h1 class="success">⏳ Provisioning in Progress</h1>
        {mode_notice}

        <div class="status" id="status">Connecting to stream...</div>

        <p>Live log:</p>
        <div id="log-container"></div>

        <script>
            const logContainer = document.getElementById('log-container');
            const statusDiv = document.getElementById('status');

            // Log to both console and container
            function log(msg, isError) {{
                console.log(msg);
                const line = document.createElement('div');
                line.className = 'log-line';
                if (isError) {{
                    line.style.color = '#ff6b6b';
                }}
                line.textContent = msg;
                logContainer.appendChild(line);
                logContainer.scrollTop = logContainer.scrollHeight;
            }}

            log('Initializing EventSource...');
            statusDiv.textContent = 'Connecting...';

            const eventSource = new EventSource('/stream');

            eventSource.onopen = function() {{
                console.log('EventSource opened');
                statusDiv.textContent = '✓ Connected - Starting provisioning...';
                statusDiv.style.background = '#d4edda';
                log('✓ Connected to stream');

                // Send start signal to server
                fetch('/provision/start', {{ method: 'POST' }})
                    .then(response => {{
                        if (response.ok) {{
                            console.log('Provisioning start signal sent');
                            statusDiv.textContent = '✓ Provisioning started';
                        }} else {{
                            console.error('Failed to start provisioning');
                            statusDiv.textContent = '✗ Failed to start';
                            statusDiv.style.background = '#f8d7da';
                        }}
                    }})
                    .catch(err => {{
                        console.error('Error sending start signal:', err);
                        log('✗ Error starting provisioning', true);
                    }});
            }};

            eventSource.onmessage = function(event) {{
                console.log('Message:', event.data);
                log(event.data);
            }};

            eventSource.onerror = function(error) {{
                console.error('EventSource error:', error);
                console.log('ReadyState:', eventSource.readyState);
                statusDiv.textContent = '✗ Connection error';
                statusDiv.style.background = '#f8d7da';
                log('✗ Connection error (see console)', true);

                if (eventSource.readyState === EventSource.CLOSED) {{
                    log('Stream closed', true);
                }}
            }};
        </script>
    </body>
    </html>
    "#,
        mode_notice = mode_notice
    );

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
