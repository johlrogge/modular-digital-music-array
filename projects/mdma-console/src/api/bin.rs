//! The playlist bin for the new web UI (ADR-033 §6): list, restore, purge and empty. The console
//! decides nothing: it parses the id and the optional new name, forwards the request, and answers
//! with what the library answered.
//!
//! - 200: listed (`[{id, name, binned_at_ms, entry_count}]`, newest first), restored (`{name}`),
//!   purged or emptied (`{deleted: true}`);
//! - 404: no such bin entry (`library_failure_status`);
//! - 409: the restore name is taken (`{error}`, by `library_failed`);
//! - 422: a bin id or a restore name that cannot be one; the library is not asked;
//! - 503: no gateway; every other failure is classified by `library_failure_status`.

use super::{fail, forward, Reply};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Json, Response},
};
use gateway_client::{LibraryRequest, LibraryResponse};
use library_ipc_client::{BinId, PlaylistName};
use serde::Deserialize;
use std::sync::Arc;

/// The bin id from a path, or the 422 response for one that cannot be an id (the library is not
/// asked).
#[allow(clippy::result_large_err)]
fn bin_id(id: &str) -> Result<BinId, Response> {
    id.parse::<BinId>()
        .map_err(|e| fail(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

/// The list as the library gave it (it sorts newest first); name and time come from each id.
fn listed(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::BinEntries(entries) => {
            let body: Vec<_> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id.to_string(),
                        "name": e.id.name().as_str(),
                        "binned_at_ms": e.id.binned_at_ms(),
                        "entry_count": e.entry_count,
                    })
                })
                .collect();
            Some((StatusCode::OK, serde_json::Value::Array(body)))
        }
        _ => None,
    }
}

fn restored(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::PlaylistRestored { name } => {
            Some((StatusCode::OK, serde_json::json!({ "name": name.as_str() })))
        }
        _ => None,
    }
}

fn purged(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::Pong => Some((StatusCode::OK, serde_json::json!({ "deleted": true }))),
        _ => None,
    }
}

/// `GET /api/bin`: the binned playlists, newest first.
pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    forward(&state, LibraryRequest::BinList, listed).await
}

#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    #[serde(rename = "as")]
    as_name: Option<String>,
}

/// `POST /api/bin/{id}/restore {as}` (`as` may be null): 200 `{name}`, the name the playlist now
/// has. The library never picks a name; a clash is 409 and the user types another.
pub async fn restore(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<RestoreBody>,
) -> Response {
    let id = match bin_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let as_name = match body.as_name.as_deref().map(PlaylistName::new).transpose() {
        Ok(n) => n,
        Err(e) => return fail(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    };
    let request = LibraryRequest::PlaylistRestore { id, as_name };
    forward(&state, request, restored).await
}

/// `DELETE /api/bin/{id}`: purge one binned playlist for good.
pub async fn purge(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match bin_id(&id) {
        Ok(id) => forward(&state, LibraryRequest::BinPurge { id }, purged).await,
        Err(response) => response,
    }
}

/// `DELETE /api/bin`: purge every binned playlist.
pub async fn empty(State(state): State<Arc<AppState>>) -> Response {
    forward(&state, LibraryRequest::BinEmpty, purged).await
}

#[cfg(test)]
mod tests {
    use crate::test_support::{
        body_json, library_ok, send_json, test_app, test_app_with, FakeGateway,
    };
    use axum::http::StatusCode;
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{BinEntry, BinId, PlaylistName, ProtocolError};
    use serde_json::{json, Value};

    fn id(text: &str) -> BinId {
        text.parse().unwrap()
    }

    /// A library that answers every bin request with `answer`.
    fn bin_gateway(answer: LibraryResponse) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request:
                    LibraryRequest::BinList
                    | LibraryRequest::BinPurge { .. }
                    | LibraryRequest::BinEmpty
                    | LibraryRequest::PlaylistRestore { .. },
            } => library_ok(answer.clone()),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    /// One call of each bin route: method, path and body.
    fn calls() -> Vec<(&'static str, &'static str, Option<Value>)> {
        vec![
            ("GET", "/api/bin", None),
            ("POST", "/api/bin/friday.42/restore", Some(json!({}))),
            ("DELETE", "/api/bin/friday.42", None),
            ("DELETE", "/api/bin", None),
        ]
    }

    #[tokio::test]
    async fn the_bin_keeps_the_librarys_order_and_takes_name_and_time_from_the_id() {
        let gw = bin_gateway(LibraryResponse::BinEntries(vec![
            BinEntry {
                id: id("new.300"),
                entry_count: 5,
            },
            BinEntry {
                id: id("mid.200"),
                entry_count: 0,
            },
            BinEntry {
                id: id("old.100"),
                entry_count: 2,
            },
        ]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/bin", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!([
                {"id": "new.300", "name": "new", "binned_at_ms": 300, "entry_count": 5},
                {"id": "mid.200", "name": "mid", "binned_at_ms": 200, "entry_count": 0},
                {"id": "old.100", "name": "old", "binned_at_ms": 100, "entry_count": 2},
            ])
        );
    }

    #[tokio::test]
    async fn an_empty_bin_is_an_empty_list() {
        let gw = bin_gateway(LibraryResponse::BinEntries(vec![]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/bin", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!([]));
    }

    #[tokio::test]
    async fn a_restore_is_200_with_the_name_and_forwards_the_id_and_as() {
        let gw = bin_gateway(LibraryResponse::PlaylistRestored {
            name: PlaylistName::new("other").unwrap(),
        });
        let res = send_json(
            test_app_with(gw.addr()),
            "POST",
            "/api/bin/friday.42/restore",
            Some(&json!({"as": "other"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({"name": "other"}));
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request: LibraryRequest::PlaylistRestore { id: got, as_name },
            }] => {
                assert_eq!(*got, id("friday.42"));
                assert_eq!(as_name.as_ref().map(|n| n.as_str()), Some("other"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_restore_without_as_sends_no_name() {
        let gw = bin_gateway(LibraryResponse::PlaylistRestored {
            name: PlaylistName::new("friday").unwrap(),
        });
        let res = send_json(
            test_app_with(gw.addr()),
            "POST",
            "/api/bin/friday.42/restore",
            Some(&json!({})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request: LibraryRequest::PlaylistRestore { as_name: None, .. },
            }] => {}
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn purge_and_empty_are_200_deleted() {
        let gw = bin_gateway(LibraryResponse::Pong);
        let all = calls();
        for call in &all[2..] {
            let res = send_json(test_app_with(gw.addr()), call.0, call.1, call.2.as_ref()).await;
            assert_eq!(res.status(), StatusCode::OK, "{}", call.1);
            assert_eq!(body_json(res).await, json!({"deleted": true}));
        }
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request: LibraryRequest::BinPurge { id: got },
            }, GatewayRequest::Library {
                request: LibraryRequest::BinEmpty,
            }] => assert_eq!(*got, id("friday.42")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_bin_entry_is_404() {
        let gw = bin_gateway(LibraryResponse::Error(ProtocolError::BinEntryNotFound {
            id: "friday.42".into(),
        }));
        for call in &calls()[1..3] {
            let res = send_json(test_app_with(gw.addr()), call.0, call.1, call.2.as_ref()).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{}", call.1);
            assert!(body_json(res).await["error"].is_string());
        }
    }

    #[tokio::test]
    async fn a_restore_name_clash_is_409_with_error() {
        let gw = bin_gateway(LibraryResponse::Error(
            ProtocolError::PlaylistAlreadyExists {
                name: "friday".into(),
            },
        ));
        let res = send_json(
            test_app_with(gw.addr()),
            "POST",
            "/api/bin/friday.42/restore",
            Some(&json!({})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn an_invalid_id_or_name_is_422_and_the_library_is_not_asked() {
        let gw = bin_gateway(LibraryResponse::Pong);
        for (method, path, body) in [
            ("POST", "/api/bin/nodot/restore", Some(json!({}))),
            ("POST", "/api/bin/bad%20name.42/restore", Some(json!({}))),
            ("POST", "/api/bin/friday.04/restore", Some(json!({}))),
            ("DELETE", "/api/bin/friday.x", None),
            (
                "POST",
                "/api/bin/friday.42/restore",
                Some(json!({"as": "bad name"})),
            ),
        ] {
            let res = send_json(test_app_with(gw.addr()), method, path, body.as_ref()).await;
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{path}");
            assert!(body_json(res).await["error"].is_string());
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn an_answer_that_is_not_the_one_asked_for_is_502() {
        let gw = bin_gateway(LibraryResponse::PlaylistDeleted);
        for call in &calls() {
            let res = send_json(test_app_with(gw.addr()), call.0, call.1, call.2.as_ref()).await;
            assert_eq!(res.status(), StatusCode::BAD_GATEWAY, "{}", call.1);
        }
    }

    #[tokio::test]
    async fn without_a_gateway_every_bin_route_is_503() {
        for call in &calls() {
            let res = send_json(test_app(), call.0, call.1, call.2.as_ref()).await;
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{}", call.1);
        }
    }
}
