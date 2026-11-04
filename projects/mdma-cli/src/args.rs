//! Argument helpers shared by several command modules.

use crate::stdin::is_hash_token;
use clap::CommandFactory;
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

/// Name of an MDMA playlist, validated by `PlaylistName`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistArg(pub PlaylistName);

impl FromStr for PlaylistArg {
    type Err = PlaylistNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PlaylistName::new(s).map(Self)
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
    use pretty_assertions::assert_eq;

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
