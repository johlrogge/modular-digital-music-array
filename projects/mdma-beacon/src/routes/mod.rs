pub(crate) mod provision;
pub(crate) mod update;

use crate::error::BeaconError;
use crate::types::ValidationError;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, Mutex};

/// Application-level errors for HTTP handlers
#[derive(Debug)]
pub(crate) enum AppError {
    Template(String),
    Validation(String),
    Beacon(BeaconError),
}

impl From<BeaconError> for AppError {
    fn from(err: BeaconError) -> Self {
        AppError::Beacon(err)
    }
}

impl From<ValidationError> for AppError {
    fn from(err: ValidationError) -> Self {
        AppError::Validation(err.to_string())
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

/// Spawn a background task that waits for a start signal before doing work.
///
/// Both the provision and update handlers share the same pattern:
/// 1. Create a oneshot channel.
/// 2. Store the sender so the `/provision/start` endpoint can fire it.
/// 3. Spawn a task that blocks until the signal arrives, then calls `f`.
///
/// `f` receives the broadcast sender and is responsible for logging any
/// errors it encounters (it returns `()`).
pub(crate) async fn spawn_with_start_signal<F, Fut>(
    provision_start: &Arc<Mutex<Option<oneshot::Sender<()>>>>,
    log_tx: broadcast::Sender<String>,
    f: F,
) where
    F: FnOnce(broadcast::Sender<String>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (start_tx, start_rx) = oneshot::channel::<()>();
    *provision_start.lock().await = Some(start_tx);
    tokio::spawn(async move {
        if start_rx.await.is_err() {
            tracing::error!("Start signal channel closed unexpectedly");
            return;
        }
        f(log_tx).await;
    });
}
