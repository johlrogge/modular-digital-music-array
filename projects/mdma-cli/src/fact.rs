use crate::args::{usage_error, TrackArg};
use crate::ctx::Ctx;
use crate::output::handle_error;
use chrono::NaiveDate;
use clap::Subcommand;
use color_eyre::Result;
use music_facts::{
    Album, Artist, Bpm, DiscNumber, EnergyLevel, Isrc, Key, MusicValue, Title, TrackNumber,
    TrackRole, Year,
};

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Add or update a fact on a track.
    ///
    /// Stainless-facts replace semantics: the most recent fact for a given field wins
    /// for non-collection types. Note this does NOT auto-retract the previous value;
    /// retract explicitly first if you want to clean history.
    Add {
        /// Track content hash (prefix accepted, git-style).
        hash: TrackArg,
        /// Field name to add. Tab-completion shows supported fields.
        #[arg(long, value_enum)]
        field: FactField,
        /// Value for the field.
        #[arg(long)]
        value: String,
    },
    /// Retract an existing fact from a track.
    ///
    /// Required for collection-type fields (each value is independent).
    /// For scalar fields, retract removes that value from history.
    Retract {
        /// Track content hash (prefix accepted, git-style).
        hash: TrackArg,
        /// Field name to retract.
        #[arg(long, value_enum)]
        field: FactField,
        /// Value to retract (must match the existing fact's value).
        #[arg(long)]
        value: String,
    },
}

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
    hash: TrackArg,
}

impl FactArgs {
    /// Checks the value against the field; a bad value is a usage error (exit 2).
    fn new(
        field: FactField,
        value: String,
        hash: TrackArg,
    ) -> std::result::Result<Self, clap::Error> {
        let value = FactValueArg::new(field, value)
            .map_err(|msg| usage_error(clap::error::ErrorKind::ValueValidation, msg))?;
        Ok(Self { field, value, hash })
    }
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::Add { hash, field, value } => add(
            FactArgs::new(field, value, hash).unwrap_or_else(|e| e.exit()),
            ctx,
        ),
        Command::Retract { hash, field, value } => retract(
            FactArgs::new(field, value, hash).unwrap_or_else(|e| e.exit()),
            ctx,
        ),
    }
}

/// Metadata fields that can be added or retracted by the user.
///
/// Internal/derived fields (FilePath, Source, ItemId, audio properties, play events,
/// AddedAt, cover art) are intentionally excluded — they are managed automatically.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
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

impl FactField {
    const ALL: [FactField; 22] = [
        FactField::Title,
        FactField::Artist,
        FactField::Album,
        FactField::AlbumArtist,
        FactField::TrackNumber,
        FactField::DiscNumber,
        FactField::Year,
        FactField::Bpm,
        FactField::Key,
        FactField::MainGenre,
        FactField::StyleDescriptor,
        FactField::FullGenre,
        FactField::Isrc,
        FactField::Label,
        FactField::RecordingYear,
        FactField::RecordingDate,
        FactField::Comment,
        FactField::BeatportLabelUrl,
        FactField::BeatportTrackUrl,
        FactField::BandcampUrl,
        FactField::Role,
        FactField::Energy,
    ];

    fn name(self) -> &'static str {
        match self {
            FactField::Title => "title",
            FactField::Artist => "artist",
            FactField::Album => "album",
            FactField::AlbumArtist => "album-artist",
            FactField::TrackNumber => "track-number",
            FactField::DiscNumber => "disc-number",
            FactField::Year => "year",
            FactField::Bpm => "bpm",
            FactField::Key => "key",
            FactField::MainGenre => "main-genre",
            FactField::StyleDescriptor => "style-descriptor",
            FactField::FullGenre => "full-genre",
            FactField::Isrc => "isrc",
            FactField::Label => "label",
            FactField::RecordingYear => "recording-year",
            FactField::RecordingDate => "recording-date",
            FactField::Comment => "comment",
            FactField::BeatportLabelUrl => "beatport-label-url",
            FactField::BeatportTrackUrl => "beatport-track-url",
            FactField::BandcampUrl => "bandcamp-url",
            FactField::Role => "role",
            FactField::Energy => "energy",
        }
    }
}

impl std::fmt::Display for FactField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for FactField {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|field| field.name() == s)
            .ok_or_else(|| format!("unknown fact field '{s}'"))
    }
}

/// Parse a raw string value into a `MusicValue` for the given `FactField`.
///
/// Returns `Err(String)` with an actionable diagnostic if the value cannot be parsed.
fn parse_field_value(field: FactField, raw: &str) -> Result<MusicValue, String> {
    match field {
        FactField::Title => Ok(MusicValue::Title(Title::new(raw))),
        FactField::Artist => Ok(MusicValue::Artist(Artist::new(raw))),
        FactField::Album => Ok(MusicValue::Album(Album::new(raw))),
        FactField::AlbumArtist => Ok(MusicValue::AlbumArtist(Artist::new(raw))),
        FactField::TrackNumber => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for TrackNumber '{raw}': {e}"))?;
            Ok(MusicValue::TrackNumber(TrackNumber::new(n)))
        }
        FactField::DiscNumber => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for DiscNumber '{raw}': {e}"))?;
            Ok(MusicValue::DiscNumber(DiscNumber::new(n)))
        }
        FactField::Year => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for Year '{raw}': {e}"))?;
            Ok(MusicValue::Year(Year::new(n)))
        }
        FactField::Bpm => {
            let b = raw
                .parse::<f32>()
                .map_err(|e| format!("invalid float for Bpm '{raw}': {e}"))?;
            let bpm = Bpm::from_f32(b)
                .map_err(|e| format!("BPM out of range for '{raw}': {e}"))?;
            Ok(MusicValue::Bpm(bpm))
        }
        FactField::Key => Key::from_camelot(raw)
            .or_else(|_| Key::from_traditional(raw))
            .map(MusicValue::Key)
            .map_err(|_| {
                format!("invalid key '{raw}': use Camelot notation (1A-12B) or traditional (e.g. 'C Major', 'A Minor')")
            }),
        FactField::MainGenre => Ok(MusicValue::MainGenre(raw.to_string())),
        FactField::StyleDescriptor => Ok(MusicValue::StyleDescriptor(raw.to_string())),
        FactField::FullGenre => Ok(MusicValue::FullGenre(raw.to_string())),
        FactField::Isrc => Ok(MusicValue::Isrc(Isrc::new(raw))),
        FactField::Label => Ok(MusicValue::Label(raw.to_string())),
        FactField::RecordingYear => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for RecordingYear '{raw}': {e}"))?;
            Ok(MusicValue::RecordingYear(Year::new(n)))
        }
        FactField::RecordingDate => {
            let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|e| format!("invalid date for RecordingDate '{raw}' (use YYYY-MM-DD): {e}"))?;
            Ok(MusicValue::RecordingDate(date))
        }
        FactField::Comment => Ok(MusicValue::Comment(raw.to_string())),
        FactField::BeatportLabelUrl => Ok(MusicValue::BeatportLabelUrl(raw.to_string())),
        FactField::BeatportTrackUrl => Ok(MusicValue::BeatportTrackUrl(raw.to_string())),
        FactField::BandcampUrl => Ok(MusicValue::BandcampUrl(raw.to_string())),
        FactField::Role => raw
            .parse::<TrackRole>()
            .map(MusicValue::Role)
            .map_err(|_| {
                format!(
                    "invalid role '{raw}': accepted values are opener, build-up, peak, banger, cool-down, closer, filler"
                )
            }),
        FactField::Energy => {
            let n = raw
                .parse::<u8>()
                .map_err(|e| format!("invalid integer for Energy '{raw}': {e}"))?;
            let level = EnergyLevel::new(n)
                .map_err(|e| format!("energy out of range for '{raw}': {e}"))?;
            Ok(MusicValue::Energy(level))
        }
    }
}

pub(crate) fn add(args: FactArgs, ctx: &Ctx) -> Result<()> {
    let FactArgs {
        field,
        value: FactValueArg { raw, value },
        hash: TrackArg(hash),
    } = args;
    let library = ctx.library();
    match library.write_fact(&hash, value) {
        Ok(()) => {
            println!("Added fact for {}: {:?} = {}", hash, field, raw);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

pub(crate) fn retract(args: FactArgs, ctx: &Ctx) -> Result<()> {
    let FactArgs {
        field,
        value: FactValueArg { raw, value },
        hash: TrackArg(hash),
    } = args;
    let library = ctx.library();
    match library.retract_fact(&hash, value) {
        Ok(()) => {
            println!("Retracted fact for {}: {:?} = {}", hash, field, raw);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use music_facts::MusicValue;

    #[test]
    fn fact_field_matches_clap_value_enum() {
        use clap::ValueEnum;
        for v in FactField::value_variants() {
            let name = v.to_possible_value().unwrap().get_name().to_string();
            assert_eq!(name.parse::<FactField>(), Ok(*v));
            assert_eq!(v.to_string(), name);
        }
        assert_eq!(FactField::value_variants().len(), FactField::ALL.len());
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
    fn fact_value_is_checked_against_the_field() {
        let hash = "abcd1234".parse::<TrackArg>().unwrap();
        let err = FactArgs::new(FactField::Year, "twenty".into(), hash.clone()).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string()
                .contains("invalid integer for Year 'twenty'"),
            "{err}"
        );
        let ok = FactArgs::new(FactField::Year, "2024".into(), hash).unwrap();
        assert_eq!(ok.value.raw, "2024");
        assert!(matches!(ok.value.value, MusicValue::Year(_)));
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
