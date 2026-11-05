//! Argument helpers shared by several command modules.

use crate::stdin::is_hash_token;
use bpaf::{positional, Parser};
use hicli::{complete_files, complete_with, Arg};
use library_ipc_client::{ContentHash, PlaylistName, PlaylistNameError};
use playlist_import::sanitize_playlist_name;
use std::path::PathBuf;
use std::str::FromStr;

/// The one place an `Arg` impl for a plain argument type is spelled out. The forms:
///
/// - `T, positional "META", "help"`: a positional that parses through `FromStr`.
/// - `T, long "x" [, short 'c'], "META", "help"`: an option with a value.
/// - `T, switch "x" [, short 'c'], "help"`: a switch on a `T(bool)`.
/// - `OptT, optional T`: `OptT(Option<T>)`, optional version of `T`'s parser.
/// - `OptT, optional_text long "x", "META", "help"` and
///   `OptT, optional_text positional "META", "help"`: `OptT(Option<String>)`
///   (a free-form text with no domain type to name).
macro_rules! impl_arg {
    ($ty:ty, positional $meta:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                bpaf::positional::<Self>($meta).help($help)
            }
        }
    };
    ($ty:ty, long $long:literal, $meta:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                bpaf::long($long).help($help).argument::<Self>($meta)
            }
        }
    };
    ($ty:ty, long $long:literal, short $short:literal, $meta:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                bpaf::short($short)
                    .long($long)
                    .help($help)
                    .argument::<Self>($meta)
            }
        }
    };
    ($ty:ty, switch $long:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                use bpaf::Parser as _;
                bpaf::long($long).help($help).switch().map(Self)
            }
        }
    };
    ($ty:ty, switch $long:literal, short $short:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                use bpaf::Parser as _;
                bpaf::short($short)
                    .long($long)
                    .help($help)
                    .switch()
                    .map(Self)
            }
        }
    };
    ($ty:ty, optional_text long $long:literal, $meta:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                use bpaf::Parser as _;
                bpaf::long($long)
                    .help($help)
                    .argument::<String>($meta)
                    .optional()
                    .map(Self)
            }
        }
    };
    ($ty:ty, optional_text positional $meta:literal, $help:literal) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                use bpaf::Parser as _;
                bpaf::positional::<String>($meta)
                    .help($help)
                    .optional()
                    .map(Self)
            }
        }
    };
    ($ty:ty, optional $base:ty) => {
        impl hicli::Arg for $ty {
            fn parser() -> impl bpaf::Parser<Self> {
                use bpaf::Parser as _;
                <$base as hicli::Arg>::parser().optional().map(Self)
            }
        }
    };
}
pub(crate) use impl_arg;

/// A value that is one of a fixed set of names. Implement this and the argument gets its parse error, its list of names and
/// tab completion from [`choice`].
pub(crate) trait Choice: Copy + 'static {
    /// Every value, in the order they are listed.
    const ALL: &'static [Self];
    /// What the value is, for the "unknown ..." message (`sort field`).
    const WHAT: &'static str;
    /// The name the user types.
    fn name(self) -> &'static str;
}

/// The value named `s`.
pub(crate) fn parse_choice<T: Choice>(s: &str) -> Result<T, String> {
    T::ALL
        .iter()
        .copied()
        .find(|value| value.name() == s)
        .ok_or_else(|| {
            format!(
                "unknown {} '{s}' (expected {})",
                T::WHAT,
                names_as_alternatives(&choice_names::<T>())
            )
        })
}

/// `a`, `a or b`, `a, b or c`.
fn names_as_alternatives(names: &[&str]) -> String {
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_string(),
        Some((last, init)) => format!("{} or {last}", init.join(", ")),
    }
}

/// Every name of `T`, in order.
pub(crate) fn choice_names<T: Choice>() -> Vec<&'static str> {
    T::ALL.iter().map(|value| value.name()).collect()
}

/// Read a `T` from the word `word` parses. Completion happens on the word
/// (which parses while half-typed); a validated type is never completed on
/// the empty word.
pub(crate) fn choice<T: Choice>(word: impl Parser<String>) -> impl Parser<T> {
    complete_with(word, |typed: &String| {
        choice_names::<T>()
            .into_iter()
            .filter(|name| name.starts_with(typed.as_str()))
            .map(|name| (name.to_string(), None))
            .collect()
    })
    .parse(|word| parse_choice::<T>(&word))
}

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

// A type used under other names (`winner`, `from`, ...) is a field of a
// multi-value args struct instead.
impl_arg!(
    TrackArg,
    positional "HASH",
    "Content hash (full or partial, with or without sha256: prefix)"
);

/// An optional [`TrackArg`] (none: the command reads hashes from stdin or has a default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptTrackArg(pub Option<TrackArg>);

impl_arg!(OptTrackArg, optional TrackArg);

/// Name of an MDMA playlist, validated by `PlaylistName`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistArg(pub PlaylistName);

impl FromStr for PlaylistArg {
    type Err = PlaylistNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PlaylistName::new(s).map(Self)
    }
}

impl_arg!(PlaylistArg, positional "NAME", "Playlist name");

/// An optional [`PlaylistArg`] (none: the command reads names from stdin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptPlaylistArg(pub Option<PlaylistArg>);

impl_arg!(OptPlaylistArg, optional PlaylistArg);

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
        complete_files(positional::<Self>("PATH").help("Path to an existing file"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_arg;
    use pretty_assertions::assert_eq;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Shade {
        Light,
        DarkBlue,
    }

    impl Choice for Shade {
        const ALL: &'static [Self] = &[Shade::Light, Shade::DarkBlue];
        const WHAT: &'static str = "shade";

        fn name(self) -> &'static str {
            match self {
                Shade::Light => "light",
                Shade::DarkBlue => "dark-blue",
            }
        }
    }

    #[test]
    fn parse_choice_finds_a_value_by_name() {
        assert_eq!(parse_choice::<Shade>("light"), Ok(Shade::Light));
        assert_eq!(parse_choice::<Shade>("dark-blue"), Ok(Shade::DarkBlue));
        assert_eq!(
            parse_choice::<Shade>("Light"),
            Err("unknown shade 'Light' (expected light or dark-blue)".to_string())
        );
        assert_eq!(names_as_alternatives(&["a"]), "a");
        assert_eq!(names_as_alternatives(&["a", "b", "c"]), "a, b or c");
    }

    #[test]
    fn choice_names_follow_the_order_of_all() {
        assert_eq!(choice_names::<Shade>(), vec!["light", "dark-blue"]);
    }

    #[test]
    fn choice_parser_reads_a_name_and_names_the_what_when_unknown() {
        let parser = || choice::<Shade>(positional::<String>("SHADE")).to_options();
        assert_eq!(
            parser().run_inner(&["dark-blue"][..]).ok(),
            Some(Shade::DarkBlue)
        );
        let message = parser()
            .run_inner(&["pink"][..])
            .unwrap_err()
            .unwrap_stderr();
        assert!(message.contains("unknown shade 'pink'"), "{message}");
    }

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
