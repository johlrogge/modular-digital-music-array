//! Playlist writes for the new web UI (ADR-027 §9): edit, revert (undo) and delete, each sent to
//! the library with the version the browser last read. The console decides nothing about a
//! playlist: it forwards the ops and the version, and answers with what the library answered.
//!
//! - 200: applied (`{snapshot, skipped, previous}`) or deleted (`{deleted: true}`);
//! - 409: the version was stale, nothing was written (`{current}`, the playlist as it is now);
//! - every other failure is classified by `library_failure_status`, the one policy of ADR-026 A7.
//!   There is no fallback to PlaylistReplace: an old node answers 502.

use super::library::{playlist_name, ApiSnapshot};
use super::{gw, library, library_failed, respond, unexpected_library_response, Reply};
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Json, Response},
};
use gateway_client::{GatewayClient, LibraryRequest, LibraryResponse};
use library_ipc_client::{
    EditOp, EditOutcome, PlaylistName, PlaylistSnapshot, PlaylistVersion, StoredPlaylist,
};
use serde::Deserialize;
use std::sync::Arc;

/// 409 `{current}`: the version was stale and nothing was written.
fn conflict(current: &PlaylistSnapshot) -> Reply {
    (
        StatusCode::CONFLICT,
        serde_json::json!({ "current": ApiSnapshot::from(current) }),
    )
}

/// How a write's answer becomes a reply: `None` for an answer that is not one of the write's
/// outcomes.
type Interpret = fn(LibraryResponse) -> Option<Reply>;

/// The answer to an edit or a revert: applied (200) or conflict (409).
fn edited(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::PlaylistEdited(EditOutcome::Applied {
            snapshot,
            skipped,
            previous,
        }) => Some((
            StatusCode::OK,
            serde_json::json!({
                "snapshot": ApiSnapshot::from(&snapshot),
                "skipped": skipped,
                "previous": previous,
            }),
        )),
        LibraryResponse::PlaylistEdited(EditOutcome::Conflict { current }) => {
            Some(conflict(&current))
        }
        _ => None,
    }
}

/// The answer to a delete: deleted (200) or conflict (409).
fn deleted(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::PlaylistDeleted => {
            Some((StatusCode::OK, serde_json::json!({ "deleted": true })))
        }
        LibraryResponse::PlaylistDeleteConflict { current } => Some(conflict(&current)),
        _ => None,
    }
}

/// Send `request` and say what the library answered, as `interpret` reads it. Any other answer
/// is a 502, as is every library failure ([`library_failed`]).
fn written(client: &GatewayClient, request: &LibraryRequest, interpret: Interpret) -> Reply {
    match library(client, request) {
        Ok(response) => {
            interpret(response).unwrap_or_else(|| library_failed(&unexpected_library_response()))
        }
        Err(e) => library_failed(&e),
    }
}

/// The one path of every write: `name` from the path is checked ([`playlist_name`]), then
/// `request` is built for it and sent.
async fn write(
    state: Arc<AppState>,
    name: String,
    interpret: Interpret,
    request: impl FnOnce(PlaylistName) -> LibraryRequest + Send + 'static,
) -> Response {
    let name = match playlist_name(&name) {
        Ok(n) => n,
        Err(response) => return response,
    };
    respond(
        gw(&state, move |client| {
            written(client, &request(name), interpret)
        })
        .await,
    )
}

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    version: PlaylistVersion,
    ops: Vec<EditOp>,
}

/// `POST /api/playlists/{name}/edit {version, ops}`: the ops go to the library unchanged.
pub async fn edit(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<EditRequest>,
) -> Response {
    write(state, name, edited, move |name| {
        LibraryRequest::PlaylistEdit {
            name,
            version: req.version,
            ops: req.ops,
        }
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct RevertRequest {
    version: PlaylistVersion,
    /// The file as it was, from an earlier answer's `previous`; written back verbatim.
    content: StoredPlaylist,
}

/// `POST /api/playlists/{name}/revert {version, content}`: undo. A conflict leaves the playlist
/// alone: an undo never overwrites someone else's edit.
pub async fn revert(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<RevertRequest>,
) -> Response {
    write(state, name, edited, move |name| {
        LibraryRequest::PlaylistRevert {
            name,
            version: req.version,
            content: req.content,
        }
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    version: PlaylistVersion,
}

/// `DELETE /api/playlists/{name}?version=`: remove the playlist if it is still at `version`.
/// 400 without a version.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<DeleteParams>,
) -> Response {
    write(state, name, deleted, move |name| {
        LibraryRequest::PlaylistDelete {
            name,
            version: params.version,
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use crate::test_support::{
        body_json, library_ok, send_json, test_app, test_app_with, FakeGateway,
    };
    use axum::http::StatusCode;
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{
        CanonicalHash, ContentHash, EditOutcome, EntryError, PlaylistEntry, PlaylistSnapshot,
        PlaylistVersion, ProtocolError, Skipped, StoredPlaylist,
    };
    use serde_json::{json, Value};

    /// A full canonical hash made of `c`.
    fn full(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn version(text: &str) -> PlaylistVersion {
        PlaylistVersion::of(text.as_bytes())
    }

    fn snapshot(text: &str) -> PlaylistSnapshot {
        PlaylistSnapshot {
            version: version(text),
            entries: vec![
                PlaylistEntry::Track {
                    hash: CanonicalHash::try_from(ContentHash::new(full('a'))).unwrap(),
                },
                PlaylistEntry::Unresolved {
                    error: EntryError::Unknown {
                        hash: ContentHash::new("0badc0de"),
                    },
                },
            ],
        }
    }

    /// `snapshot(text)` as the console serves it.
    fn snapshot_json(text: &str) -> Value {
        json!({
            "version": version(text).as_str(),
            "entries": [
                {"status": "Track", "hash": full('a')},
                {"status": "Unresolved", "token": "0badc0de", "reason": "unknown track 0badc0de"},
            ],
        })
    }

    /// One call of each playlist write: its method, its path and its body.
    fn calls() -> Vec<(&'static str, String, Option<Value>)> {
        let v = version("old");
        vec![
            (
                "POST",
                "/api/playlists/set/edit".into(),
                Some(json!({"version": v, "ops": [
                    {"op": "Insert", "at": 1, "hashes": [full('b')], "duplicates": "Skip"},
                    {"op": "Move", "from": 0, "to": 1},
                    {"op": "Remove", "at": 0},
                ]})),
            ),
            (
                "POST",
                "/api/playlists/set/revert".into(),
                Some(json!({"version": v, "content": "aaa\n"})),
            ),
            (
                "DELETE",
                format!("/api/playlists/set?version={}", v.as_str()),
                None,
            ),
        ]
    }

    /// A library that answers every playlist write with `answer`.
    fn write_gateway(answer: LibraryResponse) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request:
                    LibraryRequest::PlaylistEdit { .. }
                    | LibraryRequest::PlaylistRevert { .. }
                    | LibraryRequest::PlaylistDelete { .. },
            } => library_ok(answer.clone()),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    async fn send_call(
        app: axum::Router,
        call: &(&str, String, Option<Value>),
    ) -> axum::response::Response {
        send_json(app, call.0, &call.1, call.2.as_ref()).await
    }

    fn applied() -> LibraryResponse {
        LibraryResponse::PlaylistEdited(EditOutcome::Applied {
            snapshot: snapshot("new"),
            skipped: vec![Skipped {
                hash: ContentHash::new(full('b')),
                existing_at: 3,
            }],
            previous: StoredPlaylist::new("old\n"),
        })
    }

    fn conflict() -> LibraryResponse {
        LibraryResponse::PlaylistEdited(EditOutcome::Conflict {
            current: snapshot("theirs"),
        })
    }

    #[tokio::test]
    async fn an_applied_edit_is_200_with_snapshot_skipped_and_previous() {
        let gw = write_gateway(applied());
        let all = calls();
        for call in &all[..2] {
            let res = send_call(test_app_with(gw.addr()), call).await;
            assert_eq!(res.status(), StatusCode::OK, "{}", call.1);
            assert_eq!(
                body_json(res).await,
                json!({
                    "snapshot": snapshot_json("new"),
                    "skipped": [{"hash": full('b'), "existing_at": 3}],
                    "previous": "old\n",
                }),
                "{}",
                call.1
            );
        }
    }

    #[tokio::test]
    async fn an_edit_forwards_the_version_and_the_ops_unchanged() {
        let gw = write_gateway(applied());
        let all = calls();
        let res = send_call(test_app_with(gw.addr()), &all[0]).await;
        assert_eq!(res.status(), StatusCode::OK);
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request:
                    LibraryRequest::PlaylistEdit {
                        name,
                        version: v,
                        ops,
                    },
            }] => {
                assert_eq!(name.as_str(), "set");
                assert_eq!(*v, version("old"));
                assert_eq!(
                    serde_json::to_value(ops).unwrap(),
                    all[0].2.as_ref().unwrap()["ops"]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_revert_forwards_the_version_and_the_content_verbatim() {
        let gw = write_gateway(applied());
        let all = calls();
        send_call(test_app_with(gw.addr()), &all[1]).await;
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request:
                    LibraryRequest::PlaylistRevert {
                        name,
                        version: v,
                        content,
                    },
            }] => {
                assert_eq!(name.as_str(), "set");
                assert_eq!(*v, version("old"));
                assert_eq!(content.as_str(), "aaa\n");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_stale_version_is_409_with_the_current_snapshot() {
        let gw = write_gateway(conflict());
        let all = calls();
        for call in &all[..2] {
            let res = send_call(test_app_with(gw.addr()), call).await;
            assert_eq!(res.status(), StatusCode::CONFLICT, "{}", call.1);
            assert_eq!(
                body_json(res).await,
                json!({"current": snapshot_json("theirs")}),
                "{}",
                call.1
            );
        }
    }

    #[tokio::test]
    async fn an_answer_that_is_not_the_outcome_asked_for_is_502() {
        let gw = write_gateway(LibraryResponse::Pong);
        for call in &calls() {
            let res = send_call(test_app_with(gw.addr()), call).await;
            assert_eq!(res.status(), StatusCode::BAD_GATEWAY, "{}", call.1);
        }
    }

    #[tokio::test]
    async fn a_delete_is_200_when_deleted_and_409_with_current_when_stale() {
        let all = calls();
        let gw = write_gateway(LibraryResponse::PlaylistDeleted);
        let res = send_call(test_app_with(gw.addr()), &all[2]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({"deleted": true}));
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request: LibraryRequest::PlaylistDelete { name, version: v },
            }] => {
                assert_eq!(name.as_str(), "set");
                assert_eq!(*v, version("old"));
            }
            other => panic!("{other:?}"),
        }

        let gw = write_gateway(LibraryResponse::PlaylistDeleteConflict {
            current: snapshot("theirs"),
        });
        let res = send_call(test_app_with(gw.addr()), &all[2]).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(res).await,
            json!({"current": snapshot_json("theirs")})
        );
    }

    #[tokio::test]
    async fn a_missing_playlist_is_404() {
        let gw = write_gateway(LibraryResponse::Error(ProtocolError::PlaylistNotFound {
            name: "set".into(),
        }));
        for call in &calls() {
            let res = send_call(test_app_with(gw.addr()), call).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{}", call.1);
            assert!(body_json(res).await["error"].is_string());
        }
    }

    #[tokio::test]
    async fn a_refused_hash_an_invalid_edit_and_an_invalid_name_are_422() {
        for refusal in [
            ProtocolError::TrackNotFound {
                hash: "dead".into(),
            },
            ProtocolError::AmbiguousHash {
                hash: "ab".into(),
                candidates: vec![],
            },
            ProtocolError::InvalidEdit {
                op: 2,
                reason: "position 9 is out of range".into(),
            },
            ProtocolError::InvalidPlaylistName { name: "x".into() },
        ] {
            let gw = write_gateway(LibraryResponse::Error(refusal.clone()));
            for call in &calls() {
                let res = send_call(test_app_with(gw.addr()), call).await;
                assert_eq!(
                    res.status(),
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "{} {refusal:?}",
                    call.1
                );
                assert!(body_json(res).await["error"].is_string());
            }
        }
    }

    #[tokio::test]
    async fn an_outcome_of_another_write_is_502() {
        let all = calls();
        let deleted = write_gateway(LibraryResponse::PlaylistDeleted);
        let res = send_call(test_app_with(deleted.addr()), &all[0]).await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_GATEWAY,
            "edit answered deleted"
        );
        let res = send_call(test_app_with(deleted.addr()), &all[1]).await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_GATEWAY,
            "revert answered deleted"
        );
        let applied = write_gateway(applied());
        let res = send_call(test_app_with(applied.addr()), &all[2]).await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_GATEWAY,
            "delete answered applied"
        );
        let conflicted = write_gateway(conflict());
        let res = send_call(test_app_with(conflicted.addr()), &all[2]).await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_GATEWAY,
            "delete answered edit conflict"
        );
    }

    #[tokio::test]
    async fn a_library_that_refuses_is_502_and_one_that_cannot_be_reached_is_503() {
        let refusing = FakeGateway::start(|_| GatewayResponse::Error {
            message: "library unavailable".into(),
        });
        let unreadable = FakeGateway::start_unreadable_when(
            |req| matches!(req, GatewayRequest::Library { .. }),
            |_| unreachable!(),
        );
        for call in &calls() {
            let res = send_call(test_app_with(refusing.addr()), call).await;
            assert_eq!(res.status(), StatusCode::BAD_GATEWAY, "{}", call.1);
            let res = send_call(test_app_with(unreadable.addr()), call).await;
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{}", call.1);
            let res = send_call(test_app(), call).await;
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{}", call.1);
        }
    }

    #[tokio::test]
    async fn a_name_no_playlist_can_have_is_400_and_the_library_is_not_asked() {
        let gw = write_gateway(applied());
        for (method, path, body) in calls() {
            let path = path.replace("/set", "/bad%20name");
            let res = send_json(test_app_with(gw.addr()), method, &path, body.as_ref()).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{path}");
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn a_delete_without_a_version_is_400_and_the_library_is_not_asked() {
        let gw = write_gateway(LibraryResponse::PlaylistDeleted);
        let res = send_json(
            test_app_with(gw.addr()),
            "DELETE",
            "/api/playlists/set",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());
    }
}
