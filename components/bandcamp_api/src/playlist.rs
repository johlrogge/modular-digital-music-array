//! Bandcamp fan playlists: types and pure parsers

use crate::error::BandcampError;
use crate::html::extract_data_blob;
use serde::{Deserialize, Serialize};

/// Summary of a playlist as returned by the fan playlist listing
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub slug: String,
    pub track_count: u32,
    pub is_private: bool,
}

/// A track within a playlist
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistTrack {
    /// 1-based position in playlist order
    pub position: u32,
    pub track_id: u64,
    pub title: String,
    pub artist: String,
    pub album_title: Option<String>,
    pub album_id: Option<u64>,
    pub duration_secs: f64,
    pub is_owned: bool,
    pub url: String,
}

/// A fully fetched playlist
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub is_private: bool,
    pub total_count: u32,
    pub tracks: Vec<PlaylistTrack>,
}

/// One chunk of tracks plus the cursor for the next chunk.
/// Track positions are 0 until assembled by [`assemble_playlist`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TracksChunk {
    pub tracks: Vec<PlaylistTrack>,
    pub total_count: u32,
    pub next_cursor: Option<u64>,
}

/// The first chunk of a playlist, as embedded in the playlist page
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlaylistPage {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub is_private: bool,
    pub chunk: TracksChunk,
}

/// One page of the playlist listing
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlaylistsPage {
    pub playlists: Vec<PlaylistSummary>,
    pub next_cursor: Option<u64>,
}

// --- wire formats (unknown fields ignored) ---

#[derive(Deserialize)]
struct WirePageBlob {
    #[serde(rename = "appData")]
    app_data: WireAppData,
}

#[derive(Deserialize)]
struct WireAppData {
    #[serde(rename = "playlistId")]
    playlist_id: u64,
    title: String,
    #[serde(rename = "isPrivate", default)]
    is_private: Option<bool>,
    tracklist: WireTracklist,
}

#[derive(Deserialize)]
struct WireMoreTracks {
    tracklist: WireTracklist,
}

#[derive(Deserialize)]
struct WireTracklist {
    #[serde(rename = "itemUrl", default)]
    item_url: Option<String>,
    tracks: Vec<WireTrack>,
    #[serde(rename = "tracksSummary")]
    tracks_summary: WireSummary,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<u64>,
}

#[derive(Deserialize)]
struct WireSummary {
    #[serde(rename = "totalCount")]
    total_count: u32,
}

#[derive(Deserialize)]
struct WireTrack {
    id: u64,
    // Nullable on the wire for odd items (preorders, removed tracks)
    #[serde(default)]
    title: Option<String>,
    #[serde(rename = "artistName", default)]
    artist_name: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(rename = "isOwned", default)]
    is_owned: Option<bool>,
    #[serde(default)]
    url: Option<String>,
    album: Option<WireAlbum>,
}

#[derive(Deserialize)]
struct WireAlbum {
    id: Option<u64>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct WirePlaylists {
    items: Vec<WirePlaylistItem>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<u64>,
}

#[derive(Deserialize)]
struct WirePlaylistItem {
    #[serde(rename = "itemId")]
    item_id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(rename = "isPrivate", default)]
    is_private: Option<bool>,
    #[serde(rename = "itemUrl", default)]
    item_url: Option<String>,
    #[serde(rename = "tracksSummary")]
    tracks_summary: WireSummary,
}

impl From<WireTrack> for PlaylistTrack {
    fn from(t: WireTrack) -> Self {
        let (album_id, album_title) = match t.album {
            Some(a) => (a.id, a.title),
            None => (None, None),
        };
        PlaylistTrack {
            position: 0,
            track_id: t.id,
            title: t.title.unwrap_or_default(),
            artist: t.artist_name.unwrap_or_default(),
            album_title,
            album_id,
            duration_secs: t.duration.unwrap_or_default(),
            is_owned: t.is_owned.unwrap_or(false),
            url: t.url.unwrap_or_default(),
        }
    }
}

impl From<WireTracklist> for TracksChunk {
    fn from(t: WireTracklist) -> Self {
        TracksChunk {
            tracks: t.tracks.into_iter().map(Into::into).collect(),
            total_count: t.tracks_summary.total_count,
            next_cursor: t.next_cursor,
        }
    }
}

fn json_err(what: &str, e: serde_json::Error) -> BandcampError {
    BandcampError::HtmlParse(format!("Failed to parse {}: {}", what, e))
}

/// Parse a playlist page: the first chunk of tracks plus the next cursor.
pub(crate) fn parse_playlist_page(html: &str) -> Result<PlaylistPage, BandcampError> {
    let blob = extract_data_blob(html, "PlaylistPage")?;
    let parsed: WirePageBlob =
        serde_json::from_str(&blob).map_err(|e| json_err("playlist blob", e))?;
    let app = parsed.app_data;
    let url = app.tracklist.item_url.clone().unwrap_or_default();
    Ok(PlaylistPage {
        id: app.playlist_id,
        title: app.title,
        url,
        is_private: app.is_private.unwrap_or(false),
        chunk: app.tracklist.into(),
    })
}

/// Parse a `player_data_web` response (additional playlist tracks).
pub(crate) fn parse_more_tracks(json: &str) -> Result<TracksChunk, BandcampError> {
    let parsed: WireMoreTracks =
        serde_json::from_str(json).map_err(|e| json_err("more-tracks response", e))?;
    Ok(parsed.tracklist.into())
}

/// Parse a `fan_collection/1/playlists` response.
pub(crate) fn parse_playlists_response(json: &str) -> Result<PlaylistsPage, BandcampError> {
    let parsed: WirePlaylists =
        serde_json::from_str(json).map_err(|e| json_err("playlists response", e))?;
    Ok(PlaylistsPage {
        playlists: parsed
            .items
            .into_iter()
            .map(|i| {
                let url = i.item_url.unwrap_or_default();
                PlaylistSummary {
                    id: i.item_id,
                    title: i.title.unwrap_or_default(),
                    slug: slug_from_url(&url),
                    url,
                    track_count: i.tracks_summary.total_count,
                    is_private: i.is_private.unwrap_or(false),
                }
            })
            .collect(),
        next_cursor: parsed.next_cursor,
    })
}

/// Last path segment of a playlist URL (query and fragment stripped).
pub(crate) fn slug_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Combine the first page and any further chunks into a playlist,
/// numbering positions 1..=N in order. Warns if the count does not match
/// the reported total.
pub(crate) fn assemble_playlist(
    page: PlaylistPage,
    more: impl IntoIterator<Item = TracksChunk>,
) -> Playlist {
    let total_count = page.chunk.total_count;
    let mut tracks = page.chunk.tracks;
    for chunk in more {
        tracks.extend(chunk.tracks);
    }
    for (i, track) in tracks.iter_mut().enumerate() {
        track.position = (i + 1) as u32;
    }
    if tracks.len() != total_count as usize {
        tracing::warn!(
            "Playlist '{}': fetched {} tracks but Bandcamp reports {}",
            page.title,
            tracks.len(),
            total_count
        );
    }
    Playlist {
        id: page.id,
        title: page.title,
        url: page.url,
        is_private: page.is_private,
        total_count,
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const PAGE: &str = include_str!("../tests/fixtures/playlist_page.html");
    const BLOB: &str = include_str!("../tests/fixtures/playlist_page_blob.json");
    const MORE: &str = include_str!("../tests/fixtures/playlist_more_tracks_response.json");
    const LISTING: &str = include_str!("../tests/fixtures/playlists_listing_response.json");

    #[test]
    fn parses_playlist_page() {
        let page = parse_playlist_page(PAGE).unwrap();
        assert_eq!(page.id, 982341);
        assert_eq!(page.title, "november");
        assert!(page.is_private);
        assert_eq!(
            page.url,
            "https://bandcamp.com/testuser/playlist/november-2"
        );
        assert_eq!(page.chunk.total_count, 64);
        assert_eq!(page.chunk.next_cursor, Some(49));
        assert_eq!(page.chunk.tracks.len(), 5);

        let first = &page.chunk.tracks[0];
        assert_eq!(first.track_id, 1150569772);
        assert_eq!(first.title, "Obsidian");
        assert_eq!(first.artist, "Sun Drine, Ben Jarli, Dit:eau, Tricky Jones");
        assert_eq!(first.album_title.as_deref(), Some("Obsidian"));
        assert_eq!(first.album_id, Some(784675455));
        assert_eq!(first.duration_secs, 545.088);
        assert!(first.is_owned);
        assert_eq!(first.url, "https://lokd.bandcamp.com/track/obsidian");
    }

    #[test]
    fn page_blob_is_html_unescaped() {
        let blob = extract_data_blob(PAGE, "PlaylistPage").unwrap();
        assert!(!blob.contains("&quot;"));
        let decoded: serde_json::Value = serde_json::from_str(&blob).unwrap();
        let expected: serde_json::Value = serde_json::from_str(BLOB).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn missing_playlist_page_element_errors() {
        let err = parse_playlist_page("<html><body></body></html>").unwrap_err();
        assert!(matches!(err, BandcampError::HtmlParse(_)));
        assert!(err.to_string().contains("#PlaylistPage"));
    }

    #[test]
    fn parses_more_tracks() {
        let chunk = parse_more_tracks(MORE).unwrap();
        assert_eq!(chunk.tracks.len(), 3);
        assert_eq!(chunk.total_count, 64);
        assert_eq!(chunk.next_cursor, None);
        assert_eq!(chunk.tracks[2].title, "Weekend");
        assert_eq!(chunk.tracks[2].album_id, Some(2246268064));
    }

    #[test]
    fn parses_playlists_listing() {
        let page = parse_playlists_response(LISTING).unwrap();
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.playlists.len(), 3);
        assert_eq!(
            page.playlists[1],
            PlaylistSummary {
                id: 982341,
                title: "november".to_string(),
                url: "https://bandcamp.com/testuser/playlist/november-2".to_string(),
                slug: "november-2".to_string(),
                track_count: 64,
                is_private: true,
            }
        );
        assert!(!page.playlists[0].is_private);
    }

    #[test]
    fn null_track_fields_do_not_fail_parse() {
        let json = r#"{"tracklist": {
            "tracks": [
                {"id": 1, "title": null, "artistName": null, "duration": null,
                 "url": null, "album": {"id": null, "title": null}},
                {"id": 2, "title": "Ok", "artistName": "A", "duration": 100.5}
            ],
            "tracksSummary": {"totalCount": 2},
            "nextCursor": null
        }}"#;
        let chunk = parse_more_tracks(json).unwrap();
        assert_eq!(chunk.tracks.len(), 2);
        assert_eq!(chunk.tracks[0].title, "");
        assert_eq!(chunk.tracks[0].artist, "");
        assert_eq!(chunk.tracks[0].duration_secs, 0.0);
        assert_eq!(chunk.tracks[0].album_title, None);
        assert_eq!(chunk.tracks[1].title, "Ok");
        assert_eq!(chunk.tracks[1].duration_secs, 100.5);
    }

    #[test]
    fn null_booleans_default_to_false() {
        let json = r#"{"tracklist": {
            "tracks": [{"id": 1, "title": "T", "isOwned": null}],
            "tracksSummary": {"totalCount": 1},
            "nextCursor": null
        }}"#;
        let chunk = parse_more_tracks(json).unwrap();
        assert!(!chunk.tracks[0].is_owned);

        let listing = r#"{"items": [
            {"itemId": 5, "title": "x", "itemUrl": "https://bandcamp.com/u/playlist/x",
             "isPrivate": null, "tracksSummary": {"totalCount": 0}}
        ], "nextCursor": null}"#;
        let page = parse_playlists_response(listing).unwrap();
        assert!(!page.playlists[0].is_private);
    }

    #[test]
    fn null_listing_fields_do_not_fail_parse() {
        let json = r#"{"items": [
            {"itemId": 5, "title": null, "itemUrl": null,
             "tracksSummary": {"totalCount": 0}}
        ], "nextCursor": null}"#;
        let page = parse_playlists_response(json).unwrap();
        assert_eq!(page.playlists.len(), 1);
        assert_eq!(page.playlists[0].title, "");
        assert_eq!(page.playlists[0].slug, "");
    }

    #[test]
    fn slug_extraction() {
        assert_eq!(
            slug_from_url("https://bandcamp.com/testuser/playlist/november-2"),
            "november-2"
        );
        assert_eq!(
            slug_from_url("https://bandcamp.com/testuser/playlist/a-b/?x=1#y"),
            "a-b"
        );
    }

    #[test]
    fn positions_numbered_across_page_and_more() {
        let page = parse_playlist_page(PAGE).unwrap();
        let more = parse_more_tracks(MORE).unwrap();
        let playlist = assemble_playlist(page, [more]);
        assert_eq!(playlist.tracks.len(), 8);
        assert_eq!(playlist.total_count, 64);
        let positions: Vec<u32> = playlist.tracks.iter().map(|t| t.position).collect();
        assert_eq!(positions, (1..=8).collect::<Vec<u32>>());
        assert_eq!(
            playlist.tracks[4].title,
            "B2 Alexskyspirit - Rvanegon (Balanca Hybrid Remix)"
        );
        assert_eq!(playlist.tracks[5].title, "Alseids (Original mix)");
        assert_eq!(playlist.id, 982341);
    }
}
