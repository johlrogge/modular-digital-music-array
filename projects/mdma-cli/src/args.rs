//! Argument helpers shared by several command modules.

use crate::stdin::is_hash_token;
use clap::CommandFactory;
use library_ipc_client::{ContentHash, PlaylistName};
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

/// Optional content hash of a track; commands read stdin when it is omitted.
// Prefer `Option<TrackArg>` in clap fields; this goes away when the call sites switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptTrackArg(pub Option<ContentHash>);

/// Name of an MDMA playlist, validated by `PlaylistName`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistArg(pub PlaylistName);

impl FromStr for PlaylistArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PlaylistName::new(s).map(Self).map_err(|e| e.to_string())
    }
}

/// Optional name of an MDMA playlist (clap fields should use `Option<PlaylistArg>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptPlaylistArg(pub Option<PlaylistName>);

/// A playlist name for an imported playlist: sanitised on parse. Keeps the
/// raw text so commands can note when it was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // wired in P2.1.C / P2.1.F
pub(crate) struct SanitizedPlaylistNameArg {
    pub(crate) raw: String,
    pub(crate) sanitized: String,
}

impl FromStr for SanitizedPlaylistNameArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sanitized = sanitize_playlist_name(s);
        if sanitized.is_empty() {
            return Err(format!("'{s}': name sanitizes to empty"));
        }
        Ok(Self {
            raw: s.to_string(),
            sanitized,
        })
    }
}

/// Path of a file that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // wired in P2.1.F
pub(crate) struct ExistingFileArg(pub PathBuf);

impl FromStr for ExistingFileArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(s);
        if path.is_file() {
            Ok(Self(path))
        } else if path.exists() {
            Err(format!("'{s}' is not a file"))
        } else {
            Err(format!("file '{s}' does not exist"))
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

pub(crate) fn parse_playlist_name(name: &str) -> library_ipc_client::PlaylistName {
    library_ipc_client::PlaylistName::new(name).unwrap_or_else(|e| {
        eprintln!("Invalid playlist name: {}", e);
        std::process::exit(1);
    })
}

/// Sanitise a user-supplied playlist name for `flag`, exiting if nothing is left.
/// Prints a note when the name was changed.
pub(crate) fn sanitized_name_or_exit(raw: &str, flag: &str) -> String {
    let sanitized = sanitize_playlist_name(raw);
    if sanitized.is_empty() {
        eprintln!("Invalid {} '{}': name sanitizes to empty", flag, raw);
        std::process::exit(1);
    }
    if sanitized != raw {
        eprintln!(
            "Note: playlist name sanitized from '{}' to '{}'",
            raw, sanitized
        );
    }
    sanitized
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
            "my-list_1".parse::<PlaylistArg>(),
            Ok(PlaylistArg(PlaylistName::new("my-list_1").unwrap()))
        );
        assert!("".parse::<PlaylistArg>().is_err());
        assert!("has space".parse::<PlaylistArg>().is_err());
        assert!("a/b".parse::<PlaylistArg>().is_err());
    }

    #[test]
    fn sanitized_name_keeps_raw_and_sanitized() {
        let arg: SanitizedPlaylistNameArg = "Good Name".parse().unwrap();
        assert_eq!(arg.raw, "Good Name");
        assert_eq!(arg.sanitized, sanitize_playlist_name("Good Name"));
        let plain: SanitizedPlaylistNameArg = "plain".parse().unwrap();
        assert_eq!(plain.raw, plain.sanitized);
    }

    #[test]
    fn sanitized_name_rejects_empty_result() {
        assert!("".parse::<SanitizedPlaylistNameArg>().is_err());
        let all_bad = "///";
        if sanitize_playlist_name(all_bad).is_empty() {
            assert!(all_bad.parse::<SanitizedPlaylistNameArg>().is_err());
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
