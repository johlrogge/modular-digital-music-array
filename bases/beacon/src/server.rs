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
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tracing::info;

/// Application state shared across handlers
#[derive(Clone)]
struct AppState {
    hardware: Arc<Mutex<HardwareInfo>>,
    config: Config,
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
    let state = AppState {
        hardware: Arc::new(Mutex::new(hardware)),
        config: config.clone(),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/provision", post(provision))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await?;
    
    if config.is_check_mode() {
        info!("🔍 Beacon server listening on http://localhost:{}", config.port);
        info!("   CHECK mode - no changes will be made to your system");
        info!("   Use --apply flag to actually provision");
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
    
    // Spawn provisioning in background
    tokio::spawn(async move {
        match provisioning::provision_system(config, hardware, execution_mode).await {
            Ok(()) => {
                info!("Provisioning completed successfully");
                // TODO: Trigger reboot in production mode
            }
            Err(e) => {
                tracing::error!("Provisioning failed: {}", e);
            }
        }
    });
    
    let mode_notice = if execution_mode == crate::actions::ExecutionMode::DryRun {
        "<div class='dev-notice'><strong>🔍 CHECK MODE:</strong> No changes were made to your system. Check the logs to see what would have been done. Run with <code>--apply</code> flag to actually provision.</div>"
    } else {
        ""
    };
    
    let html = format!(r#"
    <!DOCTYPE html>
    <html>
    <head>
        <title>Provisioning Started</title>
        <style>
            body {{ font-family: sans-serif; max-width: 800px; margin: 50px auto; padding: 20px; }}
            .success {{ color: #27ae60; }}
            .dev-notice {{ background: #fff3cd; border: 2px solid #ffc107; padding: 15px; margin: 20px 0; border-radius: 6px; }}
        </style>
    </head>
    <body>
        <h1 class="success">✓ Provisioning Started</h1>
        {mode_notice}
        <p>Your MDMA unit is being configured. This will take several minutes.</p>
        <p>The system will reboot automatically when complete.</p>
        <p>After reboot, access your unit via SSH at the hostname you configured.</p>
        <pre>ssh root@your-hostname.local</pre>
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
