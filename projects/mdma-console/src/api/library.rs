//! Library wire types for the new web UI.
//!
//! `ApiTrack` is the one track shape the browser sees. Its field names are exactly the ones
//! `static/js/logic.js` reads; the old UI keeps using `TrackInfoJson` and is not affected.

use super::{fail, gw, json_or_failure, lib, library, unexpected_library_response};
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use gateway_client::{GatewayClient, LibraryRequest, LibraryResponse};
use library_ipc_client::{
    track_hashes, unresolved_entries, ClientError, Key, Mark, Mode, PlaylistEntry, PlaylistName,
    PlaylistSnapshot, PlaylistVersion, StringQuery, TrackInfo, TrackQuery,
};
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
    /// Energy level 1-10 (`7`, `5..8`, `7+-2`).
    pub energy: Option<String>,
    pub added: Option<String>,
    /// A mark's name: "liked" is the default mark.
    pub mark: Option<String>,
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
    if let Some(energy) = given(&params.energy) {
        query.energy = Some(
            library_search::parse_energy_query(&energy)
                .map_err(|e| e.to_string())?
                .0,
        );
    }
    if let Some(key) = given(&params.key) {
        query.key = Some(library_search::parse_key_query(&key).map_err(|e| e.to_string())?);
    }
    if let Some(added) = given(&params.added) {
        query.added = Some(library_search::parse_date_query(&added).map_err(|e| e.to_string())?);
    }
    query.mark = mark_param(params.mark.as_deref())?;
    Ok(query)
}

/// The mark a request names: blank or absent is none, "liked" the default mark, any other valid
/// name a named one; a bad name is the message for a 400 or 422.
pub fn mark_param(name: Option<&str>) -> Result<Option<Mark>, String> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        None => Ok(None),
        Some(n) => n.parse::<Mark>().map(Some).map_err(|e| e.to_string()),
    }
}

/// `GET /api/tracks`: the whole catalogue in one response.
pub async fn tracks(State(state): State<Arc<AppState>>) -> Response {
    json_or_failure(
        gw(&state, |client| {
            match library(client, &LibraryRequest::ListTracks { limit: None })? {
                LibraryResponse::Tracks(tracks) => {
                    Ok(tracks.iter().map(ApiTrack::from).collect::<Vec<_>>())
                }
                _ => Err(unexpected_library_response()),
            }
        })
        .await,
    )
}

/// A playlist for the cards and counts: the full hash of every entry the library resolved, in
/// file order with duplicates, and how many entries it could not resolve.
#[derive(Debug, Serialize)]
struct ApiPlaylist {
    name: String,
    hashes: Vec<String>,
    unavailable: usize,
}

impl ApiPlaylist {
    fn new(name: &PlaylistName, entries: &[PlaylistEntry]) -> Self {
        Self {
            name: name.as_str().to_owned(),
            hashes: track_hashes(entries)
                .iter()
                .map(|h| h.as_str().to_owned())
                .collect(),
            unavailable: unresolved_entries(entries).count(),
        }
    }
}

/// One line of a playlist page, tagged by `status` for the browser.
#[derive(Debug, Serialize)]
#[serde(tag = "status")]
enum ApiEntry {
    /// The line resolved to this track.
    Track { hash: String },
    /// The line did not resolve: the token as stored, and why.
    Unresolved { token: String, reason: String },
}

impl From<&PlaylistEntry> for ApiEntry {
    fn from(entry: &PlaylistEntry) -> Self {
        match entry {
            PlaylistEntry::Track { hash } => ApiEntry::Track {
                hash: hash.as_str().to_owned(),
            },
            PlaylistEntry::Unresolved { error } => ApiEntry::Unresolved {
                token: error.hash().as_str().to_owned(),
                reason: error.to_string(),
            },
        }
    }
}

/// A playlist's lines with the version they were read at: the one shape every playlist read and
/// edit answer carries (`snapshot`, `current`), so the browser redraws from any of them.
#[derive(Debug, Serialize)]
pub struct ApiSnapshot {
    version: PlaylistVersion,
    entries: Vec<ApiEntry>,
}

impl From<&PlaylistSnapshot> for ApiSnapshot {
    fn from(snapshot: &PlaylistSnapshot) -> Self {
        Self {
            version: snapshot.version.clone(),
            entries: snapshot.entries.iter().map(ApiEntry::from).collect(),
        }
    }
}

/// The body of `GET /api/playlists/{name}`.
#[derive(Debug, Serialize)]
struct ApiPlaylistPage {
    name: String,
    #[serde(flatten)]
    snapshot: ApiSnapshot,
}

/// The playlist `name` from a path, or the 400 response for a name no playlist can have (the
/// library is not asked). A bad name in a create body is 422 instead.
#[allow(clippy::result_large_err)]
pub fn playlist_name(name: &str) -> Result<PlaylistName, Response> {
    PlaylistName::new(name).map_err(|e| fail(StatusCode::BAD_REQUEST, e.to_string()))
}

/// `GET /api/playlists`: every playlist with the full hashes of its resolved entries and the
/// count of unresolved ones, over one gateway connection.
pub async fn playlists(State(state): State<Arc<AppState>>) -> Response {
    json_or_failure(
        lib(&state, |library| {
            library
                .playlist_list()?
                .iter()
                .map(|name| Ok(ApiPlaylist::new(name, &library.playlist_entries(name)?)))
                .collect::<Result<Vec<_>, ClientError>>()
        })
        .await,
    )
}

/// `GET /api/playlists/{name}`: every line of the playlist in file order, duplicates kept, an
/// unresolved line in its place, and the version they were read at (ADR-027 §9). 404 for a
/// playlist that does not exist. A name no playlist can have is 400 ([`playlist_name`]). A library
/// that does not know `PlaylistSnapshot` (an old node) is a 502: there is no fallback.
pub async fn playlist(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let name = match playlist_name(&name) {
        Ok(n) => n,
        Err(response) => return response,
    };
    json_or_failure(
        gw(&state, move |client| {
            match library(
                client,
                &LibraryRequest::PlaylistSnapshot { name: name.clone() },
            )? {
                LibraryResponse::PlaylistSnapshot(snapshot) => Ok(ApiPlaylistPage {
                    name: name.as_str().to_owned(),
                    snapshot: ApiSnapshot::from(&snapshot),
                }),
                _ => Err(unexpected_library_response()),
            }
        })
        .await,
    )
}

/// `GET /api/tracks/match?bpm=&energy=&key=&added=&mark=&q=&artist=`: hashes of the tracks the library says
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
    json_or_failure(gw(&state, move |client| track_search(client, query)).await)
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
fn track_search(client: &GatewayClient, query: TrackQuery) -> Result<Vec<String>, ClientError> {
    match library(client, &LibraryRequest::Search { query })? {
        LibraryResponse::SearchResults(tracks) => Ok(tracks
            .iter()
            .map(|t| t.content_hash.as_str().to_owned())
            .collect()),
        _ => Err(unexpected_library_response()),
    }
}

/// Names of the playlists the library matches by name. The matching is the library's.
fn playlist_search(client: &GatewayClient, query: StringQuery) -> Result<Vec<String>, ClientError> {
    match library(client, &LibraryRequest::PlaylistSearch { query })? {
        LibraryResponse::PlaylistNames(names) => {
            Ok(names.iter().map(|n| n.as_str().to_owned()).collect())
        }
        _ => Err(unexpected_library_response()),
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

/// `GET /api/library/match?q=&bpm=&energy=&key=&added=&mark=&artist=`: everything the Library view needs for
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
    json_or_failure(
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
            Ok::<_, ClientError>(LibraryMatch {
                tracks,
                filter_tracks,
                playlists,
            })
        })
        .await,
    )
}

/// A key in all three notations, so the browser picks one and never converts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiKey {
    /// `Key`'s Display form, e.g. "A Minor".
    pub musical: String,
    /// Camelot wheel, e.g. "8A".
    pub camelot: String,
    /// Open Key, e.g. "1m".
    pub open_key: String,
}

impl From<Key> for ApiKey {
    fn from(k: Key) -> Self {
        Self {
            musical: k.to_string(),
            camelot: k.to_camelot(),
            open_key: k.to_open_key(),
        }
    }
}

/// The mode as the filter stores it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum ApiMode {
    #[serde(rename = "min")]
    Minor,
    #[serde(rename = "maj")]
    Major,
}

impl From<Mode> for ApiMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Minor => ApiMode::Minor,
            Mode::Major => ApiMode::Major,
        }
    }
}

/// One key of the filter sheet: the three notations plus the `note` (sharp spelling) and `mode`
/// the filter stores and sends on; `parse_key_query` takes them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiFilterKey {
    #[serde(flatten)]
    pub key: ApiKey,
    pub note: &'static str,
    pub mode: ApiMode,
    /// Whether the requested reach includes this key; absent from the plain list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included: Option<bool>,
}

impl From<Key> for ApiFilterKey {
    fn from(k: Key) -> Self {
        Self {
            key: ApiKey::from(k),
            note: k.pitch().as_sharp(),
            mode: k.mode().into(),
            included: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct KeysParams {
    /// A key query (`8A+-2d,+7`): marks the keys it includes.
    pub reach: Option<String>,
}

/// `GET /api/keys`: the 24 keys the filter sheet offers, in wheel order (`Key::camelot_wheel`:
/// 1A..12A, then 1B..12B, so the sheet's two rows line up and a relative pair shares a column).
/// They never change, so the plain list is cached for a day; the browser asks with `?v=wheel`
/// (ignored here) so a change of order gets a new URL. With `?reach=<key query>` every entry also says whether
/// the query includes it (`KeyQuery::matches`, so the browser never works the wheel out itself);
/// a query `parse_key_query` rejects is a 400.
pub async fn keys(Query(params): Query<KeysParams>) -> Response {
    use axum::{http::header::CACHE_CONTROL, response::IntoResponse, Json};
    let reach = params
        .reach
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let Some(reach) = reach else {
        let keys = Key::camelot_wheel()
            .map(ApiFilterKey::from)
            .collect::<Vec<_>>();
        return ([(CACHE_CONTROL, "public, max-age=86400")], Json(keys)).into_response();
    };
    match library_search::parse_key_query(reach) {
        Ok(query) => {
            let keys = Key::camelot_wheel()
                .map(|k| ApiFilterKey {
                    included: Some(query.matches(k)),
                    ..ApiFilterKey::from(k)
                })
                .collect::<Vec<_>>();
            ([(CACHE_CONTROL, "private, max-age=60")], Json(keys)).into_response()
        }
        Err(e) => fail(StatusCode::BAD_REQUEST, e.to_string()),
    }
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
    /// The key in every notation the console can show, or null.
    pub key: Option<ApiKey>,
    /// Energy level 1-10 as a plain number, or null.
    pub energy: Option<u8>,
    pub duration_secs: Option<u32>,
    /// When the track was added, as the library stores it (ISO 8601 / RFC 3339), or null.
    pub added: Option<String>,
    /// True when `/cover/<hash>` has an image. The browser requests covers only for these.
    pub has_cover: bool,
    /// The marks the track carries live: "liked" for the default mark, then named marks.
    pub marks: Vec<String>,
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
            key: t.key.map(ApiKey::from),
            energy: t.energy.map(|e| e.value()),
            duration_secs: t.duration.map(|d| d.value()),
            added: t.added.clone(),
            has_cover: t.cover_art_path.is_some(),
            marks: t.marks.iter().map(Mark::to_string).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::bare_track as bare;
    use library_ipc_client::{Bpm, DurationSeconds};

    fn keys(v: &serde_json::Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    /// The JS contract (static/js/logic.js): exactly these keys, always all present.
    const KEYS: [&str; 13] = [
        "added",
        "album",
        "artist",
        "bpm",
        "disc",
        "duration_secs",
        "energy",
        "has_cover",
        "hash",
        "key",
        "marks",
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
        t.energy = Some(library_ipc_client::EnergyLevel::new(7).unwrap());
        t.track_number = Some(3);
        t.disc_number = Some(2);
        t.added = Some("2026-11-01T10:00:00+00:00".into());
        t.marks = vec![Mark::Default, "banger".parse().unwrap()];

        let json = serde_json::to_value(ApiTrack::from(&t)).unwrap();
        assert_eq!(keys(&json), KEYS);
        assert_eq!(json["hash"], "sha256:abc123");
        assert_eq!(json["title"], "Bloom");
        assert_eq!(json["track_number"], 3);
        assert_eq!(json["disc"], 2);
        assert_eq!(json["bpm"], 128.0);
        assert_eq!(json["energy"], 7);
        assert_eq!(json["duration_secs"], 180);
        assert_eq!(json["added"], "2026-11-01T10:00:00+00:00");
        assert_eq!(json["has_cover"], true);
        assert_eq!(json["marks"], json!(["liked", "banger"]));
    }

    #[test]
    fn key_carries_all_three_notations() {
        let mut t = bare("sha256:abc123");
        t.key = Some(Key::from_traditional("A Minor").unwrap());
        let json = serde_json::to_value(ApiTrack::from(&t)).unwrap();
        assert_eq!(
            json["key"],
            json!({
                "musical": "A Minor",
                "camelot": "8A",
                "open_key": "1m",
            })
        );
    }

    #[test]
    fn minimal_track_keeps_every_key_as_null_or_false() {
        let json = serde_json::to_value(ApiTrack::from(&bare("sha256:dead"))).unwrap();
        assert_eq!(keys(&json), KEYS);
        for k in KEYS
            .iter()
            .filter(|k| !["hash", "has_cover", "marks"].contains(k))
        {
            assert!(json[*k].is_null(), "{k} should be null");
        }
        assert_eq!(json["has_cover"], false);
        assert_eq!(json["marks"], json!([]));
    }

    #[test]
    fn query_takes_a_mark_and_rejects_a_bad_name() {
        let given = |mark: &str| {
            track_query_from(&SearchParams {
                mark: Some(mark.into()),
                ..Default::default()
            })
        };
        assert_eq!(
            given("banger").unwrap().mark,
            Some("banger".parse().unwrap())
        );
        assert_eq!(given("liked").unwrap().mark, Some(Mark::Default));
        assert_eq!(given(" ").unwrap().mark, None);
        assert!(given("a\u{200B}b").is_err());
        assert!(!given("banger").unwrap().is_empty());
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
            key: Some("Am+-1".into()),
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
            // The `~` suffix is gone from the key grammar: `+-1` already means the mixes-well set.
            SearchParams {
                key: Some("Am+-1~".into()),
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
    /// traditional key notation with every reach `keyReach` writes, and `>date`.
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
                for suffix in [
                    "", "+-1", "+-1d", "+-2", "+-3", "+-2d", "+-3d", ",+7", "+-1,+7", "+-3d,+7",
                ] {
                    let key = format!("{note}{mode}{suffix}");
                    let q = track_query_from(&param(None, Some(&key), None))
                        .unwrap_or_else(|e| panic!("key {key}: {e}"));
                    assert!(q.key.is_some(), "key {key}");
                }
            }
        }
        let q = track_query_from(&param(None, None, Some(">2026-10-01"))).unwrap();
        assert!(q.added.is_some());
        for energy in ["7", "1..10", "5..8", "1..6", "6..10", "7+-2"] {
            let q = track_query_from(&SearchParams {
                energy: Some(energy.into()),
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("energy {energy}: {e}"));
            assert!(q.energy.is_some(), "energy {energy}");
        }
    }

    #[test]
    fn energy_out_of_range_or_garbage_is_rejected() {
        for bad in ["0", "11", "0..5", "5..11", "loud", "7.5"] {
            let r = track_query_from(&SearchParams {
                energy: Some(bad.into()),
                ..Default::default()
            });
            assert!(r.is_err(), "energy {bad}");
        }
    }

    // ---- handlers, against a fake gateway ----

    use crate::test_support::{
        body_json, library_ok, playback_ok, send_get, test_app, test_app_with, FakeGateway,
    };
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{
        CanonicalHash, ContentHash, EntryError, PlaylistEntry, PlaylistName, PlaylistSnapshot,
        PlaylistVersion, ProtocolError,
    };
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
    async fn api_tracks_library_error_is_502() {
        // The gateway's "library unreachable" arrives as `Internal` (#100): a 502.
        let gw = FakeGateway::start(|_| GatewayResponse::Error {
            message: "down".into(),
        });
        let res = send_get(test_app_with(gw.addr()), "/api/tracks", None).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("down"));
    }

    #[tokio::test]
    async fn api_tracks_with_an_unreadable_library_answer_is_503() {
        let gw = FakeGateway::start_unreadable_when(|_| true, |_| playback_ok(None));
        let res = send_get(test_app_with(gw.addr()), "/api/tracks", None).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// A full canonical hash made of `c`.
    fn full(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn track_entry(c: char) -> PlaylistEntry {
        PlaylistEntry::Track {
            hash: CanonicalHash::try_from(ContentHash::new(full(c))).unwrap(),
        }
    }

    fn unresolved_entry(token: &str) -> PlaylistEntry {
        PlaylistEntry::Unresolved {
            error: EntryError::Unknown {
                hash: ContentHash::new(token),
            },
        }
    }

    /// A library with `playlists`, each answered as `PlaylistEntries` the way the real one does;
    /// any other playlist is not found.
    fn playlist_gateway(playlists: Vec<(&'static str, Vec<PlaylistEntry>)>) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistList,
            } => library_ok(LibraryResponse::PlaylistNames(
                playlists
                    .iter()
                    .map(|(name, _)| PlaylistName::new(name).unwrap())
                    .collect(),
            )),
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistEntries { name },
            } => library_ok(match playlists.iter().find(|(n, _)| *n == name.as_str()) {
                Some((_, entries)) => LibraryResponse::PlaylistEntries(entries.clone()),
                None => LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                    name: name.as_str().to_owned(),
                }),
            }),
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistSnapshot { name },
            } => library_ok(match playlists.iter().find(|(n, _)| *n == name.as_str()) {
                Some((n, entries)) => LibraryResponse::PlaylistSnapshot(PlaylistSnapshot {
                    version: PlaylistVersion::of(n.as_bytes()),
                    entries: entries.clone(),
                }),
                None => LibraryResponse::Error(ProtocolError::PlaylistNotFound {
                    name: name.as_str().to_owned(),
                }),
            }),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    /// Short-ID lines arrive resolved: the page gets full hashes, in file order, duplicates kept,
    /// over one gateway connection (a list request, then one entries request per playlist).
    #[tokio::test]
    async fn api_playlists_serves_full_hashes_for_short_lines() {
        let gw = playlist_gateway(vec![
            (
                "warmup",
                vec![
                    track_entry('a'),
                    track_entry('b'),
                    track_entry('a'),
                    track_entry('c'),
                ],
            ),
            ("peak", vec![track_entry('c')]),
        ]);
        let res = send_get(test_app_with(gw.addr()), "/api/playlists", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!([
                {"name": "warmup", "hashes": [full('a'), full('b'), full('a'), full('c')],
                 "unavailable": 0},
                {"name": "peak", "hashes": [full('c')], "unavailable": 0},
            ])
        );
        assert_eq!(gw.requests().len(), 3);
    }

    #[tokio::test]
    async fn api_playlists_counts_unavailable_entries() {
        let gw = playlist_gateway(vec![(
            "set",
            vec![
                track_entry('a'),
                unresolved_entry("0badc0de"),
                unresolved_entry("deadbeef"),
                track_entry('b'),
            ],
        )]);
        let res = send_get(test_app_with(gw.addr()), "/api/playlists", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!([{"name": "set", "hashes": [full('a'), full('b')], "unavailable": 2}])
        );
    }

    #[tokio::test]
    async fn api_playlist_keeps_unavailable_rows_in_place_and_duplicates() {
        let gw = playlist_gateway(vec![(
            "set",
            vec![
                track_entry('a'),
                unresolved_entry("0badc0de"),
                track_entry('b'),
                track_entry('a'),
            ],
        )]);
        let res = send_get(test_app_with(gw.addr()), "/api/playlists/set", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "name": "set",
                "version": PlaylistVersion::of(b"set").as_str(),
                "entries": [
                    {"status": "Track", "hash": full('a')},
                    {"status": "Unresolved", "token": "0badc0de",
                     "reason": "unknown track 0badc0de"},
                    {"status": "Track", "hash": full('b')},
                    {"status": "Track", "hash": full('a')},
                ],
            })
        );
    }

    #[tokio::test]
    async fn api_playlist_missing_is_404() {
        let gw = playlist_gateway(vec![("set", vec![track_entry('a')])]);
        let res = send_get(test_app_with(gw.addr()), "/api/playlists/nope", None).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(body_json(res).await["error"].is_string());
    }

    #[tokio::test]
    async fn api_playlist_from_a_library_that_refuses_is_502_and_never_falls_back() {
        // An old node does not know PlaylistSnapshot; there is no fallback to PlaylistEntries.
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::PlaylistSnapshot { .. },
            } => GatewayResponse::Error {
                message: "Invalid request: unknown variant `playlist_snapshot`".into(),
            },
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        let res = send_get(test_app_with(gw.addr()), "/api/playlists/set", None).await;
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(gw.requests().len(), 1);
    }

    #[tokio::test]
    async fn api_playlist_with_an_invalid_name_is_400_without_calling_the_library() {
        let gw = FakeGateway::start(|_| playback_ok(None));
        let res = send_get(test_app_with(gw.addr()), "/api/playlists/bad%20name", None).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"].is_string());
        assert!(gw.requests().is_empty());
    }

    #[tokio::test]
    async fn api_playlist_without_a_gateway_is_503() {
        let res = send_get(test_app(), "/api/playlists/set", None).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
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
    async fn match_forwards_the_energy_filter_and_rejects_a_bad_one() {
        let gw = FakeGateway::start(|req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::Search { query },
            } if query.energy.is_some() => {
                library_ok(LibraryResponse::SearchResults(vec![bare("sha256:calm")]))
            }
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        });
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/tracks/match?energy=5..8",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!(["sha256:calm"]));

        let before = gw.requests().len();
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/tracks/match?energy=11",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(gw.requests().len(), before);
    }

    #[tokio::test]
    async fn keys_lists_the_24_filter_keys_in_every_notation() {
        let gw = FakeGateway::start(|other| GatewayResponse::Error {
            message: format!("unexpected {other:?}"),
        });
        let res = send_get(test_app_with(gw.addr()), "/api/keys", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let keys = body_json(res).await;
        let keys = keys.as_array().unwrap();
        assert_eq!(keys.len(), 24);
        assert!(keys.contains(&json!({
            "musical": "A# Minor",
            "camelot": "3A",
            "open_key": "8m",
            "note": "A#",
            "mode": "min",
        })));
    }

    #[tokio::test]
    async fn plain_keys_are_cached_for_a_day_without_included() {
        let res = send_get(test_app(), "/api/keys", None).await;
        assert_eq!(
            res.headers()[axum::http::header::CACHE_CONTROL],
            "public, max-age=86400"
        );
        let keys = body_json(res).await;
        assert!(keys
            .as_array()
            .unwrap()
            .iter()
            .all(|k| k.get("included").is_none()));
    }

    #[tokio::test]
    async fn keys_come_minor_row_then_major_row_by_camelot_number() {
        let res = send_get(test_app(), "/api/keys", None).await;
        let keys = body_json(res).await;
        let camelot: Vec<&str> = keys
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k["camelot"].as_str().unwrap())
            .collect();
        let expected: Vec<String> = ["A", "B"]
            .iter()
            .flat_map(|l| (1..=12).map(move |n| format!("{n}{l}")))
            .collect();
        assert_eq!(camelot, expected);
    }

    #[tokio::test]
    async fn keys_with_a_reach_mark_the_included_ones() {
        // Straight at 2 from 8A: 6A 7A 9A 10A 8B, and the centre.
        let res = send_get(test_app(), "/api/keys?reach=8A%2B-2", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let keys = body_json(res).await;
        let mut included: Vec<&str> = keys
            .as_array()
            .unwrap()
            .iter()
            .filter(|k| k["included"] == json!(true))
            .map(|k| k["camelot"].as_str().unwrap())
            .collect();
        included.sort();
        assert_eq!(included, ["10A", "6A", "7A", "8A", "8B", "9A"]);
        assert_eq!(keys.as_array().unwrap().len(), 24);
        assert!(keys
            .as_array()
            .unwrap()
            .iter()
            .all(|k| k["included"].is_boolean()));
    }

    #[tokio::test]
    async fn keys_with_a_reach_are_not_cached_for_a_day() {
        let res = send_get(test_app(), "/api/keys?reach=8A%2B-2", None).await;
        assert_ne!(
            res.headers()[axum::http::header::CACHE_CONTROL],
            "public, max-age=86400"
        );
    }

    #[tokio::test]
    async fn keys_ignore_the_version_param() {
        let res = send_get(test_app(), "/api/keys?v=wheel", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()[axum::http::header::CACHE_CONTROL],
            "public, max-age=86400"
        );
    }

    #[tokio::test]
    async fn keys_with_a_bad_reach_is_400_with_the_parse_error() {
        let res = send_get(test_app(), "/api/keys?reach=8A%2B-2x", None).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("invalid key query"));
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
    async fn library_match_bad_filter_is_400_and_library_error_is_502() {
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
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
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
