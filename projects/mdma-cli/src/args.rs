//! Argument helpers shared by several command modules.

use crate::stdin::is_hash_token;
use hicli::Arg;
use library_ipc_client::{ContentHash, PlaylistName, PlaylistNameError};
use playlist_import::sanitize_playlist_name;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;

// The errors of the types below are bare phrases that read after the argument's
// name (hicli prints ``couldn't parse `v`: `--flag`: <phrase>``), and never echo
// the value.

/// Show what would happen without making changes
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Arg)]
#[arg(long = "dry-run")]
pub(crate) struct DryRun(pub bool);

/// Return as soon as the job has started
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Arg)]
#[arg(long = "no-wait")]
pub(crate) struct NoWait(pub bool);

/// Carry out the changes it finds, not only list them
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Arg)]
#[arg(long = "apply")]
pub(crate) struct Apply(pub bool);

/// Directory to write to (created if it doesn't exist)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "output", metavar = "OUTPUT", complete = dirs)]
pub(crate) struct OutputDir(pub Option<PathBuf>);

/// A whole number of at least 1 (`--limit 0` would show nothing).
pub(crate) fn parse_count(s: &str) -> Result<NonZeroUsize, &'static str> {
    s.parse::<usize>()
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or("must be a whole number of at least 1")
}

/// Content hash (full or partial, with or without sha256: prefix)
///
/// Follows the stdin rule: at least 8 lowercase hex characters, optional `sha256:`
/// prefix. A prefix is accepted; the service resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "HASH")]
pub(crate) struct TrackArg(pub ContentHash);

impl FromStr for TrackArg {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if is_hash_token(s) {
            Ok(Self(ContentHash::new(s)))
        } else {
            Err("not a content hash: expected at least 8 lowercase hex characters, optionally prefixed with `sha256:`")
        }
    }
}

/// Playlist name (only a-z, A-Z, 0-9, `_` and `-` are allowed)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "NAME")]
pub(crate) struct PlaylistArg(pub PlaylistName);

/// Why `PlaylistName` refused a name, as a phrase.
fn playlist_name_reason(error: &PlaylistNameError) -> &'static str {
    match error {
        PlaylistNameError::Empty => "playlist name cannot be empty",
        PlaylistNameError::InvalidCharacters { .. } => {
            "playlist name contains invalid characters (only a-z, A-Z, 0-9, _, - allowed)"
        }
    }
}

impl FromStr for PlaylistArg {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PlaylistName::new(s)
            .map(Self)
            .map_err(|e| playlist_name_reason(&e))
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
    pub(crate) fn new(raw: &str) -> Result<Self, &'static str> {
        let sanitized = sanitize_playlist_name(raw);
        if sanitized.is_empty() {
            return Err("name sanitizes to empty");
        }
        let name = PlaylistName::new(&sanitized).map_err(|e| playlist_name_reason(&e))?;
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
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Path to an existing file
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "PATH", complete = files)]
pub(crate) struct ExistingFileArg(pub PathBuf);

impl FromStr for ExistingFileArg {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(PathBuf::from(s))
    }
}

impl ExistingFileArg {
    /// Check that `path` is an existing file.
    pub(crate) fn new(path: PathBuf) -> Result<Self, &'static str> {
        if path.is_file() {
            Ok(Self(path))
        } else if path.exists() {
            Err("not a file")
        } else {
            Err("file does not exist")
        }
    }
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
        assert!(err.contains("not a content hash"), "{err}");
        assert!(run_arg::<TrackArg>(&[]).is_err());
    }

    #[test]
    fn playlist_arg_parsers_validate_the_name() {
        let name = PlaylistName::new("my-list").unwrap();
        assert_eq!(run_arg::<PlaylistArg>(&["my-list"]), Ok(PlaylistArg(name)));
        assert!(run_arg::<PlaylistArg>(&["a/b"]).is_err());
        assert!(run_arg::<PlaylistArg>(&[]).is_err());
    }

    /// The text of a failed parse of `value` by `T`, after bpaf's own prefix for
    /// the value and the argument's name.
    fn after_the_prefix<T: hicli::Arg + std::fmt::Debug>(args: &[&str], value: &str) -> String {
        let err = run_arg::<T>(args).unwrap_err();
        let (_, phrase) = err
            .split_once(&format!("couldn't parse `{value}`: "))
            .unwrap_or_else(|| panic!("no bpaf prefix in {err:?}"));
        phrase.to_string()
    }

    #[test]
    fn parse_errors_are_bare_phrases_that_do_not_echo_the_value() {
        let value = "xyz-has space";
        for phrase in [
            after_the_prefix::<TrackArg>(&[value], value),
            after_the_prefix::<PlaylistArg>(&[value], value),
            after_the_prefix::<ExistingFileArg>(&[value], value),
        ] {
            assert!(!phrase.contains(value), "{phrase}");
            // `` `HASH`: `` / `` `NAME`: `` names the argument, then the phrase.
            assert!(phrase.starts_with('`'), "{phrase}");
        }
        assert_eq!(
            after_the_prefix::<ExistingFileArg>(&["/definitely/not/here"], "/definitely/not/here"),
            "`PATH`: file does not exist"
        );
    }

    #[test]
    fn a_count_is_a_whole_number_of_at_least_one() {
        assert_eq!(parse_count("3"), Ok(NonZeroUsize::new(3).unwrap()));
        for bad in ["0", "-1", "x", "1.5", ""] {
            assert_eq!(
                parse_count(bad),
                Err("must be a whole number of at least 1"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn apply_and_output_dir_are_options() {
        assert_eq!(run_arg::<Apply>(&[]), Ok(Apply(false)));
        assert_eq!(run_arg::<Apply>(&["--apply"]), Ok(Apply(true)));
        assert!(run_arg::<Apply>(&["--appply"]).is_err());
        assert_eq!(run_arg::<OutputDir>(&[]), Ok(OutputDir(None)));
        assert_eq!(
            run_arg::<OutputDir>(&["--output", "out"]),
            Ok(OutputDir(Some(PathBuf::from("out"))))
        );
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
    fn a_bad_positional_names_its_metavar() {
        let err = run_arg::<TrackArg>(&["xyz"]).unwrap_err();
        assert!(err.contains("`HASH`"), "{err}");
        let err = run_arg::<ExistingFileArg>(&["/definitely/not/here.mp3"]).unwrap_err();
        assert!(err.contains("`PATH`"), "{err}");
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
        assert_eq!(
            "".parse::<SanitizedPlaylistName>(),
            Err("name sanitizes to empty")
        );
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
