//! What `mdma playback preview` takes and says (ADR-031).
//!
//! The hold itself (what is audible, where, whether it played, and going back
//! to it) lives in playback; this module parses the command line, resolves the
//! hash to a full one through the library (playback holds only full hashes,
//! ADR-026) and says what happened. The command is in the `playback` group.

use bpaf::Parser;
use color_eyre::eyre::eyre;
use color_eyre::Result;
use hicli::Arg;
use mdma_client::{Deck, LibraryBackend, NodeError, PlaybackBackend};
use track_formatter::short_hash;

use crate::args::TrackArg;
use crate::output::display_error;
use crate::queue::resolve_for_playback;

/// What to do: preview a track, or go back. Exactly one of the two, decided
/// at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreviewTarget {
    /// `HASH`: play it over what is audible, holding that to go back to.
    Track(TrackArg),
    /// `--back`: end the preview and restore what it held.
    Back,
}

impl Arg for PreviewTarget {
    /// `HASH` or `--back`.
    fn parser() -> impl Parser<Self> {
        let track = <TrackArg as Arg>::parser().map(Self::Track);
        let back = bpaf::long("back")
            .help("Go back to what was playing before the preview")
            .req_flag(Self::Back);
        bpaf::construct!([track, back])
    }
}

/// Do it on deck A and say what happened. Going back never asks the library.
pub(crate) fn run(
    playback: &PlaybackBackend,
    library: impl FnOnce() -> Result<LibraryBackend, NodeError>,
    target: PreviewTarget,
) -> Result<String> {
    match target {
        PreviewTarget::Track(TrackArg(hash)) => {
            let library = library().map_err(display_error)?;
            let full = resolve_for_playback(&library, std::slice::from_ref(&hash))?
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("the library did not resolve {}", hash.as_str()))?;
            let said = format!("Previewing {}", short_hash(&hash));
            playback
                .preview(Deck::A, full.into())
                .map_err(display_error)?;
            Ok(said)
        }
        PreviewTarget::Back => {
            let said = match playback.end_preview(Deck::A).map_err(display_error)? {
                Some(resumed) => format!("Back to {}", short_hash(&resumed)),
                None => "Back (nothing was playing)".to_string(),
            };
            Ok(said)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{parses, recorded, usage_error, FakeServices, Setup};
    use mdma_client::{Command, ContentHash};
    use pretty_assertions::assert_eq;

    const A: &str = "sha256:9fb4105e0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5";

    #[test]
    fn the_command_line_takes_a_hash_or_back_under_playback() {
        let path = ["playback", "preview"];
        assert_eq!(parses(&["playback", "preview", "9fb4105e"]), path);
        assert_eq!(parses(&["playback", "preview", "--back"]), path);
    }

    #[test]
    fn the_command_line_needs_one_of_the_two_and_not_both() {
        usage_error(&["playback", "preview"]);
        usage_error(&["playback", "preview", "9fb4105e", "--back"]);
    }

    #[test]
    fn there_is_no_top_level_preview() {
        usage_error(&["preview", "9fb4105e"]);
    }

    #[test]
    fn a_hash_is_resolved_through_the_library_and_previewed_on_deck_a() {
        let fake = FakeServices::new(&[A]);

        let said = run(
            &fake.playback,
            || Ok(fake.library.clone()),
            PreviewTarget::Track(TrackArg(ContentHash::new("9fb4105e"))),
        )
        .unwrap();

        assert_eq!(
            fake.sent(),
            vec![recorded(&Command::Preview {
                deck: Deck::A,
                hash: ContentHash::new(A),
            })]
        );
        assert_eq!(
            said,
            format!("Previewing {}", short_hash(&ContentHash::new("9fb4105e")))
        );
    }

    #[test]
    fn an_unknown_hash_is_refused_and_nothing_is_sent() {
        let fake = FakeServices::new(&[A]);

        let err = run(
            &fake.playback,
            || Ok(fake.library.clone()),
            PreviewTarget::Track(TrackArg(ContentHash::new("deadbeef"))),
        )
        .unwrap_err();

        assert!(err.to_string().contains("deadbeef"), "{err}");
        assert_eq!(fake.sent(), Vec::<serde_json::Value>::new());
    }

    #[test]
    fn back_ends_the_preview_says_where_it_went_and_never_asks_the_library() {
        let fake = FakeServices::start(Setup {
            resumed: Some(ContentHash::new(A)),
            ..Setup::default()
        });

        let said = run(
            &fake.playback,
            || panic!("going back needs no library"),
            PreviewTarget::Back,
        )
        .unwrap();

        assert_eq!(
            said,
            format!("Back to {}", short_hash(&ContentHash::new(A)))
        );
        assert_eq!(
            fake.sent(),
            vec![recorded(&Command::EndPreview { deck: Deck::A })]
        );
    }

    #[test]
    fn back_over_nothing_says_nothing_was_playing() {
        let fake = FakeServices::new(&[]);

        let said = run(
            &fake.playback,
            || panic!("going back needs no library"),
            PreviewTarget::Back,
        )
        .unwrap();

        assert_eq!(said, "Back (nothing was playing)");
    }
}
