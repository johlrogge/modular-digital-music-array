// bases/beacon/src/server.rs
use crate::config::Config;
use crate::error::BeaconError;
use crate::hardware::HardwareInfo;
use crate::provisioning;
use crate::types::{Hostname, ProvisionConfig, SshPublicKey, UnitType};
use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Form, Router,
};
use futures::stream::{self, Stream};
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tower_http::services::ServeDir;
use tracing::info;

/// Application state shared across handlers
#[derive(Clone)]
struct AppState {
    hardware: Arc<Mutex<HardwareInfo>>,
    config: Config,
    /// Broadcast channel for streaming provisioning logs to clients
    log_tx: broadcast::Sender<String>,
}

/// Main template for the welcome page
#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    hardware: HardwareInfo,
}

/// Provisioning form submission
#[derive(Debug, Deserialize)]
struct ProvisionForm {
    unit_type: String,
    hostname: String,
    ssh_key: String,
}

/// Run the beacon HTTP server
pub async fn run(hardware: HardwareInfo, config: Config) -> color_eyre::Result<()> {
    // Create broadcast channel for streaming logs (100 message buffer)
    let (log_tx, _rx) = broadcast::channel(100);
    
    let state = AppState {
        hardware: Arc::new(Mutex::new(hardware)),
        config: config.clone(),
        log_tx,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/provision", post(provision))
        .route("/stream", get(stream_events))
        .route("/test-stream", get(test_stream))  // Test endpoint!
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await?;
    
    if config.is_check_mode() {
        info!("🔍 Beacon server listening on http://localhost:{}", config.port);
        info!("   CHECK mode - no changes will be made to your system");
        info!("   Use --apply flag to actually provision");
        info!("   Test SSE: http://localhost:{}/test-stream", config.port);
    } else {
        info!("⚠️  Beacon server listening on http://welcome-to-mdma.local");
        info!("   APPLY mode - changes WILL be made!");
        info!("   Also accessible via http://0.0.0.0:{}", config.port);
    }
    
    axum::serve(listener, app).await?;
    
    Ok(())
}

/// Handler for the main page
async fn index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let hardware = state.hardware.lock().await;
    let template = IndexTemplate {
        hardware: hardware.clone(),
    };
    
    let html = template.render()
        .map_err(|e| AppError::Template(e.to_string()))?;
    
    Ok(Html(html))
}

/// TEST endpoint - Simple SSE stream that sends messages every second
async fn test_stream() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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
async fn stream_events(
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
async fn provision(
    State(state): State<AppState>,
    Form(form): Form<ProvisionForm>,
) -> Result<Html<String>, AppError> {
    info!("Received provisioning request: {:?}", form);
    
    // Parse and validate inputs using newtype constructors
    let unit_type = parse_unit_type(&form.unit_type)?;
    let hostname = Hostname::new(form.hostname)
        .map_err(|e| AppError::Validation(format!("Invalid hostname: {}", e)))?;
    let ssh_key = SshPublicKey::new(form.ssh_key)
        .map_err(|e| AppError::Validation(format!("Invalid SSH key: {}", e)))?;
    
    let config = ProvisionConfig {
        unit_type,
        hostname,
        ssh_key,
    };
    
    let hardware = state.hardware.lock().await.clone();
    let execution_mode = state.config.execution_mode;
    let log_tx = state.log_tx.clone();
    
    // Spawn provisioning in background
    tokio::spawn(async move {
        // Send initial message
        let _ = log_tx.send("🚀 Starting provisioning...".to_string());
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // TODO: Call the actual provisioning with log_tx
        // For now, send some test messages
        let _ = log_tx.send(format!("Unit type: {}", config.unit_type));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = log_tx.send(format!("Hostname: {}", config.hostname));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = log_tx.send("Validating hardware...".to_string());
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = log_tx.send("✅ Provisioning completed successfully!".to_string());
    });
    
    let mode_notice = if execution_mode == crate::actions::ExecutionMode::DryRun {
        r#"<div class='dev-notice'><strong>🔍 CHECK MODE:</strong> No changes were made to your system. Watch the log below. Run with <code>--apply</code> flag to actually provision.</div>"#
    } else {
        ""
    };
    
    let html = format!(r#"
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
                statusDiv.textContent = '✓ Connected';
                statusDiv.style.background = '#d4edda';
                log('✓ Connected to stream');
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
    "#, mode_notice = mode_notice);
    
    Ok(Html(html))
}

fn parse_unit_type(s: &str) -> Result<UnitType, AppError> {
    match s {
        "mdma-909" => Ok(UnitType::Mdma909),
        "mdma-101" => Ok(UnitType::Mdma101),
        "mdma-303" => Ok(UnitType::Mdma303),
        _ => Err(AppError::Validation(format!("Unknown unit type: {}", s))),
    }
}

/// Application-level errors for HTTP handlers
#[derive(Debug)]
enum AppError {
    Template(String),
    Validation(String),
    Beacon(BeaconError),
}

impl From<BeaconError> for AppError {
    fn from(err: BeaconError) -> Self {
        AppError::Beacon(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::Template(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Beacon(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        
        let body = format!(
            r#"<!DOCTYPE html>
            <html>
            <head><title>Error</title></head>
            <body>
                <h1>Error</h1>
                <p>{}</p>
                <a href="/">Back to home</a>
            </body>
            </html>"#,
            message
        );
        
        (status, Html(body)).into_response()
    }
}
