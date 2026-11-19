//! The rows the playlist and queue panes list: a resolved track, or a line
//! the library could not resolve. Both kinds live in one vec so positions stay
//! aligned with the stored entries, and nothing is ever dropped.

use crate::link::ReadError;
use crate::track_list::{ListedRow, Listing};
use mdma_client::{ContentHash, EntryError, TrackInfo};
use track_formatter::{format_track_line, unavailable_line};

/// One listed line: a resolved track, or a line the library could not resolve.
#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "Rows are mostly tracks, so boxing them would add an allocation per row to save space on the rare unresolved one."
)]
pub(crate) enum ListRow {
    Track(TrackInfo),
    Unresolved(EntryError),
}

impl ListRow {
    /// The hash this row stands for: the track's, or the stored token of an
    /// unresolved line.
    pub(crate) fn hash(&self) -> ContentHash {
        match self {
            ListRow::Track(track) => track.content_hash.clone(),
            ListRow::Unresolved(error) => error.hash().clone(),
        }
    }

    /// The line this row is saved as. Unresolved lines are never dropped:
    /// they are written back as their own token with the reason after it.
    fn save_line(&self) -> String {
        match self {
            ListRow::Track(track) => format_track_line(track),
            ListRow::Unresolved(error) => unavailable_line(error.hash(), error),
        }
    }

    /// The text the filter matches on.
    pub(crate) fn display_text(&self) -> String {
        match self.listing() {
            Listing::Track(track) => format!(
                "{} {} {}",
                track.artist.as_deref().unwrap_or(""),
                track.title.as_deref().unwrap_or(""),
                track.album.as_deref().unwrap_or("")
            ),
            Listing::Unavailable(text) => text,
        }
    }
}

impl ListedRow for ListRow {
    fn listing(&self) -> Listing<'_> {
        match self {
            ListRow::Track(track) => Listing::Track(track),
            ListRow::Unresolved(error) => Listing::Unavailable(format!("unavailable: {error}")),
        }
    }
}

/// Build the content to save for `rows`, one line per row, in order.
pub(crate) fn rows_content_for_write(rows: &[ListRow]) -> String {
    rows.iter()
        .map(|row| {
            let mut line = row.save_line();
            line.push('\n');
            line
        })
        .collect()
}

/// The row for the answer to looking up `hash`. The one place that decides
/// which lookup failures are an answer about the hash: "track not found"
/// becomes an unresolved row. Any other failure (transport, ambiguous hash,
/// internal error) is returned, so nothing is dropped and nothing is guessed.
pub(crate) fn row_for_lookup(
    hash: &ContentHash,
    lookup: Result<TrackInfo, ReadError>,
) -> Result<ListRow, ReadError> {
    match lookup {
        Ok(track) => Ok(ListRow::Track(track)),
        Err(error) if error.is_track_not_found() => {
            tracing::warn!("Facts have no track for {}", hash.as_str());
            Ok(ListRow::Unresolved(EntryError::Unknown {
                hash: hash.clone(),
            }))
        }
        Err(other) => Err(other),
    }
}

/// Turn hashes into rows, in order, with the facts' own message when a
/// lookup fails for any reason `row_for_lookup` does not turn into a row.
pub(crate) fn rows_for_hashes(
    hashes: &[ContentHash],
    mut get_track: impl FnMut(&ContentHash) -> Result<TrackInfo, ReadError>,
) -> Result<Vec<ListRow>, ReadError> {
    hashes
        .iter()
        .map(|hash| row_for_lookup(hash, get_track(hash)))
        .collect()
}

#[cfg(test)]
pub(crate) fn make_track_info(
    hash: &str,
    artist: &str,
    title: &str,
    duration_secs: u32,
) -> TrackInfo {
    use library_ipc_protocol::DurationSeconds;
    TrackInfo {
        content_hash: ContentHash::new(hash),
        title: Some(title.to_string()),
        artist: Some(artist.to_string()),
        album: None,
        duration: Some(DurationSeconds::new(duration_secs)),
        duration_ms: None,
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
        genre: None,
        label: None,
        styles: vec![],
        marks: vec![],
    }
}

#[cfg(test)]
pub(crate) fn not_found(h: &ContentHash) -> ReadError {
    use mdma_client::ProtocolError;
    ReadError::Facts(
        ProtocolError::TrackNotFound {
            hash: h.as_str().to_string(),
        }
        .into(),
    )
}

#[cfg(test)]
pub(crate) fn transport_error() -> ReadError {
    use mdma_client::LibraryClientError;
    ReadError::Facts(LibraryClientError::from(serde_json::from_str::<u8>("x").unwrap_err()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    fn unknown(token: &str) -> ListRow {
        ListRow::Unresolved(EntryError::Unknown { hash: hash(token) })
    }

    fn ambiguous(token: &str, candidates: &[&str]) -> ListRow {
        ListRow::Unresolved(EntryError::Ambiguous {
            hash: hash(token),
            candidates: candidates.iter().map(|c| hash(c)).collect(),
        })
    }

    /// The TUI playlist write path must produce canonical formatted lines, not bare hashes.
    #[test]
    fn write_produces_canonical_formatted_lines() {
        let rows: Vec<ListRow> = vec![
            make_track_info("sha256:8d4e7b2cabc12345", "Artist Name", "Track Title", 225),
            make_track_info(
                "sha256:a3f91e0dabc12345",
                "Other Artist",
                "Other Title",
                252,
            ),
        ]
        .into_iter()
        .map(ListRow::Track)
        .collect();

        let content = rows_content_for_write(&rows);
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(
            lines,
            vec![
                "8d4e7b2cabc1  Artist Name - Track Title  [3:45]",
                "a3f91e0dabc1  Other Artist - Other Title  [4:12]",
            ]
        );
    }

    #[test]
    fn rows_for_hashes_keeps_order_between_tracks_and_unresolved() {
        let hashes = [hash("sha256:aaa"), hash("dead"), hash("sha256:bbb")];

        let rows = rows_for_hashes(&hashes, |h| {
            if h.as_str() == "dead" {
                Err(not_found(h))
            } else {
                Ok(make_track_info(h.as_str(), "A", "T", 60))
            }
        })
        .unwrap();

        let got: Vec<ContentHash> = rows.iter().map(ListRow::hash).collect();
        assert_eq!(got, hashes.to_vec());
        assert!(matches!(rows[1], ListRow::Unresolved(_)));
    }

    #[test]
    fn rows_for_hashes_aborts_on_transport_error() {
        let result = rows_for_hashes(&[hash("sha256:aaa")], |_| Err(transport_error()));
        assert!(matches!(
            result,
            Err(ReadError::Facts(client_facts::FactsError::Transport(_)))
        ));
    }

    /// Only "track not found" is an answer about the hash. Any other library
    /// error (ambiguous hash, internal, old library) aborts with its message.
    #[test]
    fn rows_for_hashes_aborts_on_other_library_errors() {
        let result = rows_for_hashes(&[hash("sha256:aaa")], |_| {
            Err(ReadError::Facts(
                mdma_client::ProtocolError::Internal {
                    message: "ambiguous hash sha256:aaa".to_string(),
                }
                .into(),
            ))
        });

        let err = result.err().expect("an Internal error must abort");
        assert!(err.to_string().contains("ambiguous hash sha256:aaa"));
    }

    #[test]
    fn unresolved_row_is_listed_as_unavailable_and_filterable() {
        let row = unknown("dead");
        assert!(matches!(row.listing(), Listing::Unavailable(t) if t.contains("dead")));
        assert!(row.display_text().contains("dead"));
    }

    #[test]
    fn save_writes_unknown_rows_as_their_token_with_the_reason() {
        let content = rows_content_for_write(&[unknown("deadbeef")]);
        assert_eq!(content, "deadbeef  # unknown track deadbeef\n");
    }

    #[test]
    fn save_writes_ambiguous_rows_as_their_token_with_all_candidates() {
        let content = rows_content_for_write(&[ambiguous("ab", &["sha256:ab11", "sha256:ab22"])]);
        assert_eq!(
            content,
            "ab  # ambiguous hash ab: sha256:ab11 sha256:ab22\n"
        );
    }

    #[test]
    fn unresolved_rows_survive_save_and_reload() {
        let rows = vec![
            ListRow::Track(make_track_info("sha256:8d4e7b2cabc12345", "A", "One", 60)),
            unknown("dead"),
            ambiguous("ab", &["sha256:ab11", "sha256:ab22"]),
            ListRow::Track(make_track_info("sha256:a3f91e0dabc12345", "B", "Two", 61)),
        ];

        let content = rows_content_for_write(&rows);
        let reloaded: Vec<&str> = library_ipc_protocol::playlist_hashes(&content).collect();

        assert_eq!(reloaded, vec!["8d4e7b2cabc1", "dead", "ab", "a3f91e0dabc1"]);
    }

    #[test]
    fn save_keeps_resolved_lines_in_order() {
        let rows = vec![
            ListRow::Track(make_track_info("sha256:8d4e7b2cabc12345", "A", "One", 60)),
            unknown("dead"),
            ListRow::Track(make_track_info("sha256:a3f91e0dabc12345", "B", "Two", 61)),
        ];

        let content = rows_content_for_write(&rows);
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("8d4e7b2cabc1  A - One"));
        assert_eq!(lines[1], "dead  # unknown track dead");
        assert!(lines[2].starts_with("a3f91e0dabc1  B - Two"));
    }
}
