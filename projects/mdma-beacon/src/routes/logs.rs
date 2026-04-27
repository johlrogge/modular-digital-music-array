use crate::log_archive;
use crate::server::AppState;
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use std::time::{SystemTime, UNIX_EPOCH};

const PAGE_SIZE: usize = 1000;

/// A log file entry for the index template
#[derive(Debug)]
struct LogFileEntry {
    name: String,
    size_bytes: u64,
    modified_display: String,
}

fn format_system_time(t: SystemTime) -> String {
    // Simple formatting without chrono — seconds since UNIX_EPOCH formatted as UTC
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => format_unix_secs(d.as_secs()),
        Err(_) => "unknown".to_string(),
    }
}

/// Formats a UNIX timestamp as "YYYY-MM-DD HH:MM:SS UTC" without external deps.
fn format_unix_secs(secs: u64) -> String {
    // Days since epoch
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400;

    // Compute year/month/day from days since 1970-01-01
    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} UTC")
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[derive(Template)]
#[template(path = "logs_index.html")]
struct LogsIndexTemplate {
    files: Vec<LogFileEntry>,
}

#[derive(Template)]
#[template(path = "logs_view.html")]
struct LogsViewTemplate {
    name: String,
    lines: Vec<String>,
    page: usize,
    has_more: bool,
    line_start: usize,
    line_end: usize,
}

fn not_found(msg: impl std::fmt::Display) -> Response {
    let body = format!(
        r#"<!DOCTYPE html>
<html>
<head><title>Not Found</title></head>
<body>
    <h1>Not Found</h1>
    <p>{msg}</p>
    <a href="/logs">Back to logs</a>
</body>
</html>"#
    );
    (StatusCode::NOT_FOUND, Html(body)).into_response()
}

fn template_error(msg: impl std::fmt::Display) -> Response {
    let body = format!(
        r#"<!DOCTYPE html>
<html>
<head><title>Error</title></head>
<body>
    <h1>Error</h1>
    <p>{msg}</p>
    <a href="/logs">Back to logs</a>
</body>
</html>"#
    );
    (StatusCode::INTERNAL_SERVER_ERROR, Html(body)).into_response()
}

/// GET /logs — list all log files
pub async fn index(State(state): State<AppState>) -> Response {
    let log_dir = state
        .log_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let files = match tokio::task::spawn_blocking(move || log_archive::list(&log_dir)).await {
        Ok(Ok(files)) => files,
        Ok(Err(_)) => Vec::new(), // dir doesn't exist yet — show empty list
        Err(_) => Vec::new(),
    };

    let entries: Vec<LogFileEntry> = files
        .into_iter()
        .map(|f| LogFileEntry {
            name: f.name,
            size_bytes: f.size_bytes,
            modified_display: format_system_time(f.modified),
        })
        .collect();

    let template = LogsIndexTemplate { files: entries };
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => template_error(e),
    }
}

/// GET /logs/:name — view first page of a log file
pub async fn view(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    view_page_inner(state, name, 0).await
}

/// GET /logs/:name/page/:n — view page N of a log file
pub async fn view_page(
    State(state): State<AppState>,
    Path((name, page)): Path<(String, usize)>,
) -> Response {
    view_page_inner(state, name, page).await
}

async fn view_page_inner(state: AppState, name: String, page: usize) -> Response {
    if !log_archive::is_svlogd_log_name(&name) {
        return not_found(format!("Invalid log file name: {name}"));
    }

    let log_dir = state
        .log_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let path = log_archive::log_file_path(&log_dir, &name);
    let offset = page * PAGE_SIZE;

    let (lines, has_more) =
        match tokio::task::spawn_blocking(move || log_archive::read_page(&path, offset, PAGE_SIZE))
            .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => return not_found(format!("Log file not found: {name}")),
            Err(_) => return not_found(format!("Log file not found: {name}")),
        };

    let line_start = offset + 1;
    let line_end = offset + lines.len();

    let template = LogsViewTemplate {
        name,
        lines,
        page,
        has_more,
        line_start,
        line_end,
    };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => template_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::hardware::HardwareInfo;
    use crate::server::AppState;
    use axum::{body::Body, http::Request, Router};
    use std::io::Write;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    fn make_state(log_path: std::path::PathBuf) -> AppState {
        let hardware = HardwareInfo {
            model: "test".to_string(),
            serial: None,
            memory_mb: None,
            nvme_drives: vec![],
        };
        let config = Config::from_args(crate::config::CliArgs {
            port: Some(8080),
            apply: false,
            check: true,
            log_file: log_path.clone(),
        });
        AppState {
            hardware: Arc::new(Mutex::new(hardware)),
            config,
            log_path,
            provision_start: Arc::new(Mutex::new(None)),
        }
    }

    fn make_router(state: AppState) -> Router {
        use axum::routing::get;
        Router::new()
            .route("/logs", get(super::index))
            .route("/logs/:name", get(super::view))
            .route("/logs/:name/page/:n", get(super::view_page))
            .with_state(state)
    }

    #[tokio::test]
    async fn logs_index_renders() {
        let dir = tempdir().unwrap();
        let current = dir.path().join("current");
        let rotated = dir.path().join("@4000000067abc123.s");
        std::fs::write(&current, b"line1\n").unwrap();
        std::fs::write(&rotated, b"old line\n").unwrap();

        let log_path = current.clone();
        let state = make_state(log_path);
        let app = make_router(state);

        let req = Request::builder().uri("/logs").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("current"), "should list 'current'");
        assert!(
            text.contains("@4000000067abc123.s"),
            "should list '@4000000067abc123.s'"
        );
    }

    #[tokio::test]
    async fn logs_view_renders() {
        let dir = tempdir().unwrap();
        let current = dir.path().join("current");
        let mut f = std::fs::File::create(&current).unwrap();
        for i in 0..50 {
            writeln!(f, "logline {i}").unwrap();
        }

        let state = make_state(current.clone());
        let app = make_router(state);

        let req = Request::builder()
            .uri("/logs/current")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("logline 0"), "should contain first line");
        assert!(text.contains("logline 49"), "should contain last line");
    }

    #[tokio::test]
    async fn logs_view_rejects_traversal() {
        let dir = tempdir().unwrap();
        let current = dir.path().join("current");
        std::fs::write(&current, b"secret\n").unwrap();

        let state = make_state(current.clone());
        let app = make_router(state);

        // axum URL-decodes path segments, so we test both encoded and raw
        let req = Request::builder()
            .uri("/logs/..%2Fetc%2Fpasswd")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // axum rejects path traversal at the routing layer with 400/404
        assert!(
            resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST,
            "expected 404 or 400, got {}",
            resp.status()
        );
    }
}
