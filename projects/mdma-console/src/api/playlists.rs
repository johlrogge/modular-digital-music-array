//! Playlist writes for the new web UI (ADR-027 §9): create, edit, revert (undo) and delete, each sent to
//! the library with the version the browser last read. The console decides nothing about a
//! playlist: it forwards the ops and the version, and answers with what the library answered.
//!
//! - 200: applied (`{snapshot, skipped, previous}`; `previous` only when the edit wrote, so Undo
//!   exists exactly then) or deleted into the bin (`{id}`, ADR-033);
//! - 201: created (`{name}`), by `POST /api/playlists {name}`;
//! - 409: the version was stale, nothing was written (`{current}`, the playlist as it is now);
//!   or, for a create, the playlist already exists (`{error}`);
//! - every other failure is classified by `library_failure_status`, the one policy of ADR-026 A7.
//!   There is no fallback to PlaylistReplace: an old node answers 502.

use super::library::{playlist_name, ApiSnapshot};
use super::{
    error_reply, fail, forward, gateway_failure_status, gw, lib, library_failed, respond,
    source_failure_status, Interpret, Reply,
};
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Json, Response},
};
use gateway_client::{LibraryRequest, LibraryResponse, SourceRequest, SourceResponse};
use library_ipc_client::{
    EditOp, EditOutcome, PlaylistName, PlaylistSnapshot, PlaylistVersion, StoredPlaylist,
};
use serde::Deserialize;
use source_protocol::{PlaylistLinkInfo, PlaylistSyncJobStatus, PlaylistSyncPhase};
use std::sync::Arc;

/// 409 `{current}`: the version was stale and nothing was written.
fn conflict(current: &PlaylistSnapshot) -> Reply {
    (
        StatusCode::CONFLICT,
        serde_json::json!({ "current": ApiSnapshot::from(current) }),
    )
}

/// The answer to an edit or a revert: applied (200) or conflict (409).
fn edited(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::PlaylistEdited(EditOutcome::Applied {
            snapshot,
            skipped,
            previous,
        }) => {
            let mut body = serde_json::json!({
                "snapshot": ApiSnapshot::from(&snapshot),
                "skipped": skipped,
            });
            // Present exactly when the edit wrote, like `previous` on the queue routes.
            if let Some(previous) = previous {
                body["previous"] = serde_json::json!(previous);
            }
            Some((StatusCode::OK, body))
        }
        LibraryResponse::PlaylistEdited(EditOutcome::Conflict { current }) => {
            Some(conflict(&current))
        }
        _ => None,
    }
}

/// The answer to a delete, which bins (ADR-033): binned (200 `{id}`, for Undo) or conflict (409).
fn binned(response: LibraryResponse) -> Option<Reply> {
    match response {
        LibraryResponse::PlaylistBinned { id } => {
            Some((StatusCode::OK, serde_json::json!({ "id": id.to_string() })))
        }
        LibraryResponse::PlaylistDeleteConflict { current } => Some(conflict(&current)),
        _ => None,
    }
}

/// The one path of every write: `name` from the path is checked ([`playlist_name`]), then
/// `request` is built for it and sent ([`forward`]).
async fn write(
    state: Arc<AppState>,
    name: String,
    interpret: Interpret,
    request: impl FnOnce(PlaylistName) -> LibraryRequest + Send,
) -> Response {
    match playlist_name(&name) {
        Ok(name) => forward(&state, request(name), interpret).await,
        Err(response) => response,
    }
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

/// `DELETE /api/playlists/{name}?version=`: move the playlist to the bin if it is still at
/// `version` (ADR-033). 200 `{id}`, the bin id to restore it by. 400 without a version.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<DeleteParams>,
) -> Response {
    write(state, name, binned, move |name| {
        LibraryRequest::PlaylistBin {
            name,
            version: params.version,
        }
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    name: String,
}

/// `POST /api/playlists {name}`: create an empty playlist (`PlaylistNew`, ADR-027 §6: the library
/// checks that it does not exist and writes it atomically). 201 `{name}`; 422 for a name no
/// playlist can have (the library is not asked); every library failure, 409 for a playlist that
/// exists included, by `library_failed`.
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRequest>,
) -> Response {
    let name = match PlaylistName::new(&req.name) {
        Ok(n) => n,
        Err(e) => return fail(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    };
    respond(
        lib(&state, move |l| {
            l.playlist_new(&name, String::new()).map(|()| name)
        })
        .await
        .map(|created| match created {
            Ok(name) => (
                StatusCode::CREATED,
                serde_json::json!({ "name": name.as_str() }),
            ),
            Err(e) => library_failed(&e),
        }),
    )
}

// ---- Bandcamp link and sync (ADR-032 §4.4, §8) ----
//
// The console forwards to the source (`state.source_name`) and answers with what it answered:
//
// - 200: linked (the link) or listed; the sync's status, whatever its phase (`{phase: "none"}`
//   before the first job); 202: a sync job started (`{job_id}`);
// - 401: the source is not authenticated; 409: a sync (or another job) is running, or the
//   playlist is not linked; 422: a source playlist the source cannot use; 429: rate limited;
//   every status by `source_failure_status`, the one policy of source refusals;
// - 400: a name no playlist can have (`playlist_name`; the source is not asked);
// - 502: the source failed or answered something else; 503: the gateway cannot be reached
//   (`gateway_failure_status`).
// The two reads are `/api/bandcamp/...`: nothing static sits under `/api/playlists/` (see
// `routes()`).

/// How the source's answer becomes a reply: `None` for an answer that is not the one asked for.
type SourceInterpret = fn(SourceResponse) -> Option<Reply>;

/// Send `request` to the source over one gateway connection and answer as `interpret` reads its
/// answer.
async fn source_call(
    state: &Arc<AppState>,
    request: SourceRequest,
    interpret: SourceInterpret,
) -> Response {
    let source = state.source_name.clone();
    respond(
        gw(state, move |client| {
            match client.source_request(&source, &request) {
                Ok(SourceResponse::Error(e)) => {
                    error_reply(source_failure_status(&e), e.to_string())
                }
                Ok(answer) => interpret(answer).unwrap_or_else(|| {
                    error_reply(StatusCode::BAD_GATEWAY, "Unexpected response from source")
                }),
                Err(e) => error_reply(gateway_failure_status(&e), e.to_string()),
            }
        })
        .await,
    )
}

fn link_json(link: &PlaylistLinkInfo) -> serde_json::Value {
    serde_json::json!({
        "local": link.local.as_str(),
        "source": link.source,
        "last_synced": link.last_synced,
        "pending": link.pending,
    })
}

fn linked(answer: SourceResponse) -> Option<Reply> {
    match answer {
        SourceResponse::PlaylistLinked(link) => Some((StatusCode::OK, link_json(&link))),
        _ => None,
    }
}

fn listed_links(answer: SourceResponse) -> Option<Reply> {
    match answer {
        SourceResponse::PlaylistLinks(links) => {
            let body: Vec<_> = links.iter().map(link_json).collect();
            Some((StatusCode::OK, serde_json::Value::Array(body)))
        }
        _ => None,
    }
}

fn sync_started(answer: SourceResponse) -> Option<Reply> {
    match answer {
        SourceResponse::PlaylistSyncStarted { job_id } => Some((
            StatusCode::ACCEPTED,
            serde_json::json!({ "job_id": job_id }),
        )),
        _ => None,
    }
}

/// A job as the browser polls it: `{job_id, local, phase, downloads}`, plus `report` when done
/// (counts and `undo`, which is `null` when the sync wrote nothing) or `message` when failed.
fn job_json(job: &PlaylistSyncJobStatus) -> serde_json::Value {
    let phase = match &job.phase {
        PlaylistSyncPhase::Fetching => "fetching",
        PlaylistSyncPhase::Matching => "matching",
        PlaylistSyncPhase::Syncing => "syncing",
        PlaylistSyncPhase::Rematching => "rematching",
        PlaylistSyncPhase::Merging => "merging",
        PlaylistSyncPhase::Done(_) => "done",
        PlaylistSyncPhase::Failed { .. } => "failed",
    };
    let mut body = serde_json::json!({
        "job_id": job.job_id,
        "local": job.local.as_str(),
        "phase": phase,
        "downloads": job.sync,
    });
    match &job.phase {
        PlaylistSyncPhase::Done(report) => {
            body["report"] = serde_json::json!({
                "added": report.added,
                "removed": report.removed,
                "moved": report.moved,
                "pending": report.pending,
                "undo": report.undo,
            });
        }
        PlaylistSyncPhase::Failed { message } => body["message"] = serde_json::json!(message),
        _ => {}
    }
    body
}

fn sync_status(answer: SourceResponse) -> Option<Reply> {
    match answer {
        SourceResponse::PlaylistSyncStatus(job) => Some((
            StatusCode::OK,
            job.as_ref()
                .map_or_else(|| serde_json::json!({ "phase": "none" }), job_json),
        )),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
pub struct LinkRequest {
    /// The Bandcamp playlist: a slug or a URL.
    source: String,
}

/// `POST /api/playlists/{name}/link {source}`: link the playlist to a Bandcamp playlist (ADR-032
/// §8). 200 with the link.
pub async fn link(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<LinkRequest>,
) -> Response {
    match playlist_name(&name) {
        Ok(local) => {
            let request = SourceRequest::LinkPlaylist {
                local,
                source: req.source,
            };
            source_call(&state, request, linked).await
        }
        Err(response) => response,
    }
}

/// `GET /api/bandcamp/playlist-links`: every link with its pending items.
pub async fn links(State(state): State<Arc<AppState>>) -> Response {
    source_call(&state, SourceRequest::ListPlaylistLinks, listed_links).await
}

/// `POST /api/playlists/{name}/sync`: start the sync job. 202 `{job_id}`; 409 for a playlist that
/// is not linked or a sync that is running.
pub async fn sync(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    match playlist_name(&name) {
        Ok(local) => {
            let request = SourceRequest::SyncPlaylist { local };
            source_call(&state, request, sync_started).await
        }
        Err(response) => response,
    }
}

/// `GET /api/bandcamp/playlist-sync`: the current or last sync job (the source keeps one), for the
/// browser to follow by the `job_id` it was given.
pub async fn sync_job(State(state): State<Arc<AppState>>) -> Response {
    source_call(&state, SourceRequest::GetPlaylistSyncStatus, sync_status).await
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
                    | LibraryRequest::PlaylistBin { .. },
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
            previous: Some(StoredPlaylist::new("old\n")),
        })
    }

    fn applied_unchanged() -> LibraryResponse {
        LibraryResponse::PlaylistEdited(EditOutcome::Applied {
            snapshot: snapshot("new"),
            skipped: Vec::new(),
            previous: None,
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
    async fn an_edit_that_wrote_nothing_has_no_previous() {
        let gw = write_gateway(applied_unchanged());
        let all = calls();
        for call in &all[..2] {
            let res = send_call(test_app_with(gw.addr()), call).await;
            assert_eq!(res.status(), StatusCode::OK, "{}", call.1);
            assert_eq!(
                body_json(res).await,
                json!({"snapshot": snapshot_json("new"), "skipped": []}),
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
    async fn a_delete_bins_with_200_and_the_id_and_is_409_with_current_when_stale() {
        let all = calls();
        let gw = write_gateway(LibraryResponse::PlaylistBinned {
            id: "set.42".parse().unwrap(),
        });
        let res = send_call(test_app_with(gw.addr()), &all[2]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({"id": "set.42"}));
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request: LibraryRequest::PlaylistBin { name, version: v },
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

    // ---- POST /api/playlists (create) ----

    fn create_gateway(answer: LibraryResponse) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistNew { .. },
            } => library_ok(answer.clone()),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    async fn create(app: axum::Router, name: &str) -> axum::response::Response {
        send_json(
            app,
            "POST",
            "/api/playlists",
            Some(&json!({ "name": name })),
        )
        .await
    }

    #[tokio::test]
    async fn a_created_playlist_is_201_with_its_name() {
        let gw = create_gateway(LibraryResponse::PlaylistContent(String::new()));
        let res = create(test_app_with(gw.addr()), "friday").await;
        assert_eq!(res.status(), StatusCode::CREATED);
        assert_eq!(body_json(res).await, json!({"name": "friday"}));
    }

    #[tokio::test]
    async fn a_created_playlist_is_sent_to_the_library_empty() {
        let gw = create_gateway(LibraryResponse::PlaylistContent(String::new()));
        create(test_app_with(gw.addr()), "friday").await;
        match &gw.requests()[..] {
            [GatewayRequest::Library {
                request: LibraryRequest::PlaylistNew { name, content },
            }] => {
                assert_eq!(name.as_str(), "friday");
                assert_eq!(content, "");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn creating_a_playlist_that_exists_is_409() {
        let gw = create_gateway(LibraryResponse::Error(
            ProtocolError::PlaylistAlreadyExists {
                name: "friday".into(),
            },
        ));
        let res = create(test_app_with(gw.addr()), "friday").await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn creating_a_playlist_with_an_invalid_name_is_422_and_the_library_is_not_asked() {
        let gw = create_gateway(LibraryResponse::PlaylistContent(String::new()));
        let res = create(test_app_with(gw.addr()), "bad name").await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"].is_string());
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn creating_a_playlist_when_the_library_refuses_is_502() {
        let gw = FakeGateway::start(|_| GatewayResponse::Error {
            message: "library unavailable".into(),
        });
        let res = create(test_app_with(gw.addr()), "friday").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn creating_a_playlist_without_a_gateway_is_503() {
        let res = create(test_app(), "friday").await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn creating_a_playlist_when_the_library_answers_something_else_is_502() {
        let gw = create_gateway(LibraryResponse::Pong);
        let res = create(test_app_with(gw.addr()), "friday").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    // ---- Bandcamp link and sync (ADR-032) ----

    use gateway_client::{SourceName, SourceRequest, SourceResponse};
    use source_protocol::{
        ImportSyncProgress, PendingPlaylistTrack, PlaylistLinkInfo, PlaylistSyncJobStatus,
        PlaylistSyncPhase, PlaylistSyncReport, PlaylistUndo, SourceError,
    };

    fn local(name: &str) -> library_ipc_client::PlaylistName {
        library_ipc_client::PlaylistName::new(name).unwrap()
    }

    /// A source that answers every request with `answer`.
    fn source_gateway(answer: SourceResponse) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Source { .. } => GatewayResponse::Source {
                name: SourceName::new("bandcamp"),
                response: answer.clone(),
            },
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    fn link_info() -> PlaylistLinkInfo {
        PlaylistLinkInfo {
            local: local("friday"),
            source: "https://bandcamp.com/me/playlist/9".into(),
            pending: vec![PendingPlaylistTrack {
                source_track_id: "t1".into(),
                title: "Song".into(),
                artist: "Band".into(),
            }],
            last_synced: Some(1234),
        }
    }

    fn link_json() -> Value {
        json!({
            "local": "friday",
            "source": "https://bandcamp.com/me/playlist/9",
            "last_synced": 1234,
            "pending": [{"source_track_id": "t1", "title": "Song", "artist": "Band"}],
        })
    }

    fn job(phase: PlaylistSyncPhase) -> SourceResponse {
        SourceResponse::PlaylistSyncStatus(Some(PlaylistSyncJobStatus {
            job_id: 7,
            local: local("friday"),
            phase,
            sync: Some(ImportSyncProgress {
                queued: 3,
                completed: 1,
                failed: 0,
                ended: None,
            }),
        }))
    }

    fn report(undo: Option<PlaylistUndo>) -> PlaylistSyncReport {
        PlaylistSyncReport {
            local: local("friday"),
            added: 3,
            removed: 1,
            moved: 2,
            pending: 2,
            undo,
        }
    }

    async fn post(app: axum::Router, path: &str, body: Option<Value>) -> axum::response::Response {
        send_json(app, "POST", path, body.as_ref()).await
    }

    async fn get(app: axum::Router, path: &str) -> axum::response::Response {
        send_json(app, "GET", path, None).await
    }

    #[tokio::test]
    async fn linking_is_200_with_the_link_and_sends_the_name_and_source() {
        let gw = source_gateway(SourceResponse::PlaylistLinked(link_info()));
        let res = post(
            test_app_with(gw.addr()),
            "/api/playlists/friday/link",
            Some(json!({"source": "https://bandcamp.com/me/playlist/9"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, link_json());
        match &gw.requests()[..] {
            [GatewayRequest::Source {
                name,
                request: SourceRequest::LinkPlaylist { local, source },
            }] => {
                assert_eq!(name.as_str(), "bandcamp");
                assert_eq!(local.as_str(), "friday");
                assert_eq!(source, "https://bandcamp.com/me/playlist/9");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A playlist called "links" or "sync" is a playlist like any other: no static route may
    /// shadow it (see `routes()`).
    #[tokio::test]
    async fn playlists_called_links_and_sync_are_read_as_playlists() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistSnapshot { .. },
            } => library_ok(LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                name: "x".into(),
            })),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        for name in ["links", "sync"] {
            let res = get(test_app_with(gw.addr()), &format!("/api/playlists/{name}")).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{name}");
        }
        assert!(gw
            .requests()
            .iter()
            .all(|r| matches!(r, GatewayRequest::Library { .. })));
        assert_eq!(gw.requests().len(), 2);
    }

    /// Every source refusal and gateway failure has its status, by the shared policies.
    #[test]
    fn every_source_refusal_has_a_status() {
        use super::super::source_failure_status as status;
        let name = || local("friday");
        let s = |m: &str| m.to_string();
        let cases = [
            (SourceError::NotAuthenticated { message: s("m") }, 401),
            (SourceError::SyncFailed { message: s("m") }, 502),
            (
                SourceError::DownloadFailed {
                    id: source_protocol::DownloadId::new("p1"),
                    message: s("m"),
                },
                502,
            ),
            (
                SourceError::RateLimited {
                    retry_after_secs: 3,
                },
                429,
            ),
            (SourceError::Internal { message: s("m") }, 502),
            (SourceError::ItemNotFound { identifier: s("i") }, 404),
            (
                SourceError::CheckFailed {
                    identifier: s("i"),
                    reason: s("r"),
                },
                502,
            ),
            (SourceError::unsupported("x"), 502),
            (SourceError::InvalidRequest { message: s("m") }, 422),
            (SourceError::ImportInProgress { job_id: 1 }, 409),
            (SourceError::CheckInProgress { job_id: 1 }, 409),
            (SourceError::SyncInProgress { job_id: 1 }, 409),
            (SourceError::PlaylistNotLinked { name: name() }, 409),
        ];
        for (error, expected) in cases {
            assert_eq!(status(&error).as_u16(), expected, "{error:?}");
        }
    }

    #[test]
    fn every_gateway_failure_has_a_status() {
        use super::super::gateway_failure_status as status;
        use gateway_client::ClientError as E;
        assert_eq!(status(&E::Service("x".into())), StatusCode::BAD_GATEWAY);
        assert_eq!(status(&E::Refused("x".into())), StatusCode::BAD_GATEWAY);
        assert_eq!(
            status(&E::Nng(nng::Error::TimedOut)),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let bad = serde_json::from_str::<u8>("x").unwrap_err();
        assert_eq!(
            status(&E::Serialization(bad)),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn linking_a_bad_name_is_400_and_the_source_is_not_asked() {
        let gw = source_gateway(SourceResponse::PlaylistLinked(link_info()));
        let res = post(
            test_app_with(gw.addr()),
            "/api/playlists/bad%20name/link",
            Some(json!({"source": "x"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn linking_a_source_the_source_refuses_is_422() {
        let gw = source_gateway(SourceResponse::Error(SourceError::InvalidRequest {
            message: "not a playlist".into(),
        }));
        let res = post(
            test_app_with(gw.addr()),
            "/api/playlists/friday/link",
            Some(json!({"source": "nope"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn linking_when_not_authenticated_is_401() {
        let gw = source_gateway(SourceResponse::Error(SourceError::NotAuthenticated {
            message: "no cookies".into(),
        }));
        let res = post(
            test_app_with(gw.addr()),
            "/api/playlists/friday/link",
            Some(json!({"source": "x"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn the_links_are_listed_with_their_pending_items() {
        let gw = source_gateway(SourceResponse::PlaylistLinks(vec![link_info()]));
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-links").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!([link_json()]));
        assert!(matches!(
            &gw.requests()[..],
            [GatewayRequest::Source {
                request: SourceRequest::ListPlaylistLinks,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn starting_a_sync_answers_the_job_id() {
        let gw = source_gateway(SourceResponse::PlaylistSyncStarted { job_id: 7 });
        let res = post(test_app_with(gw.addr()), "/api/playlists/friday/sync", None).await;
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        assert_eq!(body_json(res).await, json!({"job_id": 7}));
        match &gw.requests()[..] {
            [GatewayRequest::Source {
                request: SourceRequest::SyncPlaylist { local },
                ..
            }] => assert_eq!(local.as_str(), "friday"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn syncing_a_playlist_that_is_not_linked_is_409() {
        let gw = source_gateway(SourceResponse::Error(SourceError::PlaylistNotLinked {
            name: local("friday"),
        }));
        let res = post(test_app_with(gw.addr()), "/api/playlists/friday/sync", None).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn syncing_while_a_sync_runs_is_409() {
        let gw = source_gateway(SourceResponse::Error(SourceError::SyncInProgress {
            job_id: 6,
        }));
        let res = post(test_app_with(gw.addr()), "/api/playlists/friday/sync", None).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn syncing_a_bad_name_is_400_and_the_source_is_not_asked() {
        let gw = source_gateway(SourceResponse::PlaylistSyncStarted { job_id: 7 });
        let res = post(
            test_app_with(gw.addr()),
            "/api/playlists/bad%20name/sync",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn a_running_sync_reports_its_phase_and_download_progress() {
        let gw = source_gateway(job(PlaylistSyncPhase::Syncing));
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-sync").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "job_id": 7,
                "local": "friday",
                "phase": "syncing",
                "downloads": {"queued": 3, "completed": 1, "failed": 0, "ended": null},
            })
        );
    }

    #[tokio::test]
    async fn a_done_sync_reports_the_counts_and_the_undo() {
        let undo = PlaylistUndo {
            version: version("after"),
            previous: StoredPlaylist::new("before\n"),
        };
        let gw = source_gateway(job(PlaylistSyncPhase::Done(report(Some(undo)))));
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-sync").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["phase"], "done");
        assert_eq!(
            body["report"],
            json!({
                "added": 3, "removed": 1, "moved": 2, "pending": 2,
                "undo": {"version": version("after").as_str(), "previous": "before\n"},
            })
        );
    }

    #[tokio::test]
    async fn a_done_sync_that_wrote_nothing_has_no_undo() {
        let gw = source_gateway(job(PlaylistSyncPhase::Done(report(None))));
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-sync").await;
        assert_eq!(body_json(res).await["report"]["undo"], Value::Null);
    }

    #[tokio::test]
    async fn a_failed_sync_reports_its_message() {
        let gw = source_gateway(job(PlaylistSyncPhase::Failed {
            message: "friday not found".into(),
        }));
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-sync").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["phase"], "failed");
        assert_eq!(body["message"], "friday not found");
    }

    #[tokio::test]
    async fn no_sync_job_yet_is_phase_none() {
        let gw = source_gateway(SourceResponse::PlaylistSyncStatus(None));
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-sync").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({"phase": "none"}));
    }

    #[tokio::test]
    async fn another_answer_from_the_source_is_502() {
        let gw = source_gateway(SourceResponse::Pong);
        let res = get(test_app_with(gw.addr()), "/api/bandcamp/playlist-links").await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let res = post(test_app_with(gw.addr()), "/api/playlists/friday/sync", None).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn a_gateway_that_refuses_is_502_and_one_that_cannot_be_reached_is_503() {
        let refusing = FakeGateway::start(|_| GatewayResponse::Error {
            message: "source unavailable".into(),
        });
        let res = get(
            test_app_with(refusing.addr()),
            "/api/bandcamp/playlist-links",
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let res = get(test_app(), "/api/bandcamp/playlist-links").await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let res = post(test_app(), "/api/playlists/friday/sync", None).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let res = post(
            test_app(),
            "/api/playlists/friday/link",
            Some(json!({"source": "x"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let res = get(test_app(), "/api/bandcamp/playlist-sync").await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_delete_without_a_version_is_400_and_the_library_is_not_asked() {
        let gw = write_gateway(LibraryResponse::Pong);
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
