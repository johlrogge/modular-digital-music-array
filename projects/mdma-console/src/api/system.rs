//! Node-level reads for the settings pages. `GET /api/inbox` lists the files waiting in the inbox
//! (the same queue the classic page renders), as paths relative to the inbox.

use super::{gw, json_or_failure, library, unexpected_library_response};
use crate::AppState;
use axum::{extract::State, response::Response};
use gateway_client::{LibraryRequest, LibraryResponse};
use std::sync::Arc;

/// `GET /api/inbox`: the paths waiting in the inbox, as the library lists them.
pub async fn inbox(State(state): State<Arc<AppState>>) -> Response {
    json_or_failure(
        gw(&state, |client| {
            match library(client, &LibraryRequest::GetInboxQueue)? {
                LibraryResponse::InboxQueue(paths) => {
                    Ok(paths.into_iter().map(String::from).collect::<Vec<_>>())
                }
                _ => Err(unexpected_library_response()),
            }
        })
        .await,
    )
}

#[cfg(test)]
mod tests {
    use crate::test_support::{
        body_json, library_ok, send_json, test_app, test_app_with, FakeGateway,
    };
    use axum::http::StatusCode;
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{InboxPath, ProtocolError};
    use serde_json::json;

    fn inbox_gateway(answer: LibraryResponse) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::GetInboxQueue,
            } => library_ok(answer.clone()),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    #[tokio::test]
    async fn the_inbox_is_the_librarys_queue_as_paths() {
        let gw = inbox_gateway(LibraryResponse::InboxQueue(vec![
            InboxPath::new("a.flac").unwrap(),
            InboxPath::new("sub/b.mp3").unwrap(),
        ]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!(["a.flac", "sub/b.mp3"]));
    }

    #[tokio::test]
    async fn an_empty_inbox_is_an_empty_list() {
        let gw = inbox_gateway(LibraryResponse::InboxQueue(vec![]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!([]));
    }

    #[tokio::test]
    async fn an_answer_that_is_not_the_queue_is_502() {
        let gw = inbox_gateway(LibraryResponse::Pong);
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn a_library_failure_is_classified() {
        let gw = inbox_gateway(LibraryResponse::Error(ProtocolError::Internal {
            message: "boom".into(),
        }));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn without_a_gateway_it_is_503() {
        let res = send_json(test_app(), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
