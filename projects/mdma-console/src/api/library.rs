//! Library wire types for the new web UI.
//!
//! `ApiTrack` is the one track shape the browser sees. Its field names are exactly the ones
//! `static/js/logic.js` reads; the old UI keeps using `TrackInfoJson` and is not affected.

use super::{fail, gw, json_or_500, library};
use crate::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use gateway_client::{LibraryRequest, LibraryResponse};
use library_ipc_client::{ContentHash, TrackInfo, TrackQuery};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The text and range filters shared by the old `/library/search` and the new
/// `/api/tracks/match`.
#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub artist: Option<String>,
    pub bpm: Option<String>,
    pub key: Option<String>,
    pub added: Option<String>,
}

/// Build the library query for `params`. Empty values count as absent; a value the library's
/// query syntax rejects is an error message for a 400.
pub fn track_query_from(params: &SearchParams) -> Result<TrackQuery, String> {
    let given = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
    let mut query = TrackQuery::default();
    if let Some(q) = given(&params.q) {
        query.any_text = Some(library_search::parse_string_query(&q));
    }
    if let Some(artist) = given(&params.artist) {
        query.artist = Some(library_search::parse_string_query(&artist));
    }
    if let Some(bpm) = given(&params.bpm) {
        query.bpm = Some(library_search::parse_numeric_query(&bpm).map_err(|e| e.to_string())?);
    }
    if let Some(key) = given(&params.key) {
        query.key = Some(library_search::parse_key_query(&key).map_err(|e| e.to_string())?);
    }
    if let Some(added) = given(&params.added) {
        query.added = Some(library_search::parse_date_query(&added).map_err(|e| e.to_string())?);
    }
    Ok(query)
}

/// Hashes of a playlist file: the first whitespace-separated token of each line.
pub fn playlist_hashes(content: &str) -> Vec<ContentHash> {
    content
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|token| ContentHash::new(token.to_string()))
        .collect()
}

/// `GET /api/tracks`: the whole catalogue in one response.
pub async fn tracks(State(state): State<Arc<AppState>>) -> Response {
    json_or_500(
        gw(&state, |client| {
            match library(client, &LibraryRequest::ListTracks { limit: None })? {
                LibraryResponse::Tracks(tracks) => {
                    Ok(tracks.iter().map(ApiTrack::from).collect::<Vec<_>>())
                }
                _ => Err("Unexpected library response".to_string()),
            }
        })
        .await,
    )
}

#[derive(Debug, Serialize)]
struct ApiPlaylist {
    name: String,
    hashes: Vec<String>,
}

/// `GET /api/playlists`: every playlist with its hashes, over one gateway connection.
pub async fn playlists(State(state): State<Arc<AppState>>) -> Response {
    let reply = gw(&state, |client| -> Result<Vec<ApiPlaylist>, String> {
        let names = match library(client, &LibraryRequest::PlaylistList)? {
            LibraryResponse::PlaylistNames(names) => names,
            _ => return Err("Unexpected library response".to_string()),
        };
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let content =
                match library(client, &LibraryRequest::PlaylistGet { name: name.clone() })? {
                    LibraryResponse::PlaylistContent(content) => content,
                    _ => return Err("Unexpected library response".to_string()),
                };
            out.push(ApiPlaylist {
                name: name.as_str().to_owned(),
                hashes: playlist_hashes(&content)
                    .iter()
                    .map(|h| h.as_str().to_owned())
                    .collect(),
            });
        }
        Ok(out)
    })
    .await;
    json_or_500(reply)
}

/// `GET /api/tracks/match?bpm=&key=&added=&q=&artist=`: hashes of the tracks the library says
/// match. 400 for a filter the library cannot parse, and for no filter at all (an empty match
/// would otherwise look like "nothing matches").
pub async fn tracks_match(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Response {
    let query = match track_query_from(&params) {
        Ok(q) if q.is_empty() => {
            return fail(StatusCode::BAD_REQUEST, "no filter given".to_string())
        }
        Ok(q) => q,
        Err(message) => return fail(StatusCode::BAD_REQUEST, message),
    };
    json_or_500(
        gw(&state, move |client| {
            match library(client, &LibraryRequest::Search { query })? {
                LibraryResponse::SearchResults(tracks) => Ok(tracks
                    .iter()
                    .map(|t| t.content_hash.as_str().to_owned())
                    .collect::<Vec<_>>()),
                _ => Err("Unexpected library response".to_string()),
            }
        })
        .await,
    )
}

/// One catalogue entry, as served by `GET /api/tracks`.
///
/// Absent facts are `null`, never omitted, so the JS can rely on every key being present.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiTrack {
    /// Content hash with its prefix, e.g. "sha256:ab12...". The identity used everywhere.
    pub hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u32>,
    pub disc: Option<u32>,
    /// Beats per minute as a plain number (hundredths preserved), or null.
    pub bpm: Option<f32>,
    /// Musical key in `Key`'s Display form, or null.
    pub key: Option<String>,
    pub duration_secs: Option<u32>,
    /// When the track was added, as the library stores it (ISO 8601 / RFC 3339), or null.
    pub added: Option<String>,
    /// True when `/cover/<hash>` has an image. The browser requests covers only for these.
    pub has_cover: bool,
}

impl From<&TrackInfo> for ApiTrack {
    fn from(t: &TrackInfo) -> Self {
        Self {
            hash: t.content_hash.as_str().to_owned(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            track_number: t.track_number,
            disc: t.disc_number,
            bpm: t.bpm.map(|b| b.as_f32()),
            key: t.key.map(|k| k.to_string()),
            duration_secs: t.duration.map(|d| d.value()),
            added: t.added.clone(),
            has_cover: t.cover_art_path.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::{Bpm, DurationSeconds, Key};

    fn bare(hash: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
        }
    }

    fn keys(v: &serde_json::Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    /// The JS contract (static/js/logic.js): exactly these keys, always all present.
    const KEYS: [&str; 11] = [
        "added",
        "album",
        "artist",
        "bpm",
        "disc",
        "duration_secs",
        "has_cover",
        "hash",
        "key",
        "title",
        "track_number",
    ];

    #[test]
    fn full_track_has_exactly_the_js_keys() {
        let mut t = bare("sha256:abc123");
        t.title = Some("Bloom".into());
        t.artist = Some("Carbon".into());
        t.album = Some("Twentythree".into());
        t.duration = Some(DurationSeconds::new(180));
        t.bpm = Some(Bpm::from_u32(128).unwrap());
        t.key = Some(Key::from_traditional("C Major").unwrap());
        t.cover_art_path = Some("cover-art/abc123.jpg".into());
        t.track_number = Some(3);
        t.disc_number = Some(2);
        t.added = Some("2026-11-01T10:00:00+00:00".into());

        let json = serde_json::to_value(ApiTrack::from(&t)).unwrap();
        assert_eq!(keys(&json), KEYS);
        assert_eq!(json["hash"], "sha256:abc123");
        assert_eq!(json["title"], "Bloom");
        assert_eq!(json["track_number"], 3);
        assert_eq!(json["disc"], 2);
        assert_eq!(json["bpm"], 128.0);
        assert_eq!(json["key"], t.key.unwrap().to_string());
        assert_eq!(json["duration_secs"], 180);
        assert_eq!(json["added"], "2026-11-01T10:00:00+00:00");
        assert_eq!(json["has_cover"], true);
    }

    #[test]
    fn minimal_track_keeps_every_key_as_null_or_false() {
        let json = serde_json::to_value(ApiTrack::from(&bare("sha256:dead"))).unwrap();
        assert_eq!(keys(&json), KEYS);
        for k in KEYS.iter().filter(|k| **k != "hash" && **k != "has_cover") {
            assert!(json[*k].is_null(), "{k} should be null");
        }
        assert_eq!(json["has_cover"], false);
    }

    #[test]
    fn playlist_lines_give_their_first_token() {
        let hashes = playlist_hashes("sha256:a  # Bloom\n\n  sha256:b\nsha256:c\t- x\n");
        let got: Vec<_> = hashes.iter().map(|h| h.as_str()).collect();
        assert_eq!(got, ["sha256:a", "sha256:b", "sha256:c"]);
    }

    #[test]
    fn query_ignores_empty_params_and_rejects_bad_ones() {
        let q = track_query_from(&SearchParams {
            bpm: Some(String::new()),
            ..Default::default()
        })
        .unwrap();
        assert!(q.is_empty());

        let q = track_query_from(&SearchParams {
            bpm: Some("120..130".into()),
            key: Some("Am+-1~".into()),
            added: Some(">2026-10-01".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(q.bpm.is_some() && q.key.is_some() && q.added.is_some());

        for bad in [
            SearchParams {
                bpm: Some("fast".into()),
                ..Default::default()
            },
            SearchParams {
                key: Some("H#".into()),
                ..Default::default()
            },
            SearchParams {
                added: Some("whenever".into()),
                ..Default::default()
            },
        ] {
            assert!(track_query_from(&bad).is_err(), "{bad:?}");
        }
    }

    /// Every format the narrow bar sends (lanes M3): ranges with open ends as 0 or 999,
    /// traditional key notation with and without the mix-well suffix, and `>date`.
    #[test]
    fn query_accepts_every_format_the_narrow_bar_sends() {
        let param = |bpm: Option<&str>, key: Option<&str>, added: Option<&str>| SearchParams {
            bpm: bpm.map(String::from),
            key: key.map(String::from),
            added: added.map(String::from),
            ..Default::default()
        };
        for bpm in ["120..130", "0..130", "120..999", "0..999"] {
            let q = track_query_from(&param(Some(bpm), None, None))
                .unwrap_or_else(|e| panic!("bpm {bpm}: {e}"));
            assert!(q.bpm.is_some(), "bpm {bpm}");
        }
        for note in [
            "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
        ] {
            for mode in ["", "m"] {
                for suffix in ["", "+-1~"] {
                    let key = format!("{note}{mode}{suffix}");
                    let q = track_query_from(&param(None, Some(&key), None))
                        .unwrap_or_else(|e| panic!("key {key}: {e}"));
                    assert!(q.key.is_some(), "key {key}");
                }
            }
        }
        let q = track_query_from(&param(None, None, Some(">2026-10-01"))).unwrap();
        assert!(q.added.is_some());
    }

    // ---- handlers, against a fake gateway ----

    use crate::test_support::{
        body_json, library_ok, playback_ok, send_get, test_app, test_app_with, FakeGateway,
    };
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::PlaylistName;
    use serde_json::json;

    #[tokio::test]
    async fn api_tracks_lists_everything_with_the_js_keys() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::ListTracks { limit: None },
            } => {
                let mut t = bare("sha256:a");
                t.title = Some("Bloom".into());
                library_ok(LibraryResponse::Tracks(vec![t, bare("sha256:b")]))
            }
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        let res = send_get(test_app_with(gw.addr()), "/api/tracks", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        let list = json.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(keys(&list[0]), KEYS);
        assert_eq!(list[0]["title"], "Bloom");
        assert_eq!(list[1]["hash"], "sha256:b");
    }

    #[tokio::test]
    async fn api_tracks_library_error_is_500() {
        let gw = FakeGateway::start(|_| GatewayResponse::Error {
            message: "down".into(),
        });
        let res = send_get(test_app_with(gw.addr()), "/api/tracks", None).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn api_playlists_gives_names_with_hashes_over_one_connection() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistList,
            } => library_ok(LibraryResponse::PlaylistNames(vec![
                PlaylistName::new("warmup").unwrap(),
                PlaylistName::new("peak").unwrap(),
            ])),
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistGet { name },
            } => library_ok(LibraryResponse::PlaylistContent(match name.as_str() {
                "warmup" => "sha256:a # one\nsha256:b\n".to_string(),
                _ => "sha256:c\n".to_string(),
            })),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        let res = send_get(test_app_with(gw.addr()), "/api/playlists", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!([
                {"name": "warmup", "hashes": ["sha256:a", "sha256:b"]},
                {"name": "peak", "hashes": ["sha256:c"]},
            ])
        );
        assert_eq!(gw.requests().len(), 3);
    }

    #[tokio::test]
    async fn match_with_a_bad_param_is_400_without_calling_the_library() {
        let gw = FakeGateway::start(|_| playback_ok(None));
        for qs in ["bpm=fast", "key=H%23", "added=whenever", ""] {
            let res = send_get(
                test_app_with(gw.addr()),
                &format!("/api/tracks/match?{qs}"),
                None,
            )
            .await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{qs}");
            assert!(body_json(res).await["error"].is_string());
        }
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn match_returns_the_hashes_the_library_found() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::Search { query },
            } if query.bpm.is_some() => library_ok(LibraryResponse::SearchResults(vec![
                bare("sha256:a"),
                bare("sha256:b"),
            ])),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/tracks/match?bpm=120..130",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!(["sha256:a", "sha256:b"]));
    }

    #[tokio::test]
    async fn root_is_the_new_shell_and_classic_the_old_ui() {
        let res = send_get(test_app(), "/", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = crate::test_support::body_text(res).await;
        assert!(body.contains("<title>MDMA Player</title>"));

        let res = send_get(test_app(), "/classic", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = crate::test_support::body_text(res).await;
        assert!(body.contains("<title>MDMA Console</title>"));
    }
}
