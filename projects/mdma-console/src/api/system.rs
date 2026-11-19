//! Node-level reads for the settings pages. `GET /api/inbox` lists the files waiting in the inbox
//! (the same queue the classic page renders), as paths relative to the inbox. `GET /api/packages`
//! lists the installed packages and the updates the last check found.

use super::{gw, json_or_failure, library, unexpected_library_response};
use crate::{AppState, PackageView};
use axum::{
    extract::State,
    response::{IntoResponse, Json, Response},
};
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

/// `GET /api/packages`: the installed MDMA packages, each with the version an update would bring
/// (`update_version`, null when it is current). The installed list is read afresh; when that fails
/// the last one read is used, as the classic page does. Updates are those the last check found.
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
