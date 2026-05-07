//! Canonical track line formatter shared between CLI and TUI.
//!
//! Produces the standard `{8-char-hash}  {Artist} - {Title}  [{duration}]`
//! format used for playlist files and pipe-mode output.

use library_ipc_protocol::{ContentHash, TrackInfo};

/// Extract the first 8 characters of a hash, stripping any `sha256:` prefix.
pub fn short_hash(hash: &ContentHash) -> &str {
    let clean = hash
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(hash.as_str());
    if clean.len() >= 8 {
        &clean[..8]
    } else {
        clean
    }
}

/// Format a single track as the canonical playlist line.
///
/// Format: `{8-char-hash}  {Artist} - {Title}  [{duration}]`
///
/// If duration is absent the trailing `  [{duration}]` segment is omitted.
/// Artist and Title fall back to `"Unknown"` when absent.
pub fn format_track_line(track: &TrackInfo) -> String {
    let title = track.title.as_deref().unwrap_or("Unknown");
    let artist = track.artist.as_deref().unwrap_or("Unknown");
    let hash = short_hash(&track.content_hash);
    match track.duration {
        Some(d) => format!("{}  {} - {}  [{}]", hash, artist, title, d),
        None => format!("{}  {} - {}", hash, artist, title),
    }
}

/// Build playlist file content from a slice of track info.
///
/// Each track produces one canonical line. Lines are joined with `\n` and
/// a trailing newline is appended.
///
/// The hash used in each line is taken from `track.content_hash` — there is
/// no separate hash slice, making it structurally impossible to produce a
/// bare-hash line via this function.
pub fn format_playlist_content(tracks: &[TrackInfo]) -> String {
    let mut out = String::new();
    for track in tracks {
        out.push_str(&format_track_line(track));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_protocol::{ContentHash, TrackInfo};

    fn make_track(
        hash: &str,
        artist: Option<&str>,
        title: Option<&str>,
        duration: Option<u32>,
    ) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: None,
            duration: duration.map(|s| library_ipc_protocol::DurationSeconds::new(s)),
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        }
    }

    #[test]
    fn short_hash_strips_prefix_and_truncates() {
        let h = ContentHash::new("sha256:abcdef1234567890");
        assert_eq!(short_hash(&h), "abcdef12");
    }

    #[test]
    fn short_hash_no_prefix_truncates() {
        let h = ContentHash::new("abcdef1234567890");
        assert_eq!(short_hash(&h), "abcdef12");
    }

    #[test]
    fn format_track_line_with_duration() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("Artist Name"),
            Some("Track Title"),
            Some(225), // 3:45
        );
        let line = format_track_line(&track);
        assert_eq!(line, "8d4e7b2c  Artist Name - Track Title  [3:45]");
    }

    #[test]
    fn format_track_line_without_duration() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("Artist Name"),
            Some("Track Title"),
            None,
        );
        let line = format_track_line(&track);
        assert_eq!(line, "8d4e7b2c  Artist Name - Track Title");
    }

    #[test]
    fn format_track_line_missing_metadata_falls_back_to_unknown() {
        let track = make_track("sha256:8d4e7b2cabc12345", None, None, None);
        let line = format_track_line(&track);
        assert_eq!(line, "8d4e7b2c  Unknown - Unknown");
    }

    #[test]
    fn format_track_line_matches_canonical_regex() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("Artist Name"),
            Some("Track Title"),
            Some(225),
        );
        let line = format_track_line(&track);
        // Regex: ^[0-9a-f]{8}\s{2}.+\s-\s.+\s{2}\[\d{1,2}:\d{2}(:\d{2})?\]$
        assert!(
            line.starts_with("8d4e7b2c  "),
            "must start with 8-char hash + 2 spaces"
        );
        assert!(line.contains(" - "), "must contain artist-title separator");
        assert!(line.ends_with(']'), "must end with closing bracket");
    }

    #[test]
    fn format_playlist_content_builds_newline_terminated_lines() {
        let tracks = vec![
            make_track(
                "sha256:8d4e7b2cabc12345",
                Some("Artist One"),
                Some("Title One"),
                Some(225),
            ),
            make_track(
                "sha256:a3f91e0dabc12345",
                Some("Artist Two"),
                Some("Title Two"),
                Some(252),
            ),
        ];
        let content = format_playlist_content(&tracks);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "8d4e7b2c  Artist One - Title One  [3:45]");
        assert_eq!(lines[1], "a3f91e0d  Artist Two - Title Two  [4:12]");
    }
}
