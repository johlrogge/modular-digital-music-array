use crate::ctx::Ctx;
use crate::output::handle_error;
use chrono::NaiveDate;
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::ContentHash;
use mdma_client::LibraryBackend;
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
        hash: String,
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
        hash: String,
        /// Field name to retract.
        #[arg(long, value_enum)]
        field: FactField,
        /// Value to retract (must match the existing fact's value).
        #[arg(long)]
        value: String,
    },
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    let lib = ctx.library();
    match &command {
        Command::Add { hash, field, value } => {
            handle_fact_add(&lib, hash.clone(), *field, value.clone())
        }
        Command::Retract { hash, field, value } => {
            handle_fact_retract(&lib, hash.clone(), *field, value.clone())
        }
    }
}

/// Metadata fields that can be added or retracted by the user.
///
/// Internal/derived fields (FilePath, Source, ItemId, audio properties, play events,
/// AddedAt, cover art) are intentionally excluded — they are managed automatically.
#[derive(clap::ValueEnum, Debug, Clone, Copy)]
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

fn handle_fact_add(
    client: &LibraryBackend,
    hash: String,
    field: FactField,
    value: String,
) -> Result<()> {
    let music_value = match parse_field_value(field, &value) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
    };
    let content_hash = ContentHash::new(hash.clone());
    match client.write_fact(&content_hash, music_value) {
        Ok(()) => {
            println!("Added fact for {}: {:?} = {}", hash, field, value);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_fact_retract(
    client: &LibraryBackend,
    hash: String,
    field: FactField,
    value: String,
) -> Result<()> {
    let music_value = match parse_field_value(field, &value) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
    };
    let content_hash = ContentHash::new(hash.clone());
    match client.retract_fact(&content_hash, music_value) {
        Ok(()) => {
            println!("Retracted fact for {}: {:?} = {}", hash, field, value);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use music_facts::MusicValue;

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
