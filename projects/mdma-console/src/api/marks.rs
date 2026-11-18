//! Marks for the new web UI (ADR-016 amendment): set and clear marks on tracks, and the macro
//! marks the player offers. Which tracks carry which marks is `ApiTrack.marks`, the one source.

use super::library::{mark_param, ApiTrack};
use super::{
    error_reply, lib, library_failure_status, parse_hashes, resolve_failed, respond, Reply,
};
use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use library_ipc_client::{ContentHash, Mark, MarkWrite, Unmark};
use mdma_client::LibraryBackend;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

/// The macro marks the full player offers next to the heart. A node-side default for now (ADR-016
/// amendment): configuration, not a fact about music, and the same on every device.
pub const DEFAULT_MACROS: [&str; 2] = ["banger", "opener"];

/// What a marks request does to the tracks it names. A set operation, never a toggle: it is safe
/// to repeat, and it never acts on the browser's possibly stale view of the marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkAction {
    Mark,
    Unmark,
}

#[derive(Debug, Deserialize)]
pub struct MarksRequest {
    action: MarkAction,
    hashes: Vec<String>,
    /// The mark's name; absent, blank or "liked" is the default mark.
    mark: Option<String>,
}

/// `GET /api/marks/config`: `{macros: [name, ...]}`.
pub async fn config() -> Response {
    Json(serde_json::json!({ "macros": DEFAULT_MACROS })).into_response()
}

/// `POST /api/marks {action: "mark"|"unmark", hashes, mark?}`. Every hash is resolved against the
/// library before anything is written.
///
/// Replies: 200 `{changed, tracks}`, where `changed` holds the canonical hashes whose marks this
/// request changed (a track that already had the mark, or never had it, is not in it; the
/// catalogue's hashes are canonical, so the browser compares them as they are) and `tracks` each
/// named track as `GET /api/tracks` serves it, marks as they are now; 207 `{changed, tracks, error}` when it stopped
/// partway; the failure's status (502/503) with `{error, changed: [], tracks: []}` when the
/// first write failed; 400 without hashes; 422 for a bad mark name or a hash the library cannot
/// resolve (nothing written).
pub async fn marks(State(state): State<Arc<AppState>>, Json(req): Json<MarksRequest>) -> Response {
    let hashes = match parse_hashes(req.hashes) {
        Ok(h) => h,
        Err(response) => return response,
    };
    let mark = match mark_param(req.mark.as_deref()) {
        Ok(m) => m.unwrap_or(Mark::Default),
        Err(message) => return respond(Ok(error_reply(StatusCode::UNPROCESSABLE_ENTITY, message))),
    };
    let action = req.action;
    respond(
        lib(&state, move |backend| {
            apply_each(backend, &hashes, |hash| {
                change(backend, action, hash, &mark)
            })
        })
        .await,
    )
}

/// Whether the library changed the track's marks.
fn change(
    backend: &LibraryBackend,
    action: MarkAction,
    hash: &ContentHash,
    mark: &Mark,
) -> Result<bool, library_ipc_client::ClientError> {
    Ok(match action {
        MarkAction::Mark => backend.write_mark(hash, mark)? == MarkWrite::Written,
        MarkAction::Unmark => matches!(backend.unmark(hash, mark)?, Unmark::Retracted { .. }),
    })
}

/// Resolve every hash before anything is written, then run `change` on each track once, in the
/// order first named, and answer `{changed, tracks}` as the module doc says. `change` says whether
/// it changed the track. Shared by the marks and energy routes.
pub(super) fn apply_each(
    backend: &LibraryBackend,
    hashes: &[ContentHash],
    change: impl Fn(&ContentHash) -> Result<bool, library_ipc_client::ClientError>,
) -> Reply {
    let mut resolved = match backend.resolve_for_playback(hashes) {
        Ok(resolved) => resolved,
        Err(e) => return resolve_failed(e),
    };
    // A hash named twice (or as a short and a full one) is written once, in the order first named.
    let mut seen = HashSet::new();
    resolved.retain(|h| seen.insert(h.as_str().to_owned()));
    let mut changed = Vec::new();
    let mut tracks: Vec<ApiTrack> = Vec::new();
    let mut failure = None;
    for canonical in resolved {
        let hash = ContentHash::from(canonical);
        let result = change(&hash).and_then(|did_change| {
            if did_change {
                changed.push(hash.as_str().to_owned());
            }
            backend.get_track(&hash)
        });
        match result {
            Ok(track) => tracks.push(ApiTrack::from(&track)),
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    match failure {
        None => (
            StatusCode::OK,
            serde_json::json!({ "changed": changed, "tracks": tracks }),
        ),
        Some(e) if tracks.is_empty() && changed.is_empty() => (
            library_failure_status(&e),
            serde_json::json!({ "error": e.to_string(), "changed": [], "tracks": [] }),
        ),
        Some(e) => (
            StatusCode::MULTI_STATUS,
            serde_json::json!({
                "changed": changed,
                "tracks": tracks,
                "error": e.to_string(),
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        bare_track, body_json, library_ok, send_get, send_post_json, test_app_with, FakeGateway,
    };
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::ProtocolError;
    use serde_json::json;

    fn full(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn banger() -> Mark {
        "banger".parse().unwrap()
    }

    fn internal() -> GatewayResponse {
        library_ok(LibraryResponse::Error(ProtocolError::Internal {
            message: "disk full".into(),
        }))
    }

    /// A library that knows the tracks `a`, `b` and `c`, whose tracks carry `now` after a write,
    /// and which answers every write with `written` and every unmark with `unmarked`.
    /// `fail_on` makes the write for that hash fail.
    fn gateway(
        now: Vec<Mark>,
        written: MarkWrite,
        unmarked: Unmark,
        fail_on: Option<char>,
    ) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => {
                let known = ['a', 'b', 'c'].iter().any(|c| hash.as_str() == full(*c));
                if known {
                    let mut t = bare_track(hash.as_str());
                    t.marks = now.clone();
                    library_ok(LibraryResponse::Track(Box::new(t)))
                } else {
                    library_ok(LibraryResponse::Error(ProtocolError::TrackNotFound {
                        hash: hash.as_str().to_owned(),
                    }))
                }
            }
            GatewayRequest::Library {
                request: LibraryRequest::WriteMark { hash, .. },
            }
            | GatewayRequest::Library {
                request: LibraryRequest::UnmarkTrack { hash, .. },
            } if fail_on.is_some_and(|c| hash.as_str() == full(c)) => internal(),
            GatewayRequest::Library {
                request: LibraryRequest::WriteMark { .. },
            } => library_ok(LibraryResponse::MarkWritten(written)),
            GatewayRequest::Library {
                request: LibraryRequest::UnmarkTrack { .. },
            } => library_ok(LibraryResponse::TrackUnmarked(unmarked)),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    fn retracted() -> Unmark {
        Unmark::Retracted {
            count: std::num::NonZeroUsize::new(1).unwrap(),
        }
    }

    fn writes(gw: &FakeGateway) -> usize {
        gw.requests()
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    GatewayRequest::Library {
                        request: LibraryRequest::WriteMark { .. }
                            | LibraryRequest::UnmarkTrack { .. }
                    }
                )
            })
            .count()
    }

    async fn post(gw: &FakeGateway, body: serde_json::Value) -> Response {
        send_post_json(test_app_with(gw.addr()), "/api/marks", &body).await
    }

    #[tokio::test]
    async fn marking_answers_what_changed_and_the_marks_now() {
        let gw = gateway(
            vec![Mark::Default, banger()],
            MarkWrite::Written,
            Unmark::NotLive,
            None,
        );
        let res = post(
            &gw,
            json!({"action": "mark", "hashes": [full('a')], "mark": "banger"}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        // The track as `GET /api/tracks` serves it: the browser replaces its entry.
        let track = ApiTrack::from(&{
            let mut t = bare_track(&full('a'));
            t.marks = vec![Mark::Default, banger()];
            t
        });
        assert_eq!(body["tracks"], json!([track]));
        assert_eq!(body["tracks"][0]["marks"], json!(["liked", "banger"]));
        let sent = gw.requests().into_iter().find_map(|r| match r {
            GatewayRequest::Library {
                request: LibraryRequest::WriteMark { mark, .. },
            } => Some(mark),
            _ => None,
        });
        assert_eq!(sent, Some(banger()));
    }

    #[tokio::test]
    async fn marking_without_a_name_or_with_a_blank_one_is_the_default_mark() {
        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, None);
        post(&gw, json!({"action": "mark", "hashes": [full('a')]})).await;
        for name in ["liked", "", "  "] {
            post(
                &gw,
                json!({"action": "mark", "hashes": [full('a')], "mark": name}),
            )
            .await;
        }
        let marks: Vec<Mark> = gw
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                GatewayRequest::Library {
                    request: LibraryRequest::WriteMark { mark, .. },
                } => Some(mark),
                _ => None,
            })
            .collect();
        assert_eq!(marks, vec![Mark::Default; 4]);
    }

    #[tokio::test]
    async fn marking_what_is_already_marked_changes_nothing() {
        let gw = gateway(
            vec![Mark::Default],
            MarkWrite::AlreadyLive,
            Unmark::NotLive,
            None,
        );
        let res = post(&gw, json!({"action": "mark", "hashes": [full('a')]})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([]));
        assert_eq!(body["tracks"][0]["marks"], json!(["liked"]));
    }

    #[tokio::test]
    async fn unmarking_reports_a_retracted_mark_as_changed_and_a_not_live_one_as_not() {
        let gw = gateway(vec![], MarkWrite::Written, retracted(), None);
        let res = post(&gw, json!({"action": "unmark", "hashes": [full('a')]})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        assert_eq!(body["tracks"][0]["hash"], full('a'));
        assert_eq!(body["tracks"][0]["marks"], json!([]));

        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, None);
        let res = post(&gw, json!({"action": "unmark", "hashes": [full('a')]})).await;
        assert_eq!(body_json(res).await["changed"], json!([]));
    }

    #[tokio::test]
    async fn a_set_of_hashes_is_written_in_order_and_a_repeat_is_written_once() {
        let gw = gateway(
            vec![Mark::Default],
            MarkWrite::Written,
            Unmark::NotLive,
            None,
        );
        let res = post(
            &gw,
            json!({"action": "mark", "hashes": [full('b'), full('a'), full('b')]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('b'), full('a')]));
        assert_eq!(writes(&gw), 2);
    }

    #[tokio::test]
    async fn no_hashes_is_400_and_sends_nothing() {
        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, None);
        let res = post(&gw, json!({"action": "mark", "hashes": []})).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"].is_string());
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn a_bad_mark_name_is_422_and_sends_nothing() {
        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, None);
        for name in ["a\u{200B}b", &"x".repeat(65)] {
            let res = post(
                &gw,
                json!({"action": "mark", "hashes": [full('a')], "mark": name}),
            )
            .await;
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
            assert!(body_json(res).await["error"].is_string());
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_action_is_422_and_sends_nothing() {
        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, None);
        let res = post(&gw, json!({"action": "toggle", "hashes": [full('a')]})).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_hash_is_422_and_nothing_is_written() {
        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, None);
        let res = post(
            &gw,
            json!({"action": "mark", "hashes": [full('a'), full('f')]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"].is_string());
        assert_eq!(writes(&gw), 0);
    }

    #[tokio::test]
    async fn stopping_partway_is_207_with_what_was_done() {
        let gw = gateway(
            vec![Mark::Default],
            MarkWrite::Written,
            Unmark::NotLive,
            Some('b'),
        );
        let res = post(
            &gw,
            json!({"action": "mark", "hashes": [full('a'), full('b'), full('c')]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::MULTI_STATUS);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        assert_eq!(body["tracks"].as_array().unwrap().len(), 1);
        assert_eq!(body["tracks"][0]["hash"], full('a'));
        assert_eq!(body["tracks"][0]["marks"], json!(["liked"]));
        assert!(body["error"].as_str().unwrap().contains("disk full"));
    }

    #[tokio::test]
    async fn a_first_write_that_fails_is_502_with_nothing_changed() {
        let gw = gateway(vec![], MarkWrite::Written, Unmark::NotLive, Some('a'));
        let res = post(&gw, json!({"action": "mark", "hashes": [full('a')]})).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([]));
        assert_eq!(body["tracks"], json!([]));
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn an_unreachable_library_is_503() {
        let gw = FakeGateway::start_unreadable_when(
            |_| true,
            |_| unreachable!("every request is unreadable"),
        );
        let res = post(&gw, json!({"action": "mark", "hashes": [full('a')]})).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn marking_without_a_gateway_is_503() {
        let res = send_post_json(
            crate::test_support::test_app(),
            "/api/marks",
            &json!({"action": "mark", "hashes": [full('a')]}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ---- GET /api/marks/config ----

    #[tokio::test]
    async fn config_serves_the_macros() {
        let res = send_get(crate::test_support::test_app(), "/api/marks/config", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({"macros": ["banger", "opener"]})
        );
    }

    #[test]
    fn every_default_macro_is_a_valid_named_mark() {
        for name in DEFAULT_MACROS {
            assert!(
                matches!(name.parse::<Mark>(), Ok(Mark::Named(n)) if n.as_str() == name),
                "{name}"
            );
        }
    }
}
