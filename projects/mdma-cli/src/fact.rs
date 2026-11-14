use crate::args::HashInput;
use crate::ctx::Ctx;
use crate::output::{for_each_track, TrackFailure};
use crate::stdin::{dedup_hashes, hashes_of_input};
use bpaf::{construct, Parser};
use chrono::NaiveDate;
use color_eyre::Result;
use hicli::{Arg, Choice};
use library_ipc_client::{USER_ORIGIN, USER_TOOL};
use music_facts::{
    Album, Artist, Bpm, DiscNumber, EnergyLevel, FactSource, Isrc, Key, MusicValue, OriginKind,
    Title, TrackNumber, TrackRole, Year,
};
use std::str::FromStr;

/// The value of a fact as typed, with the domain value it parses to for its field.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FactValueArg {
    raw: String,
    value: MusicValue,
}

impl FactValueArg {
    /// What counts as a valid value depends on the field, so this is a cross-argument check.
    fn new(field: FactField, raw: String) -> std::result::Result<Self, String> {
        let value = parse_field_value(field, &raw)?;
        Ok(Self { raw, value })
    }
}

/// A fact on a track, as named on the command line.
#[derive(Debug)]
pub(crate) struct FactArgs {
    field: FactField,
    value: FactValueArg,
    hash: HashInput,
}

/// Value for the field (to retract: must match the existing fact's value)
// Checked against the field in `FactArgs`, so it is the text as typed here.
#[derive(Debug, Clone, Arg)]
#[arg(long = "value", metavar = "VALUE")]
pub(crate) struct RawValueArg(pub String);

// Stays hand-written: the value is checked against the field, a rule that spans
// two arguments.
impl Arg for FactArgs {
    /// `<HASH|-> --field <FIELD> --value <VALUE>`; the value is checked against
    /// the field, so a bad value is a parse error. `add` and `retract` share it.
    fn parser() -> impl Parser<Self> {
        let field = <FactField as Arg>::parser();
        let value = <RawValueArg as Arg>::parser();
        let fact = construct!(field, value).parse(|(field, RawValueArg(raw))| {
            FactValueArg::new(field, raw).map(|value| (field, value))
        });
        let hash = <HashInput as Arg>::parser();
        construct!(fact, hash).map(|((field, value), hash)| Self { field, value, hash })
    }
}

/// Fact field
///
/// Metadata fields that can be added or retracted by the user.
///
/// Internal/derived fields (FilePath, Source, ItemId, audio properties, play events,
/// AddedAt, cover art) are intentionally excluded — they are managed automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Choice, Arg)]
#[arg(long = "field", metavar = "FIELD")]
pub(crate) enum FactField {
    Title,
    Artist,
    Album,
    AlbumArtist,
    TrackNumber,
    DiscNumber,
    Year,
    Bpm,
    Key,
    MainGenre,
    StyleDescriptor,
    FullGenre,
    Isrc,
    Label,
    RecordingYear,
    RecordingDate,
    Comment,
    BeatportLabelUrl,
    BeatportTrackUrl,
    BandcampUrl,
    Role,
    Energy,
}

/// Parse a raw string value into a `MusicValue` for the given `FactField`.
///
/// The error is a phrase that names the field and says what it must be, without
/// echoing the value (the argument parser prints that).
fn parse_field_value(field: FactField, raw: &str) -> Result<MusicValue, String> {
    let name = field.name();
    let whole_number = || format!("`{name}` must be a whole number");
    match field {
        FactField::Title => Ok(MusicValue::Title(Title::new(raw))),
        FactField::Artist => Ok(MusicValue::Artist(Artist::new(raw))),
        FactField::Album => Ok(MusicValue::Album(Album::new(raw))),
        FactField::AlbumArtist => Ok(MusicValue::AlbumArtist(Artist::new(raw))),
        FactField::TrackNumber => {
            let n = raw.parse::<u32>().map_err(|_| whole_number())?;
            Ok(MusicValue::TrackNumber(TrackNumber::new(n)))
        }
        FactField::DiscNumber => {
            let n = raw.parse::<u32>().map_err(|_| whole_number())?;
            Ok(MusicValue::DiscNumber(DiscNumber::new(n)))
        }
        FactField::Year => {
            let n = raw.parse::<u32>().map_err(|_| whole_number())?;
            Ok(MusicValue::Year(Year::new(n)))
        }
        FactField::Bpm => {
            let b = raw
                .parse::<f32>()
                .map_err(|_| format!("`{name}` must be a number"))?;
            let bpm = Bpm::from_f32(b).map_err(|_| format!("`{name}` is out of range"))?;
            Ok(MusicValue::Bpm(bpm))
        }
        FactField::Key => Key::from_camelot(raw)
            .or_else(|_| Key::from_traditional(raw))
            .map(MusicValue::Key)
            .map_err(|_| {
                format!("`{name}` must be Camelot notation (1A-12B) or traditional (e.g. `C Major`, `A Minor`)")
            }),
        FactField::MainGenre => Ok(MusicValue::MainGenre(raw.to_string())),
        FactField::StyleDescriptor => Ok(MusicValue::StyleDescriptor(raw.to_string())),
        FactField::FullGenre => Ok(MusicValue::FullGenre(raw.to_string())),
        FactField::Isrc => Ok(MusicValue::Isrc(Isrc::new(raw))),
        FactField::Label => Ok(MusicValue::Label(raw.to_string())),
        FactField::RecordingYear => {
            let n = raw.parse::<u32>().map_err(|_| whole_number())?;
            Ok(MusicValue::RecordingYear(Year::new(n)))
        }
        FactField::RecordingDate => {
            let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|_| format!("`{name}` must be a date as YYYY-MM-DD"))?;
            Ok(MusicValue::RecordingDate(date))
        }
        FactField::Comment => Ok(MusicValue::Comment(raw.to_string())),
        FactField::BeatportLabelUrl => Ok(MusicValue::BeatportLabelUrl(raw.to_string())),
        FactField::BeatportTrackUrl => Ok(MusicValue::BeatportTrackUrl(raw.to_string())),
        FactField::BandcampUrl => Ok(MusicValue::BandcampUrl(raw.to_string())),
        FactField::Role => raw.parse::<TrackRole>().map(MusicValue::Role).map_err(|_| {
            format!(
                "`{name}` must be one of opener, build-up, peak, banger, cool-down, closer, filler"
            )
        }),
        FactField::Energy => {
            let level = raw
                .parse::<u8>()
                .ok()
                .and_then(|n| EnergyLevel::new(n).ok())
                .ok_or_else(|| format!("`{name}` must be a whole number from 1 to 10"))?;
            Ok(MusicValue::Energy(level))
        }
    }
}

/// Add or update a fact on a track
///
/// Stainless-facts replace semantics: the most recent fact for a given field wins
/// for non-collection types. Note this does NOT auto-retract the previous value;
/// retract explicitly first if you want to clean history.
///
/// Give `-` instead of a hash to add the fact to every track whose hash is read from
/// stdin (each hash once). One line per track; a track that fails is reported on
/// stderr, the rest still run, and the exit code is 1.
///
/// # Examples
///
/// ```sh
/// mdma fact add abcd1234 --field main-genre --value Techno
/// mdma search --artist CBL | mdma fact add - --field label --value Ultimae
/// ```
#[hicli::command]
pub(crate) fn add(args: FactArgs, ctx: &Ctx) -> Result<hicli::Success> {
    let FactArgs {
        field,
        value: FactValueArg { raw, value },
        hash,
    } = args;
    let library = ctx.library()?;
    let hashes = dedup_hashes(hashes_of_input(hash)?);
    let source = library_ipc_client::user_fact_source(env!("CARGO_PKG_VERSION"));
    for_each_track(hashes, |hash| {
        library
            .write_fact_as(hash, value.clone(), &source)
            .map(|()| format!("Added fact {:?} = {}", field, raw))
    })
}

/// Origin kind of the fact to retract (default: User; Rekordbox for imported values, Upload for uploaded files' tags)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "origin", metavar = "KIND", complete = complete_origins)]
pub(crate) struct OriginArg(pub OriginKind);

/// The origin kinds the CLI writes, that start with `typed`.
fn complete_origins(typed: &str) -> Vec<hicli::Candidate> {
    use crate::origins::{REKORDBOX_ORIGIN, UPLOAD_ORIGIN};
    [
        (USER_ORIGIN, "manual edits"),
        (REKORDBOX_ORIGIN, "imported from Rekordbox"),
        (UPLOAD_ORIGIN, "tags of uploaded files"),
    ]
    .into_iter()
    .filter(|(origin, _)| origin.starts_with(typed))
    .map(|(origin, what)| (origin.to_string(), Some(what.to_string())))
    .collect()
}

impl FromStr for OriginArg {
    // A phrase that reads after hicli's `--origin` label, so it does not name the flag.
    type Err = &'static str;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        OriginKind::new(s)
            .map(Self)
            .map_err(|_| "must not be empty")
    }
}

/// The origin a retract targets when `--origin` is not given: a manual edit.
fn user_origin() -> OriginKind {
    OriginKind::new(USER_ORIGIN).expect("USER_ORIGIN is not empty")
}

// A fact to retract, and the origin that wrote it. The tool is always the user's
// (`mdma`); the origin kind tells a manual edit (`User`) from, say, a Rekordbox import.
// The origin comes first, so its value is not taken for the positional hash. No doc
// comment: on a composed struct it would be printed as a heading in the help.
#[allow(missing_docs)]
#[derive(Debug, Arg)]
pub(crate) struct RetractArgs {
    origin: Option<OriginArg>,
    fact: FactArgs,
}

/// Retract an existing fact from a track
///
/// Required for collection-type fields (each value is independent).
/// For scalar fields, retract removes that value from history.
/// A fact written by an import or an upload is retracted with the matching `--origin`.
///
/// Give `-` instead of a hash to retract the fact from every track whose hash is read
/// from stdin (each hash once). One line per track; a track that had no such fact, or
/// can't be found, is reported on stderr, the rest still run, and the exit code is 1.
///
/// # Examples
///
/// ```sh
/// mdma fact retract abcd1234 --field main-genre --value Techno
/// mdma search --label Ultimae | mdma fact retract - --field label --value Ultimae
/// ```
#[hicli::command]
pub(crate) fn retract(args: RetractArgs, ctx: &Ctx) -> Result<hicli::Success> {
    let RetractArgs {
        origin,
        fact:
            FactArgs {
                field,
                value: FactValueArg { raw, value },
                hash,
            },
    } = args;
    let origin = origin.map_or_else(user_origin, |OriginArg(kind)| kind);
    let library = ctx.library()?;
    let hashes = dedup_hashes(hashes_of_input(hash)?);
    let source = FactSource::new(USER_TOOL, env!("CARGO_PKG_VERSION"), origin.as_str());
    // A retract that matched nothing is a failed line, so scripts can tell.
    for_each_track(hashes, |hash| {
        match library.retract_fact_as(hash, value.clone(), &source)? {
            true => Ok(format!("Retracted fact {:?} = {}", field, raw)),
            false => Err(TrackFailure::Said(format!(
                "no live {:?} = {} written by {}",
                field, raw, origin
            ))),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use music_facts::MusicValue;

    #[test]
    fn fact_field_names_round_trip() {
        for v in FactField::VARIANTS {
            assert_eq!(v.name().parse::<FactField>(), Ok(*v));
            assert_eq!(v.to_string(), v.name());
        }
        assert!("nope".parse::<FactField>().is_err());
    }

    // ── argv parsing ──────────────────────────────────────────────────────────

    use crate::test_support::{parses, usage_error};

    fn fact_argv<'a>(verb: &'a str, hash: &'a str, value: &'a str, field: &'a str) -> Vec<&'a str> {
        vec!["fact", verb, hash, "--field", field, "--value", value]
    }

    #[test]
    fn fact_hash_must_be_a_valid_hash() {
        for verb in ["add", "retract"] {
            for bad in ["abcd123", "ABCD1234"] {
                usage_error(&fact_argv(verb, bad, "x", "title"));
            }
            parses(&fact_argv(verb, "abcd1234", "x", "title"));
        }
    }

    #[test]
    fn fact_value_is_checked_against_the_field_at_parse_time() {
        for verb in ["add", "retract"] {
            let e = usage_error(&fact_argv(verb, "abcd1234", "twenty", "year"));
            assert!(e.contains("`year` must be a whole number"), "{e}");
            assert!(!e.contains("'twenty'"), "the value is not echoed: {e}");
        }
    }

    #[test]
    fn multi_word_fact_fields_are_kebab_case() {
        use crate::test_support::run_arg;
        for (typed, field) in [
            ("main-genre", FactField::MainGenre),
            ("album-artist", FactField::AlbumArtist),
            ("track-number", FactField::TrackNumber),
            ("beatport-label-url", FactField::BeatportLabelUrl),
        ] {
            assert_eq!(
                run_arg::<FactField>(&["--field", typed]),
                Ok(field),
                "{typed}"
            );
        }
        // Only the kebab-case spelling parses; the error lists the names.
        let err = run_arg::<FactField>(&["--field", "MainGenre"]).unwrap_err();
        assert!(err.contains("`album-artist`, `track-number`"), "{err}");
        assert!(err.contains("`main-genre`"), "{err}");
    }

    #[test]
    fn field_value_errors_are_phrases_that_do_not_echo_the_value() {
        for (field, raw, phrase) in [
            (FactField::Bpm, "fast", "`bpm` must be a number"),
            (FactField::Bpm, "5.0", "`bpm` is out of range"),
            (
                FactField::Energy,
                "11",
                "`energy` must be a whole number from 1 to 10",
            ),
            (FactField::Role, "dancer", "`role` must be one of opener"),
            (FactField::Key, "NotAKey", "`key` must be Camelot notation"),
            (
                FactField::RecordingDate,
                "2024/06/15",
                "`recording-date` must be a date as YYYY-MM-DD",
            ),
        ] {
            let err = parse_field_value(field, raw).unwrap_err();
            assert!(err.starts_with(phrase), "{err}");
            assert!(!err.contains(raw), "{err}");
        }
    }

    #[test]
    fn fact_args_parser_reads_field_value_and_hash() {
        use crate::test_support::run_arg;
        let args =
            run_arg::<FactArgs>(&["--field", "year", "--value", "2024", "abcd1234"]).unwrap();
        assert_eq!(args.field, FactField::Year);
        assert_eq!(args.value.raw, "2024");
        assert!(matches!(args.value.value, MusicValue::Year(_)));
        assert_eq!(args.hash, "abcd1234".parse::<HashInput>().unwrap());
    }

    #[test]
    fn fact_args_parser_checks_the_value_against_the_field() {
        use crate::test_support::run_arg;
        let err =
            run_arg::<FactArgs>(&["--field", "year", "--value", "twenty", "abcd1234"]).unwrap_err();
        assert!(err.contains("`year` must be a whole number"), "{err}");
        let err =
            run_arg::<FactArgs>(&["--field", "nope", "--value", "x", "abcd1234"]).unwrap_err();
        assert!(err.contains("`--field`"), "{err}");
        assert!(err.contains("expected `title`, `artist`,"), "{err}");
        assert!(err.contains("`bandcamp-url`, `role` or `energy`"), "{err}");
        assert!(run_arg::<FactArgs>(&["--field", "year", "--value", "1"]).is_err());
    }

    #[test]
    fn retract_origin_defaults_to_user_and_can_name_rekordbox() {
        use crate::test_support::run_arg;
        let base = ["--field", "year", "--value", "2024", "abcd1234"];
        let default = run_arg::<RetractArgs>(&base).unwrap();
        assert_eq!(default.origin, None);
        assert_eq!(user_origin().as_str(), "User");
        let rb = run_arg::<RetractArgs>(&[
            "--field",
            "year",
            "--value",
            "2024",
            "--origin",
            "Rekordbox",
            "abcd1234",
        ])
        .unwrap();
        assert_eq!(rb.origin.unwrap().0.as_str(), "Rekordbox");
        let err = run_arg::<RetractArgs>(&[
            "--field", "year", "--value", "2024", "--origin", "", "abcd1234",
        ])
        .unwrap_err();
        assert!(err.ends_with("`--origin`: must not be empty"), "{err}");
    }

    #[test]
    fn retract_help_names_the_origins_it_knows() {
        use crate::origins::{REKORDBOX_ORIGIN, UPLOAD_ORIGIN};
        let help = crate::test_support::help(&["fact", "retract", "--help"]);
        for origin in [USER_ORIGIN, REKORDBOX_ORIGIN, UPLOAD_ORIGIN] {
            assert!(help.contains(origin), "{origin}: {help}");
        }
    }

    // ── parse_field_value ─────────────────────────────────────────────────────

    #[test]
    fn parse_field_value_title_valid() {
        let result = parse_field_value(FactField::Title, "Spacewalk");
        assert!(
            matches!(result, Ok(MusicValue::Title(_))),
            "expected MusicValue::Title, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_year_valid() {
        let result = parse_field_value(FactField::Year, "2024");
        assert!(
            matches!(result, Ok(MusicValue::Year(_))),
            "expected MusicValue::Year, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_year_invalid() {
        let result = parse_field_value(FactField::Year, "twenty-four");
        assert!(result.is_err(), "expected Err for non-integer year");
    }

    #[test]
    fn parse_field_value_bpm_valid() {
        let result = parse_field_value(FactField::Bpm, "128.0");
        assert!(
            matches!(result, Ok(MusicValue::Bpm(_))),
            "expected MusicValue::Bpm, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_bpm_out_of_range() {
        // 5.0 BPM is below the valid range
        let result = parse_field_value(FactField::Bpm, "5.0");
        assert!(result.is_err(), "expected Err for out-of-range BPM");
    }

    #[test]
    fn parse_field_value_key_camelot_valid() {
        let result = parse_field_value(FactField::Key, "8B");
        assert!(
            matches!(result, Ok(MusicValue::Key(_))),
            "expected MusicValue::Key, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_key_invalid() {
        let result = parse_field_value(FactField::Key, "NotAKey");
        assert!(result.is_err(), "expected Err for invalid key");
    }

    #[test]
    fn parse_field_value_recording_date_valid() {
        let result = parse_field_value(FactField::RecordingDate, "2024-06-15");
        assert!(
            matches!(result, Ok(MusicValue::RecordingDate(_))),
            "expected MusicValue::RecordingDate, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_recording_date_bad_format() {
        let result = parse_field_value(FactField::RecordingDate, "2024/06/15");
        assert!(result.is_err(), "expected Err for bad date format");
    }
}
