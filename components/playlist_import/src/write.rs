//! Writing a playlist of matched tracks to the library.

use library_ipc_client::{
    ClientError, ContentHash, LibraryClient, PlaylistName, ProtocolError, TrackInfo,
};
use track_formatter::format_playlist_content;

/// Minimal seam for the playlist-write path.
///
/// Only the three operations used by [`write_playlist`] are required;
/// this keeps the test fake minimal while locking the canonical-format invariant.
pub trait PlaylistWriteOps {
    /// Resolve content hashes to canonical playlist content.
    fn resolve_and_format_playlist(&self, hashes: &[ContentHash]) -> Result<String, ClientError>;
    /// Create a new playlist with pre-formatted content. Fails if already exists.
    fn playlist_new(&self, name: &PlaylistName, content: String) -> Result<(), ClientError>;
    /// Replace an existing playlist with pre-formatted content.
    fn playlist_replace(&self, name: &PlaylistName, content: String) -> Result<(), ClientError>;
}

/// Resolve `hashes` with `get_track` and format them as canonical playlist content.
///
/// Fails on the first hash that cannot be resolved, so a transient error
/// never yields a partial playlist.
pub fn resolve_and_format_playlist(
    hashes: &[ContentHash],
    get_track: impl Fn(&ContentHash) -> Result<TrackInfo, ClientError>,
) -> Result<String, ClientError> {
    let tracks: Vec<TrackInfo> = hashes.iter().map(get_track).collect::<Result<_, _>>()?;
    Ok(format_playlist_content(&tracks))
}

impl PlaylistWriteOps for LibraryClient {
    fn resolve_and_format_playlist(&self, hashes: &[ContentHash]) -> Result<String, ClientError> {
        resolve_and_format_playlist(hashes, |h| self.get_track(h))
    }

    fn playlist_new(&self, name: &PlaylistName, content: String) -> Result<(), ClientError> {
        self.playlist_new(name, content)
    }

    fn playlist_replace(&self, name: &PlaylistName, content: String) -> Result<(), ClientError> {
        self.playlist_replace(name, content)
    }
}

/// Write a single named playlist via the [`PlaylistWriteOps`] seam.
///
/// Formats the hashes as canonical content, then calls `playlist_new`.
/// Falls back to `playlist_replace` only if the playlist already exists;
/// any other error is returned as-is.
/// Returns `Ok(true)` when created, `Ok(false)` when replaced.
pub fn write_playlist(
    backend: &dyn PlaylistWriteOps,
    name: &PlaylistName,
    hashes: &[ContentHash],
) -> Result<bool, ClientError> {
    let content = backend.resolve_and_format_playlist(hashes)?;
    match backend.playlist_new(name, content.clone()) {
        Ok(()) => Ok(true),
        Err(ClientError::Protocol(ProtocolError::PlaylistAlreadyExists { .. })) => {
            backend.playlist_replace(name, content)?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::DurationSeconds;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;

    fn make_rekordbox_track(
        hash: &str,
        artist: &str,
        title: &str,
        duration_secs: u32,
    ) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: None,
            duration: Some(DurationSeconds::new(duration_secs)),
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

    // ── write_playlist real regression tests ─────────────────────────────────
    //
    // These tests drive the actual `write_playlist` code path through a
    // `RecordingFake` that captures the `String` arguments passed to `playlist_new`
    // and `playlist_replace`.  The canonical-format assertion would fail if anyone
    // reverted the import code to pass raw `&[ContentHash]` instead of the
    // formatted content string.

    struct RecordingFake {
        /// Pre-canned TrackInfo entries the fake will "resolve" for any hash.
        tracks: Vec<TrackInfo>,
        /// `playlist_new` content arguments, in call order.
        new_calls: RefCell<Vec<String>>,
        /// `playlist_replace` content arguments, in call order.
        replace_calls: RefCell<Vec<String>>,
        /// When `true`, `playlist_new` returns `PlaylistAlreadyExists` so the
        /// code falls through to `playlist_replace`.
        force_conflict: bool,
        /// When `true`, `playlist_new` fails with an unrelated error that must
        /// NOT trigger the replace fallback.
        force_other_error: bool,
        /// When `true`, resolving hashes fails (e.g. a gateway hiccup).
        force_resolve_error: bool,
    }

    impl RecordingFake {
        fn new(tracks: Vec<TrackInfo>) -> Self {
            Self {
                tracks,
                new_calls: RefCell::new(Vec::new()),
                replace_calls: RefCell::new(Vec::new()),
                force_conflict: false,
                force_other_error: false,
                force_resolve_error: false,
            }
        }

        fn with_resolve_error(tracks: Vec<TrackInfo>) -> Self {
            Self {
                force_resolve_error: true,
                ..Self::new(tracks)
            }
        }

        fn with_other_error(tracks: Vec<TrackInfo>) -> Self {
            Self {
                force_other_error: true,
                ..Self::new(tracks)
            }
        }

        fn with_conflict(tracks: Vec<TrackInfo>) -> Self {
            Self {
                force_conflict: true,
                ..Self::new(tracks)
            }
        }
    }

    impl PlaylistWriteOps for RecordingFake {
        fn resolve_and_format_playlist(
            &self,
            _hashes: &[ContentHash],
        ) -> Result<String, ClientError> {
            if self.force_resolve_error {
                return Err(ClientError::Protocol(ProtocolError::Internal {
                    message: "resolve hiccup".to_string(),
                }));
            }
            // Produce canonical content from the pre-canned tracks (ignoring hashes
            // so no actual IPC is needed).
            Ok(format_playlist_content(&self.tracks))
        }

        fn playlist_new(&self, _name: &PlaylistName, content: String) -> Result<(), ClientError> {
            if self.force_conflict {
                return Err(ClientError::Protocol(
                    ProtocolError::PlaylistAlreadyExists {
                        name: "test".to_string(),
                    },
                ));
            }
            if self.force_other_error {
                return Err(ClientError::Protocol(ProtocolError::Internal {
                    message: "disk on fire".to_string(),
                }));
            }
            self.new_calls.borrow_mut().push(content);
            Ok(())
        }

        fn playlist_replace(
            &self,
            _name: &PlaylistName,
            content: String,
        ) -> Result<(), ClientError> {
            self.replace_calls.borrow_mut().push(content);
            Ok(())
        }
    }

    /// Assert every line of `content` is in canonical `{12hash}  {text}` form.
    fn assert_all_lines_canonical(content: &str, context: &str) {
        assert!(!content.is_empty(), "{context}: content must not be empty");
        for line in content.lines() {
            // Must not be a bare hash
            assert!(
                !line.starts_with("sha256:"),
                "{context}: line must not be a bare sha256: hash; got: {line:?}"
            );
            // Must start with exactly 12 lowercase hex chars followed by two spaces
            let prefix: String = line.chars().take(12).collect();
            assert!(
                prefix.chars().all(|c| c.is_ascii_hexdigit()),
                "{context}: line must start with 12 hex chars; got: {line:?}"
            );
            assert!(
                line.len() > 14 && &line[12..14] == "  ",
                "{context}: line must have two spaces after hash; got: {line:?}"
            );
        }
    }

    /// Happy path: `playlist_new` receives canonical content on the first import.
    #[test]
    fn rekordbox_import_playlist_new_produces_canonical_format() {
        let tracks = vec![
            make_rekordbox_track(
                "sha256:8d4e7b2cabc12345",
                "Carbon Based Lifeforms",
                "Polyrytmi",
                367,
            ),
            make_rekordbox_track("sha256:a3f91e0dabc12345", "Sunju Hargun", "Silverhaze", 421),
        ];
        let hashes: Vec<ContentHash> = tracks.iter().map(|t| t.content_hash.clone()).collect();

        let fake = RecordingFake::new(tracks);
        let name = PlaylistName::new("my-set").unwrap();

        let created = write_playlist(&fake, &name, &hashes).unwrap();
        assert!(created, "expected playlist_new to succeed (created=true)");

        let new_calls = fake.new_calls.borrow();
        assert_eq!(new_calls.len(), 1, "playlist_new called exactly once");
        assert_all_lines_canonical(&new_calls[0], "playlist_new content");

        // Verify exact canonical format for both tracks
        let lines: Vec<&str> = new_calls[0].lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "8d4e7b2cabc1  Carbon Based Lifeforms - Polyrytmi  [6:07]"
        );
        assert_eq!(lines[1], "a3f91e0dabc1  Sunju Hargun - Silverhaze  [7:01]");
    }

    /// Conflict path: `playlist_replace` receives canonical content when the
    /// playlist already exists (`playlist_new` returns `PlaylistAlreadyExists`).
    #[test]
    fn rekordbox_import_playlist_replace_conflict_produces_canonical_format() {
        let tracks = vec![make_rekordbox_track(
            "sha256:8d4e7b2cabc12345",
            "Carbon Based Lifeforms",
            "Polyrytmi",
            367,
        )];
        let hashes: Vec<ContentHash> = tracks.iter().map(|t| t.content_hash.clone()).collect();

        let fake = RecordingFake::with_conflict(tracks);
        let name = PlaylistName::new("my-set").unwrap();

        let created = write_playlist(&fake, &name, &hashes).unwrap();
        assert!(
            !created,
            "expected playlist_replace fallback (created=false)"
        );

        // playlist_new was NOT recorded (it returned an error before we pushed)
        assert!(
            fake.new_calls.borrow().is_empty(),
            "playlist_new should not record content when it returns an error"
        );

        let replace_calls = fake.replace_calls.borrow();
        assert_eq!(
            replace_calls.len(),
            1,
            "playlist_replace called exactly once"
        );
        assert_all_lines_canonical(&replace_calls[0], "playlist_replace content");

        let lines: Vec<&str> = replace_calls[0].lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            "8d4e7b2cabc1  Carbon Based Lifeforms - Polyrytmi  [6:07]"
        );
    }

    /// A resolve failure aborts before touching the playlist: neither
    /// `playlist_new` nor `playlist_replace` is called.
    #[test]
    fn write_playlist_resolve_failure_writes_nothing() {
        for fake in [
            RecordingFake::with_resolve_error(vec![]),
            RecordingFake {
                force_conflict: true,
                ..RecordingFake::with_resolve_error(vec![])
            },
        ] {
            let name = PlaylistName::new("my-set").unwrap();
            let err = write_playlist(&fake, &name, &[]).unwrap_err();
            assert!(err.to_string().contains("resolve hiccup"), "got: {err}");
            assert!(fake.new_calls.borrow().is_empty());
            assert!(fake.replace_calls.borrow().is_empty());
        }
    }

    /// Any `playlist_new` failure other than "already exists" is returned as-is
    /// and never replaces an existing playlist.
    #[test]
    fn write_playlist_does_not_replace_on_other_errors() {
        let fake = RecordingFake::with_other_error(vec![]);
        let name = PlaylistName::new("my-set").unwrap();

        let err = write_playlist(&fake, &name, &[]).unwrap_err();
        assert!(err.to_string().contains("disk on fire"), "got: {err}");
        assert!(fake.replace_calls.borrow().is_empty());
    }

    /// `resolve_and_format_playlist` stops at the first unresolvable hash.
    #[test]
    fn resolve_and_format_playlist_fails_on_first_unresolved_hash() {
        let hashes = [
            ContentHash::new("sha256:aaa"),
            ContentHash::new("sha256:bbb"),
        ];
        let err = resolve_and_format_playlist(&hashes, |h| {
            if h.as_str() == "sha256:bbb" {
                Err(ClientError::Protocol(ProtocolError::Internal {
                    message: "gone".to_string(),
                }))
            } else {
                Ok(make_rekordbox_track(h.as_str(), "A", "One", 60))
            }
        })
        .unwrap_err();
        assert!(err.to_string().contains("gone"), "got: {err}");
    }
}
