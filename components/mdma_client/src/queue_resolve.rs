//! Resolving hashes against the playback queue.
//!
//! Adding to the queue resolves against the library's visible tracks
//! ([`crate::LibraryBackend::resolve_for_playback`]). Removing resolves against
//! what the queue holds, so an entry whose track has left the library can still
//! be named, even by a prefix. Both use the library's own prefix matcher,
//! [`library_projection::resolve_prefix`].

use library_ipc_client::{
    CanonicalHash, ClientError, ContentHash, NotCanonical, ProtocolError, ResolveError,
};
use library_projection::{resolve_prefix, HashedTrack, Unresolved};

/// A queue entry as a record the library's prefix matcher can resolve against.
/// It has no title, and a queue entry is never hidden.
pub(crate) struct QueueEntry<'a>(&'a ContentHash);

impl HashedTrack for QueueEntry<'_> {
    fn content_hash(&self) -> &str {
        self.0.as_str()
    }

    fn title(&self) -> Option<&str> {
        None
    }

    fn is_hidden(&self) -> bool {
        false
    }
}

pub(crate) fn queue_entries(queue: &[ContentHash]) -> Vec<QueueEntry<'_>> {
    queue.iter().map(QueueEntry).collect()
}

/// The canonical hash of the one entry of `entries` that `hash` names.
pub(crate) fn resolve_queue_entry(
    entries: &[QueueEntry<'_>],
    hash: &ContentHash,
) -> Result<CanonicalHash, ResolveError> {
    let entry = resolve_prefix(entries, hash.as_str()).map_err(|unresolved| match unresolved {
        Unresolved::NotFound => ResolveError::NotInQueue { hash: hash.clone() },
        Unresolved::Ambiguous(_) => ResolveError::AmbiguousInQueue { hash: hash.clone() },
    })?;
    CanonicalHash::try_from(entry.0.clone()).map_err(|NotCanonical(returned)| {
        ResolveError::NotCanonical {
            hash: hash.clone(),
            returned,
        }
    })
}

/// The canonical hash of each of `hashes`, resolved against the entries of the
/// playback `queue` (as `QueueList` returns them), for removing. A hash that
/// names no entry, or several, fails the batch naming it.
pub fn resolve_in_queue(
    queue: &[ContentHash],
    hashes: &[ContentHash],
) -> Result<Vec<CanonicalHash>, ResolveError> {
    let entries = queue_entries(queue);
    hashes
        .iter()
        .map(|hash| resolve_queue_entry(&entries, hash))
        .collect()
}

/// How resolving one hash went against one set of tracks (the queue, or the
/// library).
pub(crate) enum Outcome {
    /// The set names one track.
    Found(CanonicalHash),
    /// The set has no such track, which says nothing against the other set. Holds
    /// the error to give if no set has it.
    Absent(ResolveError),
    /// The set could not say: ambiguous, not full, or the library failed.
    Refused(ResolveError),
}

impl Outcome {
    /// `result` as an outcome: an error `is_absent` accepts means the set has no
    /// such track; any other error refuses.
    pub(crate) fn classify(
        result: Result<CanonicalHash, ResolveError>,
        is_absent: impl FnOnce(&ResolveError) -> bool,
    ) -> Self {
        match result {
            Ok(found) => Outcome::Found(found),
            Err(e) if is_absent(&e) => Outcome::Absent(e),
            Err(e) => Outcome::Refused(e),
        }
    }

    /// The outcome of resolving against the queue: only `NotInQueue` is absent.
    pub(crate) fn of_queue(result: Result<CanonicalHash, ResolveError>) -> Self {
        Self::classify(result, |e| matches!(e, ResolveError::NotInQueue { .. }))
    }

    /// The outcome of resolving against the library: only `TrackNotFound` is
    /// absent (a hidden track is not found). Every other failure refuses.
    pub(crate) fn of_library(result: Result<CanonicalHash, ResolveError>) -> Self {
        Self::classify(result, |e| {
            matches!(
                e,
                ResolveError::Lookup {
                    source: ClientError::Protocol(ProtocolError::TrackNotFound { .. }),
                    ..
                }
            )
        })
    }
}

/// The one answer for `hash` from what the queue and the library each made of it.
pub(crate) fn combine(
    hash: &ContentHash,
    queue: Outcome,
    library: Outcome,
) -> Result<CanonicalHash, ResolveError> {
    match (queue, library) {
        (Outcome::Refused(e), _) | (_, Outcome::Refused(e)) => Err(e),
        (Outcome::Found(queued), Outcome::Found(library)) if queued == library => Ok(queued),
        (Outcome::Found(queued), Outcome::Found(library)) => {
            Err(ResolveError::AmbiguousAcrossQueueAndLibrary {
                hash: hash.clone(),
                queued,
                library,
            })
        }
        (Outcome::Found(found), Outcome::Absent(_))
        | (Outcome::Absent(_), Outcome::Found(found)) => Ok(found),
        (Outcome::Absent(_), Outcome::Absent(not_found)) => Err(not_found),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(tag: &str) -> ContentHash {
        ContentHash::new(format!("sha256:{}", tag.repeat(64)))
    }

    #[test]
    fn a_prefix_resolves_to_the_queue_entry_it_starts() {
        let queue = [full("a"), full("b")];
        let got = resolve_in_queue(&queue, &[ContentHash::new("bbbb")]).unwrap();
        assert_eq!(got, vec![full("b")]);
    }

    #[test]
    fn a_full_hash_resolves_to_itself_when_queued() {
        let queue = [full("a")];
        let got = resolve_in_queue(&queue, &[full("a")]).unwrap();
        assert_eq!(got, vec![full("a")]);
    }

    #[test]
    fn a_full_hash_that_is_not_queued_is_refused() {
        let queue = [full("a")];
        let err = resolve_in_queue(&queue, &[full("c")]).unwrap_err();
        assert!(matches!(err, ResolveError::NotInQueue { .. }), "{err}");
        assert_eq!(err.hash(), &full("c"));
    }

    #[test]
    fn a_prefix_of_different_entries_is_ambiguous() {
        let queue = [
            ContentHash::new(format!("sha256:ab{}", "0".repeat(62))),
            ContentHash::new(format!("sha256:ab{}", "1".repeat(62))),
        ];
        let err = resolve_in_queue(&queue, &[ContentHash::new("ab")]).unwrap_err();
        assert!(
            matches!(err, ResolveError::AmbiguousInQueue { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_track_queued_twice_is_not_ambiguous() {
        let queue = [full("a"), full("a")];
        let got = resolve_in_queue(&queue, &[ContentHash::new("aaaa")]).unwrap();
        assert_eq!(got, vec![full("a")]);
    }

    fn asked() -> ContentHash {
        ContentHash::new("aaaa")
    }

    fn not_in_queue() -> ResolveError {
        ResolveError::NotInQueue { hash: asked() }
    }

    fn library_not_found() -> ResolveError {
        ResolveError::Lookup {
            hash: asked(),
            source: ClientError::Protocol(ProtocolError::TrackNotFound {
                hash: "aaaa".to_string(),
            }),
        }
    }

    fn library_failed() -> ResolveError {
        ResolveError::Lookup {
            hash: asked(),
            source: ClientError::Protocol(ProtocolError::Internal {
                message: "boom".to_string(),
            }),
        }
    }

    fn canonical(tag: &str) -> CanonicalHash {
        CanonicalHash::try_from(full(tag)).unwrap()
    }

    fn combined(queue: Outcome, library: Outcome) -> Result<CanonicalHash, ResolveError> {
        combine(&asked(), queue, library)
    }

    #[test]
    fn both_sets_finding_the_same_track_resolve_to_it() {
        let got = combined(
            Outcome::Found(canonical("a")),
            Outcome::Found(canonical("a")),
        );
        assert_eq!(got.unwrap(), canonical("a"));
    }

    #[test]
    fn different_tracks_in_the_queue_and_the_library_are_ambiguous() {
        let err = combined(
            Outcome::Found(canonical("a")),
            Outcome::Found(canonical("b")),
        )
        .unwrap_err();
        match err {
            ResolveError::AmbiguousAcrossQueueAndLibrary {
                hash,
                queued,
                library,
            } => {
                assert_eq!(hash, asked());
                assert_eq!(queued, canonical("a"));
                assert_eq!(library, canonical("b"));
            }
            other => panic!("expected AmbiguousAcrossQueueAndLibrary, got {other}"),
        }
    }

    #[test]
    fn a_track_only_the_queue_has_resolves_to_the_queue_entry() {
        let got = combined(
            Outcome::Found(canonical("a")),
            Outcome::Absent(library_not_found()),
        );
        assert_eq!(got.unwrap(), canonical("a"));
    }

    #[test]
    fn a_track_only_the_library_has_resolves_to_the_library_track() {
        let got = combined(
            Outcome::Absent(not_in_queue()),
            Outcome::Found(canonical("b")),
        );
        assert_eq!(got.unwrap(), canonical("b"));
    }

    #[test]
    fn a_hash_neither_set_has_gives_the_librarys_not_found() {
        let err = combined(
            Outcome::Absent(not_in_queue()),
            Outcome::Absent(library_not_found()),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::Lookup {
                    source: ClientError::Protocol(ProtocolError::TrackNotFound { .. }),
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn a_refusal_by_either_set_is_the_answer() {
        let queue_refused = combined(
            Outcome::Refused(ResolveError::AmbiguousInQueue { hash: asked() }),
            Outcome::Found(canonical("a")),
        )
        .unwrap_err();
        assert!(matches!(
            queue_refused,
            ResolveError::AmbiguousInQueue { .. }
        ));

        let library_refused = combined(
            Outcome::Found(canonical("a")),
            Outcome::Refused(library_failed()),
        )
        .unwrap_err();
        assert!(
            matches!(
                library_refused,
                ResolveError::Lookup {
                    source: ClientError::Protocol(ProtocolError::Internal { .. }),
                    ..
                }
            ),
            "{library_refused}"
        );

        let absent_queue = combined(
            Outcome::Absent(not_in_queue()),
            Outcome::Refused(library_failed()),
        );
        assert!(absent_queue.is_err());
    }

    #[test]
    fn a_stale_queue_entry_the_library_lacks_resolves_to_the_queue_entry() {
        let queue = [full("a")];
        let entries = queue_entries(&queue);
        let queue_side =
            Outcome::of_queue(resolve_queue_entry(&entries, &ContentHash::new("aaaa")));
        let library_side = Outcome::of_library(Err(library_not_found()));
        let got = combine(&ContentHash::new("aaaa"), queue_side, library_side);
        assert_eq!(got.unwrap(), canonical("a"));
    }

    #[test]
    fn only_not_in_queue_is_absent_from_the_queue() {
        assert!(matches!(
            Outcome::of_queue(Err(not_in_queue())),
            Outcome::Absent(_)
        ));
        assert!(matches!(
            Outcome::of_queue(Err(ResolveError::AmbiguousInQueue { hash: asked() })),
            Outcome::Refused(_)
        ));
    }

    #[test]
    fn only_track_not_found_is_absent_from_the_library() {
        assert!(matches!(
            Outcome::of_library(Err(library_not_found())),
            Outcome::Absent(_)
        ));
        assert!(matches!(
            Outcome::of_library(Err(library_failed())),
            Outcome::Refused(_)
        ));
        let ambiguous = ResolveError::Lookup {
            hash: asked(),
            source: ClientError::Protocol(ProtocolError::AmbiguousHash {
                hash: "aaaa".to_string(),
                candidates: vec![],
            }),
        };
        assert!(matches!(
            Outcome::of_library(Err(ambiguous)),
            Outcome::Refused(_)
        ));
    }

    #[test]
    fn the_first_unresolvable_hash_fails_the_batch_naming_it() {
        let queue = [full("a")];
        let err = resolve_in_queue(&queue, &[full("a"), ContentHash::new("dead"), full("e")])
            .unwrap_err();
        assert_eq!(err.hash(), &ContentHash::new("dead"));
    }
}
