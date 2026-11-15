//! Shared helpers for handler tests: the real `app()` on an `AppState`, optionally wired to a
//! fake gateway that answers from a script and records everything it was asked.

use crate::AppState;
use axum::{body::Body, http::Request, response::Response, Router};
use gateway_client::{Command, GatewayRequest, GatewayResponse};
use nng::options::{Options, RecvTimeout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tower::ServiceExt;

const DEAD_GATEWAY: &str = "ipc:///nonexistent/gateway.sock";

/// The whole application with a dead gateway. Fine for routes that never call it.
pub fn test_app() -> Router {
    test_app_with(DEAD_GATEWAY)
}

/// The whole application with its gateway client pointed at `gateway_addr`.
pub fn test_app_with(gateway_addr: &str) -> Router {
    let (tx, _) = tokio::sync::broadcast::channel(4);
    let state = Arc::new(AppState::new(
        "ipc:///nonexistent/library.sock".into(),
        gateway_addr.into(),
        "bandcamp".into(),
        tx,
        std::path::PathBuf::from("/nonexistent"),
        std::path::PathBuf::from("/nonexistent/cookies"),
        std::path::PathBuf::from("/nonexistent/conf"),
    ));
    crate::app(state)
}

/// GET `path` on `app`, optionally asking for an encoding (e.g. "gzip").
pub async fn send_get(app: Router, path: &str, accept_encoding: Option<&str>) -> Response {
    let mut req = Request::get(path);
    if let Some(enc) = accept_encoding {
        req = req.header("accept-encoding", enc);
    }
    app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
}

/// POST a JSON body to `path` on `app`.
pub async fn send_post_json(app: Router, path: &str, json: &serde_json::Value) -> Response {
    let req = Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(json).unwrap()))
        .unwrap();
    app.oneshot(req).await.unwrap()
}

/// Parsed JSON body of a response.
pub async fn body_json(res: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Body of a response as text.
pub async fn body_text(res: Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// A successful playback answer carrying `data`.
pub fn playback_ok(data: Option<media_protocol::ResponseData>) -> GatewayResponse {
    GatewayResponse::Playback {
        response: gateway_client::Response::Ok { data },
    }
}

/// A playback service error.
pub fn playback_err(message: &str) -> GatewayResponse {
    GatewayResponse::Playback {
        response: gateway_client::Response::Err {
            message: message.to_string(),
        },
    }
}

/// A library answer.
pub fn library_ok(response: gateway_client::LibraryResponse) -> GatewayResponse {
    GatewayResponse::Library { response }
}

/// Short form of a playback command for asserting on recorded traffic: `now`, `list`,
/// `next <hash>`, `append <hash>`, `play`, `replace <hash>,<hash>`; anything else as `{:?}`.
pub fn describe(command: &Command) -> String {
    match command {
        Command::NowPlaying => "now".into(),
        Command::QueueList => "list".into(),
        Command::PlayQueue => "play".into(),
        Command::QueueNext { hash, .. } => format!("next {}", hash.as_str()),
        Command::QueueAppend { hash, .. } => format!("append {}", hash.as_str()),
        Command::QueueReplace { entries } => format!(
            "replace {}",
            entries
                .iter()
                .map(|(h, _)| h.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => format!("{other:?}"),
    }
}

/// `describe` of every playback command a fake gateway received.
pub fn described(gw: &FakeGateway) -> Vec<String> {
    gw.commands().iter().map(describe).collect()
}

pub async fn get_with_encoding(path: &str, accept_encoding: Option<&str>) -> Response {
    send_get(test_app(), path, accept_encoding).await
}

pub async fn get(path: &str) -> Response {
    get_with_encoding(path, None).await
}

/// A gateway on a temporary IPC socket. Answers every request with the script's response and
/// records the request. Stops and removes its socket when dropped.
pub struct FakeGateway {
    addr: String,
    recorded: Arc<Mutex<Vec<GatewayRequest>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

impl FakeGateway {
    /// Bind the socket and start answering. The socket is listening when this returns.
    pub fn start<F>(mut script: F) -> Self
    where
        F: FnMut(&GatewayRequest) -> GatewayResponse + Send + 'static,
    {
        let dir = tempfile::tempdir().unwrap();
        let addr = format!("ipc://{}/gw.sock", dir.path().display());
        let socket = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        socket
            .set_opt::<RecvTimeout>(Some(Duration::from_millis(50)))
            .unwrap();
        socket.listen(&addr).unwrap();

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let recorded = Arc::clone(&recorded);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let msg = match socket.recv() {
                        Ok(m) => m,
                        Err(_) => continue, // timed out: check the stop flag
                    };
                    let response = match serde_json::from_slice::<GatewayRequest>(&msg) {
                        Ok(req) => {
                            let response = script(&req);
                            recorded.lock().unwrap().push(req);
                            response
                        }
                        Err(e) => GatewayResponse::invalid_request(e),
                    };
                    let bytes = serde_json::to_vec(&response).unwrap();
                    let _ = socket.send(nng::Message::from(&bytes[..]));
                }
            })
        };
        Self {
            addr,
            recorded,
            stop,
            thread: Some(thread),
            _dir: dir,
        }
    }

    /// Address to give `test_app_with`.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<GatewayRequest> {
        self.recorded.lock().unwrap().clone()
    }

    /// The playback commands received so far, in order.
    pub fn commands(&self) -> Vec<Command> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                GatewayRequest::Playback { request } => Some(request),
                _ => None,
            })
            .collect()
    }
}

impl Drop for FakeGateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use gateway_client::Response as PlaybackResponse;
    use media_protocol::ResponseData;

    #[tokio::test]
    async fn fake_gateway_answers_from_script_and_records_commands() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Playback {
                request: Command::GetSession,
            } => GatewayResponse::Playback {
                response: PlaybackResponse::Ok {
                    data: Some(ResponseData::Session(Some("s-1".into()))),
                },
            },
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });

        let res = send_get(test_app_with(gw.addr()), "/player/session", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["session"], "s-1");

        let commands = gw.commands();
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], Command::GetSession));
    }
}
