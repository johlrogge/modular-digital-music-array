//! Library wire types for the new web UI.
//!
//! `ApiTrack` is the one track shape the browser sees. Its field names are exactly the ones
//! `static/js/logic.js` reads; the old UI keeps using `TrackInfoJson` and is not affected.

use library_ipc_client::TrackInfo;
use serde::Serialize;

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
    use library_ipc_client::{Bpm, ContentHash, DurationSeconds, Key};

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
}
