//! Node-level reads for the settings pages. `GET /api/inbox` lists the files waiting in the inbox
//! (the same queue the library reports), as paths relative to the inbox. `GET /api/packages`
//! lists the installed packages and the updates the last check found.

use super::{gw, json_or_failure, library, unexpected_library_response};
use crate::{AppState, PackageView};
use axum::{
    extract::State,
    response::{IntoResponse, Json, Response},
};
use gateway_client::{LibraryRequest, LibraryResponse};
use library_ipc_client::{ConversionState, InboxConversion, InboxPath};
use serde::Serialize;
use std::sync::Arc;

/// An inbox file and where the conversion worker has it (ADR-037).
#[derive(Debug, Serialize)]
pub struct InboxFileView {
    path: String,
    /// `idle` (the worker does not hold it), `queued`, `converting`, `retrying` or `failed`.
    state: &'static str,
    /// Of a `retrying` file: the failures so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
    /// Of a `retrying` or `failed` file: why.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// A recent conversion that ended in the library, and what it went on without.
#[derive(Debug, Serialize)]
pub struct InboxDoneView {
    path: String,
    /// The track it is in the library.
    hash: String,
    warnings: Vec<String>,
}

/// What `GET /api/inbox` answers.
#[derive(Debug, Serialize)]
pub struct InboxView {
    files: Vec<InboxFileView>,
    /// Newest first.
    done: Vec<InboxDoneView>,
    /// What an upload takes (`.flac`, `.aiff`, ..., `.zip`), for the file input's
    /// `accept`: derived from the ingest policy, so the page cannot drift from it.
    accept: Vec<String>,
}

/// The files waiting in the inbox with the state the worker reports for each, and the
/// outcomes it remembers.
fn inbox_view(paths: Vec<InboxPath>, conversions: Vec<InboxConversion>) -> InboxView {
    let files = paths
        .into_iter()
        .map(|path| {
            let held = conversions
                .iter()
                .find(|c| c.path == path && !matches!(c.state, ConversionState::Done { .. }));
            let (state, attempt, error) = match held.map(|c| &c.state) {
                Some(ConversionState::Queued) => ("queued", None, None),
                Some(ConversionState::Converting) => ("converting", None, None),
                Some(ConversionState::Retrying {
                    attempt,
                    last_error,
                }) => ("retrying", Some(*attempt), Some(last_error.clone())),
                Some(ConversionState::Failed { message }) => {
                    ("failed", None, Some(message.clone()))
                }
                Some(ConversionState::Unknown) => ("unknown", None, None),
                Some(ConversionState::Done { .. }) | None => ("idle", None, None),
            };
            InboxFileView {
                path: path.into(),
                state,
                attempt,
                error,
            }
        })
        .collect();
    let done = conversions
        .into_iter()
        .filter_map(|c| match c.state {
            ConversionState::Done { hash, warnings } => Some(InboxDoneView {
                path: c.path.into(),
                hash: hash.to_string(),
                warnings: warnings.iter().map(|w| w.to_string()).collect(),
            }),
            _ => None,
        })
        .collect();
    InboxView {
        files,
        done,
        accept: inbox_utils::upload_accept(),
    }
}

/// `GET /api/inbox`: the files waiting in the inbox, as the library lists them, each with its
/// conversion state, and the conversions that ended recently. A library that cannot report
/// conversions (an older one) reports none: the list is still the list.
pub async fn inbox(State(state): State<Arc<AppState>>) -> Response {
    json_or_failure(
        gw(&state, |client| {
            let paths = match library(client, &LibraryRequest::GetInboxQueue)? {
                LibraryResponse::InboxQueue(paths) => paths,
                _ => return Err(unexpected_library_response()),
            };
            let conversions = match library(client, &LibraryRequest::GetStatus) {
                Ok(LibraryResponse::Status(status)) => status.conversions,
                _ => Vec::new(),
            };
            Ok(inbox_view(paths, conversions))
        })
        .await,
    )
}

/// `GET /api/packages`: the installed MDMA packages, each with the version an update would bring
/// (`update_version`, null when it is current). The installed list is read afresh; when that fails
/// the last one read is used. Updates are those the last check found.
pub async fn packages(State(state): State<Arc<AppState>>) -> Response {
    match crate::packages::list_installed().await {
        Ok(pkgs) => *state.packages.lock().await = pkgs,
        Err(e) => tracing::warn!(error = %e, "Failed to list packages"),
    }
    Json(package_views(&state).await).into_response()
}

/// The cached installed packages, each with the update the last check found for it.
async fn package_views(state: &AppState) -> Vec<PackageView> {
    let installed = state.packages.lock().await;
    let updates = state.updates.lock().await;
    installed
        .iter()
        .map(|pkg| PackageView::from_installed(pkg, &updates))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::test_support::{
        body_json, library_ok, send_json, test_app, test_app_with, FakeGateway,
    };
    use axum::http::StatusCode;
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{
        ContentHash, ConversionState, InboxConversion, InboxPath, IngestWarning, ProtocolError,
        ServiceStatus,
    };
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

    /// A gateway that answers the queue and the status (with `conversions`).
    fn inbox_gateway_with_status(
        paths: Vec<InboxPath>,
        conversions: Vec<InboxConversion>,
    ) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::GetInboxQueue,
            } => library_ok(LibraryResponse::InboxQueue(paths.clone())),
            GatewayRequest::Library {
                request: LibraryRequest::GetStatus,
            } => library_ok(LibraryResponse::Status(ServiceStatus {
                version: "1".into(),
                tracks_indexed: 0,
                facts_count: 0,
                inbox_queue_size: paths.len(),
                uptime_seconds: 0,
                unparseable_lines: 0,
                conversions: conversions.clone(),
            })),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    fn held(name: &str, state: ConversionState) -> InboxConversion {
        InboxConversion {
            path: InboxPath::new(name).unwrap(),
            state,
        }
    }

    /// A node that cannot report conversions: every file is idle, nothing is done.
    #[tokio::test]
    async fn the_inbox_is_the_librarys_queue_with_every_file_idle() {
        let gw = inbox_gateway(LibraryResponse::InboxQueue(vec![
            InboxPath::new("a.flac").unwrap(),
            InboxPath::new("sub/b.mp3").unwrap(),
        ]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "files": [
                    {"path": "a.flac", "state": "idle"},
                    {"path": "sub/b.mp3", "state": "idle"},
                ],
                "done": [],
                "accept": inbox_utils::upload_accept(),
            })
        );
    }

    /// The upload's file input takes what ingest takes, AIFF in both spellings
    /// included, and ZIP archives.
    #[tokio::test]
    async fn the_inbox_says_which_extensions_an_upload_takes() {
        let gw = inbox_gateway(LibraryResponse::InboxQueue(vec![]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        let body = body_json(res).await;
        let accept: Vec<&str> = body["accept"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for ext in [".flac", ".mp3", ".wav", ".m4a", ".aiff", ".aif", ".zip"] {
            assert!(accept.contains(&ext), "{ext} missing from {accept:?}");
        }
    }

    #[tokio::test]
    async fn each_file_carries_the_state_the_worker_reports_and_the_recent_outcomes_follow() {
        let gw = inbox_gateway_with_status(
            ["a.aiff", "b.aiff", "c.aiff", "d.aiff", "e.flac"]
                .map(|n| InboxPath::new(n).unwrap())
                .to_vec(),
            vec![
                held("a.aiff", ConversionState::Converting),
                held("b.aiff", ConversionState::Queued),
                held(
                    "c.aiff",
                    ConversionState::Retrying {
                        attempt: 2,
                        last_error: "no flac".into(),
                    },
                ),
                held(
                    "d.aiff",
                    ConversionState::Failed {
                        message: "gave up".into(),
                    },
                ),
                held(
                    "f.aiff",
                    ConversionState::Done {
                        hash: ContentHash::new("sha256:abc"),
                        warnings: vec![IngestWarning::StoredAsAiff { reason: "r".into() }],
                    },
                ),
                held(
                    "g.aiff",
                    ConversionState::Done {
                        hash: ContentHash::new("sha256:def"),
                        warnings: Vec::new(),
                    },
                ),
            ],
        );
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "files": [
                    {"path": "a.aiff", "state": "converting"},
                    {"path": "b.aiff", "state": "queued"},
                    {"path": "c.aiff", "state": "retrying", "attempt": 2, "error": "no flac"},
                    {"path": "d.aiff", "state": "failed", "error": "gave up"},
                    {"path": "e.flac", "state": "idle"},
                ],
                "done": [
                    {"path": "f.aiff", "hash": "sha256:abc",
                     "warnings": ["not converted to FLAC, stored as AIFF (r)"]},
                    {"path": "g.aiff", "hash": "sha256:def", "warnings": []},
                ],
                "accept": inbox_utils::upload_accept(),
            })
        );
    }

    /// A finished conversion of a name is not the state of a file of that name that
    /// has arrived in the inbox since.
    #[tokio::test]
    async fn a_finished_outcome_does_not_mark_a_new_file_of_the_same_name() {
        let gw = inbox_gateway_with_status(
            vec![InboxPath::new("a.aiff").unwrap()],
            vec![held(
                "a.aiff",
                ConversionState::Done {
                    hash: ContentHash::new("sha256:abc"),
                    warnings: Vec::new(),
                },
            )],
        );
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        let body = body_json(res).await;
        assert_eq!(body["files"], json!([{"path": "a.aiff", "state": "idle"}]));
        assert_eq!(body["done"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_inbox_is_an_empty_list() {
        let gw = inbox_gateway(LibraryResponse::InboxQueue(vec![]));
        let res = send_json(test_app_with(gw.addr()), "GET", "/api/inbox", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["files"], json!([]));
        assert_eq!(body["done"], json!([]));
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

    async fn state_with(
        packages: Vec<(&str, &str)>,
        updates: Vec<(&str, &str, &str)>,
    ) -> std::sync::Arc<crate::AppState> {
        let state = crate::test_support::test_state("ipc:///nonexistent/gateway.sock");
        *state.packages.lock().await = packages
            .into_iter()
            .map(|(name, version)| crate::packages::InstalledPackage {
                name: name.into(),
                version: version.into(),
            })
            .collect();
        *state.updates.lock().await = updates
            .into_iter()
            .map(|(name, current, new)| crate::packages::AvailableUpdate {
                name: name.into(),
                current_version: current.into(),
                new_version: new.into(),
            })
            .collect();
        state
    }

    #[tokio::test]
    async fn packages_list_name_version_and_the_update_when_there_is_one() {
        let state = state_with(
            vec![("mdma-console", "0.41.0_1"), ("mdma-library", "0.41.0_1")],
            vec![("mdma-console", "0.41.0_1", "0.42.0_1")],
        )
        .await;
        assert_eq!(
            serde_json::to_value(super::package_views(&state).await).unwrap(),
            json!([
                {"name": "mdma-console", "version": "0.41.0_1", "update_version": "0.42.0_1"},
                {"name": "mdma-library", "version": "0.41.0_1", "update_version": null},
            ])
        );
    }

    #[tokio::test]
    async fn updating_one_package_keeps_the_other_pending_updates() {
        let state = state_with(
            vec![("mdma-console", "1"), ("mdma-library", "1")],
            vec![("mdma-console", "1", "2"), ("mdma-library", "1", "2")],
        )
        .await;
        crate::forget_update(
            &state,
            &crate::types::PackageName::new("mdma-library").unwrap(),
        )
        .await;
        assert_eq!(
            serde_json::to_value(super::package_views(&state).await).unwrap(),
            json!([
                {"name": "mdma-console", "version": "1", "update_version": "2"},
                {"name": "mdma-library", "version": "1", "update_version": null},
            ])
        );
    }

    #[tokio::test]
    async fn no_packages_is_an_empty_list() {
        let state = state_with(vec![], vec![]).await;
        assert_eq!(
            serde_json::to_value(super::package_views(&state).await).unwrap(),
            json!([])
        );
    }
}
