pub(crate) mod boot_order;
pub(crate) mod logs;
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
use tokio::sync::{oneshot, Mutex};

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
            escape_html(&message)
        );

        (status, Html(body)).into_response()
    }
}

/// Escape text for interpolation into HTML element content or attributes.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

/// Spawn a background task that waits for a start signal before doing work.
///
/// Both the provision and update handlers share the same pattern:
/// 1. Create a oneshot channel.
/// 2. Store the sender so the `/provision/start` endpoint can fire it.
/// 3. Spawn a task that blocks until the signal arrives, then calls `f`.
///
/// All progress is emitted via `tracing::info!` / `error!` — the SSE endpoint
/// tails the on-disk log file rather than a broadcast channel.
pub(crate) async fn spawn_with_start_signal<F, Fut>(
    provision_start: &Arc<Mutex<Option<oneshot::Sender<()>>>>,
    f: F,
) where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (start_tx, start_rx) = oneshot::channel::<()>();
    *provision_start.lock().await = Some(start_tx);
    tokio::spawn(async move {
        if start_rx.await.is_err() {
            tracing::error!("Start signal channel closed unexpectedly");
            return;
        }
        f().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_neutralises_markup() {
        assert_eq!(
            escape_html(r#"<script>alert("x")</script> & 'y'"#),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#x27;y&#x27;"
        );
    }

    #[test]
    fn escape_html_leaves_plain_text_alone() {
        assert_eq!(
            escape_html("Unknown boot target 'sd'?"),
            "Unknown boot target &#x27;sd&#x27;?"
        );
        assert_eq!(escape_html("plain text 123"), "plain text 123");
    }

    #[tokio::test]
    async fn error_response_body_is_escaped() {
        let resp = AppError::Validation("<img src=x onerror=alert(1)>".to_string()).into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!body.contains("<img"), "{body}");
        assert!(body.contains("&lt;img"), "{body}");
    }
}
