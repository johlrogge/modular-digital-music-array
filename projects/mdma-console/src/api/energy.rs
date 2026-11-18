//! Energy for the new web UI (ADR-016 amendment 2026-11-18): set or clear the user's energy
//! (1..10) on tracks. Like marks it is a set operation, never a toggle, and what the tracks carry
//! is `ApiTrack.energy`, the one source.

use super::marks::apply_each;
use super::{error_reply, lib, parse_hashes, respond};
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::Response, Json};
use library_ipc_client::{
    user_fact_source, ClientError, ContentHash, EnergyLevel, FactSource, MusicValue,
};
use mdma_client::LibraryBackend;
use serde::{Deserialize, Deserializer};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct EnergyRequest {
    hashes: Vec<String>,
    /// Required: 1..10 sets the user's energy, null clears it. A missing `level` is `None` here
    /// (and a 422), never a clear. Wider than a level so that one out of range is the
    /// validator's 422, not a parse failure.
    #[serde(default, deserialize_with = "present")]
    level: Option<Option<i64>>,
}

/// A field that was given, null or not: `Some(value)`. The field's `default` is the absent case.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<i64>>, D::Error> {
    Option::<i64>::deserialize(d).map(Some)
}

/// `POST /api/energy {hashes, level: 1..10 | null}`. Every hash is resolved against the library
/// before anything is written; the user's source states every write.
///
/// Replies exactly as `POST /api/marks` does: 200 `{changed, tracks}` (a track already at that
/// energy, or cleared of none, is not in `changed`), 207 when it stopped partway, the failure's
/// status when the first write failed, 400 without hashes, 422 for a level outside 1..10 or a
/// hash the library cannot resolve (nothing written).
pub async fn energy(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnergyRequest>,
) -> Response {
    let hashes = match parse_hashes(req.hashes) {
        Ok(h) => h,
        Err(response) => return response,
    };
    let unprocessable =
        |message: String| respond(Ok(error_reply(StatusCode::UNPROCESSABLE_ENTITY, message)));
    let level = match req.level {
        None => return unprocessable("level is required: 1 to 10, or null to clear".to_string()),
        Some(None) => None,
        Some(Some(n)) => match u8::try_from(n).ok().and_then(|n| EnergyLevel::new(n).ok()) {
            Some(level) => Some(level),
            None => {
                return unprocessable(format!(
                    "energy must be a whole number from 1 to 10, or null to clear it (got {n})"
                ))
            }
        },
    };
    let source = user_fact_source(env!("CARGO_PKG_VERSION"));
    respond(
        lib(&state, move |backend| {
            apply_each(backend, &hashes, |hash| {
                change(backend, hash, level, &source)
            })
        })
        .await,
    )
}

/// Whether the track's energy changed.
///
/// The catalogue shows the last live energy of any source, so setting writes unless it already
/// shows that level, and clearing retracts every level the user ever asserted (an older one would
/// surface again otherwise). A track that shows no energy has nothing to clear.
fn change(
    backend: &LibraryBackend,
    hash: &ContentHash,
    level: Option<EnergyLevel>,
    source: &FactSource,
) -> Result<bool, ClientError> {
    let shown = backend.get_track(hash)?.energy;
    match level {
        Some(level) if shown == Some(level) => Ok(false),
        Some(level) => {
            backend.write_fact_as(hash, MusicValue::Energy(level), source)?;
            Ok(true)
        }
        None if shown.is_none() => Ok(false),
        None => {
            let mut retracted = false;
            for level in EnergyLevel::all() {
                retracted |= backend.retract_fact_as(hash, MusicValue::Energy(level), source)?;
            }
            Ok(retracted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        bare_track, body_json, library_ok, send_post_json, test_app_with, FakeGateway,
    };
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::ProtocolError;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn full(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn gateway(initial: &[(char, Option<u8>)]) -> FakeGateway {
        gateway_failing(initial, None)
    }

    /// What a library holds: the tracks in `initial`, each with the user's live energy
    /// assertions in the order written (the last one is the one shown, as the projection does).
    /// A write adds an assertion; a retract removes every assertion of that level. A write for
    /// the track `fail_on` fails.
    fn gateway_failing(initial: &[(char, Option<u8>)], fail_on: Option<char>) -> FakeGateway {
        let state: Mutex<HashMap<String, Vec<u8>>> = Mutex::new(
            initial
                .iter()
                .map(|(c, e)| (full(*c), e.iter().copied().collect()))
                .collect(),
        );
        FakeGateway::start(move |req| {
            let mut state = state.lock().unwrap();
            match req {
                GatewayRequest::Library {
                    request: LibraryRequest::GetTrack { hash },
                } => match state.get(hash.as_str()) {
                    Some(asserted) => {
                        let mut t = bare_track(hash.as_str());
                        t.energy = asserted.last().map(|n| EnergyLevel::new(*n).unwrap());
                        library_ok(LibraryResponse::Track(Box::new(t)))
                    }
                    None => library_ok(LibraryResponse::Error(ProtocolError::TrackNotFound {
                        hash: hash.as_str().to_owned(),
                    })),
                },
                GatewayRequest::Library {
                    request:
                        LibraryRequest::WriteFact {
                            hash,
                            fact: MusicValue::Energy(level),
                            ..
                        },
                } if fail_on.is_some_and(|c| hash.as_str() == full(c)) => {
                    library_ok(LibraryResponse::Error(ProtocolError::Internal {
                        message: "disk full".into(),
                    }))
                }
                GatewayRequest::Library {
                    request:
                        LibraryRequest::WriteFact {
                            hash,
                            fact: MusicValue::Energy(level),
                            ..
                        },
                } => {
                    state
                        .entry(hash.as_str().to_owned())
                        .or_default()
                        .push(level.value());
                    library_ok(LibraryResponse::FactWritten)
                }
                GatewayRequest::Library {
                    request:
                        LibraryRequest::RetractFact {
                            hash,
                            fact: MusicValue::Energy(level),
                            ..
                        },
                } => {
                    let asserted = state.entry(hash.as_str().to_owned()).or_default();
                    let before = asserted.len();
                    asserted.retain(|n| *n != level.value());
                    if asserted.len() < before {
                        library_ok(LibraryResponse::FactRetracted)
                    } else {
                        library_ok(LibraryResponse::NothingToRetract)
                    }
                }
                other => GatewayResponse::Error {
                    message: format!("unexpected {other:?}"),
                },
            }
        })
    }

    fn writes(gw: &FakeGateway) -> Vec<LibraryRequest> {
        gw.requests()
            .into_iter()
            .filter_map(|r| match r {
                GatewayRequest::Library {
                    request:
                        request
                        @ (LibraryRequest::WriteFact { .. } | LibraryRequest::RetractFact { .. }),
                } => Some(request),
                _ => None,
            })
            .collect()
    }

    async fn post(gw: &FakeGateway, body: serde_json::Value) -> Response {
        send_post_json(test_app_with(gw.addr()), "/api/energy", &body).await
    }

    #[tokio::test]
    async fn setting_writes_the_level_as_the_user_and_answers_the_track_as_it_is_now() {
        let gw = gateway(&[('a', None)]);
        let res = post(&gw, json!({"hashes": [full('a')], "level": 7})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        assert_eq!(body["tracks"][0]["hash"], full('a'));
        assert_eq!(body["tracks"][0]["energy"], 7);
        let sent = writes(&gw);
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            LibraryRequest::WriteFact { fact, source, .. } => {
                assert_eq!(*fact, MusicValue::Energy(EnergyLevel::new(7).unwrap()));
                let source = source.as_ref().expect("the writer states its source");
                assert!(source.same_source(&user_fact_source("any")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn setting_a_new_level_replaces_the_shown_one() {
        let gw = gateway(&[('a', Some(3))]);
        let res = post(&gw, json!({"hashes": [full('a')], "level": 9})).await;
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        assert_eq!(body["tracks"][0]["energy"], 9);
    }

    #[tokio::test]
    async fn clearing_retracts_the_users_energy() {
        let gw = gateway(&[('a', Some(5))]);
        let res = post(&gw, json!({"hashes": [full('a')], "level": null})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        assert_eq!(body["tracks"][0]["energy"], json!(null));
        assert!(writes(&gw)
            .iter()
            .all(|r| matches!(r, LibraryRequest::RetractFact { .. })));
    }

    #[tokio::test]
    async fn repeating_the_same_level_is_a_no_op() {
        let gw = gateway(&[('a', Some(7))]);
        let res = post(&gw, json!({"hashes": [full('a')], "level": 7})).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([]));
        assert_eq!(body["tracks"][0]["energy"], 7);
        assert!(writes(&gw).is_empty());
    }

    #[tokio::test]
    async fn clearing_a_track_without_energy_is_a_no_op() {
        let gw = gateway(&[('a', None)]);
        let res = post(&gw, json!({"hashes": [full('a')], "level": null})).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["changed"], json!([]));
        assert!(writes(&gw).is_empty());
    }

    #[tokio::test]
    async fn a_set_of_hashes_is_changed_in_order_and_only_where_it_differs() {
        let gw = gateway(&[('a', None), ('b', Some(4)), ('c', None)]);
        let res = post(
            &gw,
            json!({"hashes": [full('b'), full('a'), full('b'), full('c')], "level": 4}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a'), full('c')]));
        assert_eq!(body["tracks"].as_array().unwrap().len(), 3);
        assert_eq!(writes(&gw).len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_hash_is_422_and_nothing_is_written() {
        let gw = gateway(&[('a', None)]);
        let res = post(&gw, json!({"hashes": [full('a'), full('f')], "level": 6})).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_json(res).await["error"].is_string());
        assert!(writes(&gw).is_empty());
    }

    #[tokio::test]
    async fn a_level_outside_one_to_ten_is_422_and_sends_nothing() {
        let gw = gateway(&[('a', None)]);
        for level in [
            json!(0),
            json!(11),
            json!(-1),
            json!(300),
            json!(1000000000000i64),
        ] {
            let res = post(&gw, json!({"hashes": [full('a')], "level": level})).await;
            assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{level}");
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn a_missing_level_is_422_and_never_clears() {
        let gw = gateway(&[('a', Some(5))]);
        let res = post(&gw, json!({"hashes": [full('a')]})).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn clearing_leaves_no_energy_even_after_several_sets() {
        let gw = gateway(&[('a', None)]);
        for body in [
            json!({"hashes": [full('a')], "level": 3}),
            json!({"hashes": [full('a')], "level": 9}),
            json!({"hashes": [full('a')], "level": null}),
        ] {
            assert_eq!(post(&gw, body).await.status(), StatusCode::OK);
        }
        // Both the 3 and the 9 the user asserted are gone: neither resurfaces.
        let res = post(&gw, json!({"hashes": [full('a')], "level": null})).await;
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([]));
        assert_eq!(body["tracks"][0]["energy"], json!(null));
    }

    #[tokio::test]
    async fn stopping_partway_is_207_with_what_was_done() {
        let gw = gateway_failing(&[('a', None), ('b', None), ('c', None)], Some('b'));
        let res = post(
            &gw,
            json!({"hashes": [full('a'), full('b'), full('c')], "level": 7}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::MULTI_STATUS);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([full('a')]));
        assert_eq!(body["tracks"].as_array().unwrap().len(), 1);
        assert_eq!(body["tracks"][0]["energy"], 7);
        assert!(body["error"].as_str().unwrap().contains("disk full"));
    }

    #[tokio::test]
    async fn a_first_write_that_fails_is_502_with_nothing_changed() {
        let gw = gateway_failing(&[('a', None)], Some('a'));
        let res = post(&gw, json!({"hashes": [full('a')], "level": 7})).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = body_json(res).await;
        assert_eq!(body["changed"], json!([]));
        assert_eq!(body["tracks"], json!([]));
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn no_hashes_is_400_and_sends_nothing() {
        let gw = gateway(&[]);
        let res = post(&gw, json!({"hashes": [], "level": 5})).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn energy_without_a_gateway_is_503() {
        let res = send_post_json(
            crate::test_support::test_app(),
            "/api/energy",
            &json!({"hashes": [full('a')], "level": 5}),
        )
        .await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
