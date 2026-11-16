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
use gateway_client::{GatewayClient, LibraryRequest, LibraryResponse};
use library_ipc_client::{ContentHash, StringQuery, TrackInfo, TrackQuery};
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
    let given = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
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
    let query = match required_query(&params) {
        Ok(q) => q,
        Err(response) => return response,
    };
    json_or_500(gw(&state, move |client| track_search(client, query)).await)
}

/// The query for `params`, or the 400 response for a filter the library cannot parse and for no
/// text or filter at all (an empty match would otherwise look like "nothing matches").
#[allow(clippy::result_large_err)]
fn required_query(params: &SearchParams) -> Result<TrackQuery, Response> {
    match track_query_from(params) {
        Ok(q) if q.is_empty() => Err(fail(StatusCode::BAD_REQUEST, "no filter given".to_string())),
        Ok(q) => Ok(q),
        Err(message) => Err(fail(StatusCode::BAD_REQUEST, message)),
    }
}

/// Hashes of the tracks matching `query`, as the library finds them.
fn track_search(client: &GatewayClient, query: TrackQuery) -> Result<Vec<String>, String> {
    match library(client, &LibraryRequest::Search { query })? {
        LibraryResponse::SearchResults(tracks) => Ok(tracks
            .iter()
            .map(|t| t.content_hash.as_str().to_owned())
            .collect()),
        _ => Err("Unexpected library response".to_string()),
    }
}

/// Names of the playlists the library matches by name. The matching is the library's.
fn playlist_search(client: &GatewayClient, query: StringQuery) -> Result<Vec<String>, String> {
    match library(client, &LibraryRequest::PlaylistSearch { query })? {
        LibraryResponse::PlaylistNames(names) => {
            Ok(names.iter().map(|n| n.as_str().to_owned()).collect())
        }
        _ => Err("Unexpected library response".to_string()),
    }
}

/// The body of `GET /api/library/match`.
#[derive(Debug, Serialize)]
struct LibraryMatch {
    /// Tracks matching the text and the filters together.
    tracks: Vec<String>,
    /// Tracks matching the filters alone; null unless both text and filters are given.
    filter_tracks: Option<Vec<String>>,
    /// Playlists whose name matches the text; null without text.
    playlists: Option<Vec<String>>,
}

/// `GET /api/library/match?q=&bpm=&key=&added=&artist=`: everything the Library view needs for
/// one narrowing state, over one gateway connection: the matching tracks, the tracks passing the
/// filters alone (only when text and filters are both given) and the playlists whose name
/// matches the text (only with text). 400 for a filter the library cannot parse and for no
/// filter or text at all.
pub async fn library_match(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Response {
    let query = match required_query(&params) {
        Ok(q) => q,
        Err(response) => return response,
    };
    let filters = TrackQuery {
        any_text: None,
        ..query.clone()
    };
    let text = query.any_text.clone();
    json_or_500(
        gw(&state, move |client| {
            let tracks = track_search(client, query)?;
            let filter_tracks = match &text {
                Some(_) if !filters.is_empty() => Some(track_search(client, filters)?),
                _ => None,
            };
            let playlists = match text {
                Some(text) => Some(playlist_search(client, text)?),
                None => None,
            };
            Ok(LibraryMatch {
                tracks,
                filter_tracks,
                playlists,
            })
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
    async fn match_with_only_q_succeeds_and_returns_the_hash() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::Search { query },
            } if query.any_text.is_some() && query.bpm.is_none() => {
                library_ok(LibraryResponse::SearchResults(vec![bare("sha256:cbl")]))
            }
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        let res = send_get(test_app_with(gw.addr()), "/api/tracks/match?q=CBL", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!(["sha256:cbl"]));
    }

    /// The JSON of the string query for `text`, for comparing what the console forwarded.
    fn string_query_json(text: &str) -> serde_json::Value {
        serde_json::to_value(library_search::parse_string_query(text)).unwrap()
    }

    /// The queries of every PlaylistSearch the fake library received, as JSON.
    fn forwarded_playlist_queries(gw: &FakeGateway) -> Vec<serde_json::Value> {
        gw.requests()
            .into_iter()
            .filter_map(|req| match req {
                GatewayRequest::Library {
                    request: LibraryRequest::PlaylistSearch { query },
                } => Some(serde_json::to_value(query).unwrap()),
                _ => None,
            })
            .collect()
    }

    /// A library with two tracks (a: CBL by text only, b: passes the filters) for `/api/library/match`.
    fn library_match_gateway() -> FakeGateway {
        FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::Search { query },
            } => library_ok(LibraryResponse::SearchResults(
                match (query.any_text.is_some(), query.bpm.is_some()) {
                    (true, true) => vec![bare("sha256:both")],
                    (true, false) => vec![bare("sha256:both"), bare("sha256:text")],
                    (false, true) => vec![bare("sha256:both"), bare("sha256:tempo")],
                    (false, false) => vec![],
                },
            )),
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistSearch { .. },
            } => library_ok(LibraryResponse::PlaylistNames(vec![PlaylistName::new(
                "Carbon-Based-Lifeforms",
            )
            .unwrap()])),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    #[tokio::test]
    async fn library_match_with_text_and_filters_asks_three_things_on_one_connection() {
        let gw = library_match_gateway();
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/library/match?q=CBL&bpm=120..130",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "tracks": ["sha256:both"],
                "filter_tracks": ["sha256:both", "sha256:tempo"],
                "playlists": ["Carbon-Based-Lifeforms"],
            })
        );
        assert_eq!(gw.requests().len(), 3);
        assert_eq!(forwarded_playlist_queries(&gw), [string_query_json("CBL")]);
    }

    #[tokio::test]
    async fn library_match_with_text_only_has_no_filter_tracks() {
        let gw = library_match_gateway();
        let res = send_get(test_app_with(gw.addr()), "/api/library/match?q=CBL", None).await;
        assert_eq!(
            body_json(res).await,
            json!({
                "tracks": ["sha256:both", "sha256:text"],
                "filter_tracks": null,
                "playlists": ["Carbon-Based-Lifeforms"],
            })
        );
        assert_eq!(gw.requests().len(), 2);
    }

    #[tokio::test]
    async fn library_match_with_filters_only_skips_playlists() {
        let gw = library_match_gateway();
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/library/match?bpm=120..130",
            None,
        )
        .await;
        assert_eq!(
            body_json(res).await,
            json!({
                "tracks": ["sha256:both", "sha256:tempo"],
                "filter_tracks": null,
                "playlists": null,
            })
        );
        assert_eq!(gw.requests().len(), 1);
    }

    #[tokio::test]
    async fn library_match_blank_q_counts_as_absent() {
        let gw = library_match_gateway();
        // Blank text with a filter is a filters-only lookup.
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/library/match?q=%20&bpm=120..130",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert!(body["filter_tracks"].is_null() && body["playlists"].is_null());
        // Blank text alone, or nothing, is a 400 that never reaches the library.
        let before = gw.requests().len();
        for qs in ["q=%20", ""] {
            let res = send_get(
                test_app_with(gw.addr()),
                &format!("/api/library/match?{qs}"),
                None,
            )
            .await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{qs}");
        }
        assert_eq!(gw.requests().len(), before);
    }

    #[tokio::test]
    async fn library_match_bad_filter_is_400_and_library_error_is_500() {
        let gw = library_match_gateway();
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/library/match?bpm=fast",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());

        let down = FakeGateway::start(|_| GatewayResponse::Error {
            message: "down".into(),
        });
        let res = send_get(test_app_with(down.addr()), "/api/library/match?q=CBL", None).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// The console forwards the trimmed text as the library's own string query. Matching is the
    /// library's (tested in library_service), so nothing about matching rules is asserted here.
    #[tokio::test]
    async fn library_match_forwards_the_trimmed_text_as_a_playlist_search() {
        let gw = library_match_gateway();
        send_get(
            test_app_with(gw.addr()),
            "/api/library/match?q=%20CBL%20",
            None,
        )
        .await;
        assert_eq!(forwarded_playlist_queries(&gw), [string_query_json("CBL")]);
    }

    #[test]
    fn blank_text_params_count_as_absent() {
        for blank in ["", " ", "  \t"] {
            let q = track_query_from(&SearchParams {
                q: Some(blank.into()),
                artist: Some(blank.into()),
                bpm: Some(blank.into()),
                ..Default::default()
            })
            .unwrap();
            assert!(q.is_empty(), "{blank:?}");
        }
    }

    #[tokio::test]
    async fn tracks_match_with_blank_q_is_400() {
        let gw = FakeGateway::start(|_| playback_ok(None));
        let res = send_get(test_app_with(gw.addr()), "/api/tracks/match?q=%20", None).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(gw.requests().is_empty());
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
