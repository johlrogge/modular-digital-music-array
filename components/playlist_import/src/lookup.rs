//! Library-backed [`TrackLookup`].

use library_ipc_client::{ClientError, ContentHash, TrackInfo};
use library_search::{StringQuery, TrackQuery};
use track_matcher::TrackLookup;

/// A library lookup failed; the import must stop before writing.
#[derive(Debug, thiserror::Error)]
#[error("Library lookup failed: {0}. Aborting before anything was written.")]
pub struct LookupError(String);

/// Remembers the first library lookup failure.
///
/// [`TrackLookup`] is infallible, so a failed search cannot be returned from
/// the lookup itself. Treating it as "not in library" would let an import
/// overwrite a good playlist with a partial one, so callers must check
/// [`LibraryTrackLookup::check`] after matching and abort.
#[derive(Default)]
struct LookupErrors(std::cell::RefCell<Option<String>>);

impl LookupErrors {
    fn record(&self, message: String) {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            *slot = Some(message);
        }
    }

    fn take(&self) -> Option<String> {
        self.0.borrow_mut().take()
    }
}

/// [`TrackLookup`] over a library search function.
///
/// `search` is the library's `search` call: `LibraryBackend::search` in the
/// CLI, `LibraryClient::search` in services.
pub struct LibraryTrackLookup<S> {
    search: S,
    errors: LookupErrors,
}

impl<S> LibraryTrackLookup<S>
where
    S: Fn(&TrackQuery) -> Result<Vec<TrackInfo>, ClientError>,
{
    pub fn new(search: S) -> Self {
        Self {
            search,
            errors: LookupErrors::default(),
        }
    }

    /// Fail if any lookup errored; the import must stop before writing.
    pub fn check(&self) -> Result<(), LookupError> {
        match self.errors.take() {
            None => Ok(()),
            Some(e) => Err(LookupError(e)),
        }
    }
}

impl<S> TrackLookup for LibraryTrackLookup<S>
where
    S: Fn(&TrackQuery) -> Result<Vec<TrackInfo>, ClientError>,
{
    fn find_by_isrc(&self, _isrc: &str) -> Vec<ContentHash> {
        // TODO: ISRC search is not available via TrackQuery — no ISRC field in library_search.
        // HasFact could be used but returns bool, not ContentHash.
        // For now, return empty and rely on artist+title matching.
        vec![]
    }

    fn find_by_artist_title(&self, artist: &str, title: &str) -> Vec<(ContentHash, Option<u32>)> {
        let query = TrackQuery {
            artist: Some(StringQuery::Contains(artist.to_string())),
            title: Some(StringQuery::Contains(title.to_string())),
            ..Default::default()
        };

        match (self.search)(&query) {
            Ok(tracks) => tracks
                .into_iter()
                .map(|t| {
                    let duration = t.duration.map(|d| d.value());
                    (t.content_hash, duration)
                })
                .collect(),
            Err(e) => {
                self.errors.record(e.to_string());
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::ProtocolError;
    use pretty_assertions::assert_eq;

    #[test]
    fn lookup_errors_are_recorded_for_the_caller() {
        let errors = LookupErrors::default();
        assert_eq!(errors.take(), None);
        errors.record("boom".to_string());
        errors.record("second".to_string());
        assert_eq!(errors.take(), Some("boom".to_string()));
        assert_eq!(errors.take(), None);
    }

    #[test]
    fn failed_search_is_not_treated_as_not_in_library() {
        let lookup = LibraryTrackLookup::new(|_: &TrackQuery| {
            Err(ClientError::Protocol(ProtocolError::Internal {
                message: "gateway hiccup".to_string(),
            }))
        });
        assert!(lookup.find_by_artist_title("a", "b").is_empty());
        let err = lookup.check().unwrap_err();
        assert!(err.to_string().contains("gateway hiccup"), "got: {err}");
        assert!(lookup.check().is_ok(), "check consumes the recorded error");
    }
}
