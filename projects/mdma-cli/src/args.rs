//! Argument helpers shared by several command modules.

use crate::stdin::is_hash_token;
use bpaf::{positional, Parser};
use clap::CommandFactory;
use hicli::{complete_files, Arg};
use library_ipc_client::{ContentHash, PlaylistName, PlaylistNameError};
use playlist_import::sanitize_playlist_name;
use std::path::PathBuf;
use std::str::FromStr;

/// Content hash of a track, as typed (a prefix is accepted; the service resolves it).
/// Follows the stdin rule: at least 8 lowercase hex characters, optional `sha256:` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackArg(pub ContentHash);

impl FromStr for TrackArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if is_hash_token(s) {
            Ok(Self(ContentHash::new(s)))
        } else {
            Err(format!(
                "'{s}' is not a content hash (expected at least 8 lowercase hex characters, optionally prefixed with 'sha256:')"
            ))
        }
    }
}

// The Arg impls spell the arguments as clap does today (metavar = the clap
// field name uppercased, same help). A type used under other names (`winner`,
// `from`, ...) is a field of a multi-value args struct instead.
impl Arg for TrackArg {
    fn parser() -> impl Parser<Self> {
        positional::<Self>("HASH")
            .help("Content hash (full or partial, with or without sha256: prefix)")
    }
}

/// An optional [`TrackArg`] (none: the command reads hashes from stdin or has a default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptTrackArg(pub Option<TrackArg>);

impl Arg for OptTrackArg {
    fn parser() -> impl Parser<Self> {
        positional::<TrackArg>("HASH")
            .help("Content hash (full or partial). Omit to read from stdin.")
            .optional()
            .map(Self)
    }
}

/// Name of an MDMA playlist, validated by `PlaylistName`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistArg(pub PlaylistName);

impl FromStr for PlaylistArg {
    type Err = PlaylistNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PlaylistName::new(s).map(Self)
    }
}

impl Arg for PlaylistArg {
    fn parser() -> impl Parser<Self> {
        positional::<Self>("NAME").help("Playlist name")
    }
}

/// An optional [`PlaylistArg`] (none: the command reads names from stdin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptPlaylistArg(pub Option<PlaylistArg>);

impl Arg for OptPlaylistArg {
    fn parser() -> impl Parser<Self> {
        positional::<PlaylistArg>("NAME")
            .help("Playlist name. Omit to read names from stdin.")
            .optional()
            .map(Self)
    }
}

/// A playlist name for an imported playlist: sanitised on parse. Keeps the
/// raw text so commands can note when it was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SanitizedPlaylistName {
    raw: String,
    name: PlaylistName,
}

impl SanitizedPlaylistName {
    /// Sanitise `raw` into a playlist name. The error is the reason only
    /// (callers add context), so argv and XML names share this one path.
    pub(crate) fn new(raw: &str) -> Result<Self, String> {
        let sanitized = sanitize_playlist_name(raw);
        if sanitized.is_empty() {
            return Err("name sanitizes to empty".to_string());
        }
        let name = PlaylistName::new(&sanitized).map_err(|e| e.to_string())?;
        Ok(Self {
            raw: raw.to_string(),
            name,
        })
    }

    /// The sanitised name.
    pub(crate) fn name(&self) -> &PlaylistName {
        &self.name
    }

    /// A note for the user when sanitising changed the text.
    pub(crate) fn change_note(&self) -> Option<String> {
        (self.name.as_str() != self.raw).then(|| {
            format!(
                "Note: playlist name sanitized from '{}' to '{}'",
                self.raw,
                self.name.as_str()
            )
        })
    }
}

impl FromStr for SanitizedPlaylistName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s).map_err(|reason| format!("'{s}': {reason}"))
    }
}

/// Path of a file that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingFileArg(pub PathBuf);

impl FromStr for ExistingFileArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(PathBuf::from(s))
    }
}

impl Arg for ExistingFileArg {
    fn parser() -> impl Parser<Self> {
        complete_files(positional::<Self>("PATH").help("Path to rekordbox.xml file"))
    }
}

impl ExistingFileArg {
    /// Check that `path` is an existing file.
    pub(crate) fn new(path: PathBuf) -> Result<Self, String> {
        if path.is_file() {
            Ok(Self(path))
        } else if path.exists() {
            Err(format!("'{}' is not a file", path.display()))
        } else {
            Err(format!("file '{}' does not exist", path.display()))
        }
    }
}

/// Report a cross-argument usage error the way clap does (message, usage, exit 2).
pub(crate) fn usage_error(
    kind: clap::error::ErrorKind,
    msg: impl std::fmt::Display,
) -> clap::Error {
    crate::Cli::command().error(kind, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_arg;
    use pretty_assertions::assert_eq;

    #[test]
    fn track_arg_parser_reads_a_positional_hash() {
        assert_eq!(
            run_arg::<TrackArg>(&["abcd1234"]),
            Ok(TrackArg(ContentHash::new("abcd1234")))
        );
        let err = run_arg::<TrackArg>(&["xyz"]).unwrap_err();
        assert!(err.contains("is not a content hash"), "{err}");
        assert!(run_arg::<TrackArg>(&[]).is_err());
    }

    #[test]
    fn opt_track_arg_parser_may_be_omitted() {
        assert_eq!(run_arg::<OptTrackArg>(&[]), Ok(OptTrackArg(None)));
        assert_eq!(
            run_arg::<OptTrackArg>(&["abcd1234"]),
            Ok(OptTrackArg(Some(TrackArg(ContentHash::new("abcd1234")))))
        );
        let err = run_arg::<OptTrackArg>(&["xyz"]).unwrap_err();
        assert!(err.contains("is not a content hash"), "{err}");
    }

    #[test]
    fn playlist_arg_parsers_validate_the_name() {
        let name = PlaylistName::new("my-list").unwrap();
        assert_eq!(
            run_arg::<PlaylistArg>(&["my-list"]),
            Ok(PlaylistArg(name.clone()))
        );
        assert_eq!(
            run_arg::<OptPlaylistArg>(&["my-list"]),
            Ok(OptPlaylistArg(Some(PlaylistArg(name))))
        );
        assert_eq!(run_arg::<OptPlaylistArg>(&[]), Ok(OptPlaylistArg(None)));
        let reason = PlaylistName::new("has space").unwrap_err().to_string();
        let err = run_arg::<PlaylistArg>(&["has space"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        assert!(run_arg::<OptPlaylistArg>(&["a/b"]).is_err());
        assert!(run_arg::<PlaylistArg>(&[]).is_err());
    }

    #[test]
    fn existing_file_arg_parser_requires_an_existing_file() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        assert_eq!(
            run_arg::<ExistingFileArg>(&[manifest]),
            Ok(ExistingFileArg(PathBuf::from(manifest)))
        );
        let err = run_arg::<ExistingFileArg>(&["/definitely/not/here.mp3"]).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn track_arg_accepts_short_and_prefixed_hashes() {
        assert_eq!(
            "abcd1234".parse::<TrackArg>(),
            Ok(TrackArg(ContentHash::new("abcd1234")))
        );
        assert_eq!(
            "sha256:abcd1234".parse::<TrackArg>(),
            Ok(TrackArg(ContentHash::new("sha256:abcd1234")))
        );
    }

    #[test]
    fn track_arg_rejects_bad_hashes() {
        for bad in [
            "abcd123",
            "ABCD1234",
            "sha256:abc",
            "ghijklmn",
            "",
            "abcd 1234",
        ] {
            assert!(bad.parse::<TrackArg>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn playlist_arg_validates_through_playlist_name() {
        assert_eq!(
            "my-list_1".parse::<PlaylistArg>().unwrap(),
            PlaylistArg(PlaylistName::new("my-list_1").unwrap())
        );
        assert!("".parse::<PlaylistArg>().is_err());
        assert!("has space".parse::<PlaylistArg>().is_err());
        assert!("a/b".parse::<PlaylistArg>().is_err());
    }

    #[test]
    fn sanitized_name_keeps_raw_and_sanitized() {
        let arg: SanitizedPlaylistName = "Good Name".parse().unwrap();
        assert_eq!(arg.name().as_str(), sanitize_playlist_name("Good Name"));
        assert_eq!(
            arg.change_note().as_deref(),
            Some(
                format!(
                    "Note: playlist name sanitized from 'Good Name' to '{}'",
                    arg.name().as_str()
                )
                .as_str()
            )
        );
        let plain: SanitizedPlaylistName = "plain".parse().unwrap();
        assert_eq!(plain.name().as_str(), "plain");
        assert_eq!(plain.change_note(), None);
    }

    #[test]
    fn sanitized_name_rejects_empty_result() {
        assert!("".parse::<SanitizedPlaylistName>().is_err());
        let all_bad = "///";
        if sanitize_playlist_name(all_bad).is_empty() {
            assert!(all_bad.parse::<SanitizedPlaylistName>().is_err());
        }
    }

    #[test]
    fn existing_file_arg_requires_an_existing_file() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        assert_eq!(
            manifest.parse::<ExistingFileArg>(),
            Ok(ExistingFileArg(PathBuf::from(manifest)))
        );
        assert!(env!("CARGO_MANIFEST_DIR")
            .parse::<ExistingFileArg>()
            .is_err());
        assert!("/definitely/not/here.mp3"
            .parse::<ExistingFileArg>()
            .is_err());
    }
}
