//! Generate facts from audio file metadata

use audio_metadata::{discover_all_fields, extract_metadata, TrackMetadata};
use music_facts::{
    Album, Artist, BitDepth, Bitrate, Channels, ContentHash, DurationSeconds, FactOrigin,
    FactSource, FileSizeBytes, Isrc, MusicValue, SampleRate, Title, TrackNumber, Year,
};
use music_primitives::{Bpm, Key};
use std::collections::HashMap;
use std::path::Path;

use crate::pipeline::{AudioFormat, IngestError};

/// Generate facts from audio file metadata
pub fn generate_facts(
    path: &Path,
    content_hash: &ContentHash,
    _format: AudioFormat,
) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
    let metadata = extract_metadata(path).map_err(|e| IngestError::MetadataError(e.to_string()))?;

    let all_fields =
        discover_all_fields(path).map_err(|e| IngestError::MetadataError(e.to_string()))?;

    generate_facts_from_metadata(content_hash, &metadata, &all_fields)
}

/// Look up a field name across all known tag type prefixes
fn find_field<'a>(all_fields: &'a HashMap<String, String>, field_name: &str) -> Option<&'a String> {
    const PREFIXES: &[&str] = &["VorbisComments", "Id3v2", "Id3v1", "ApeTag"];
    PREFIXES
        .iter()
        .find_map(|prefix| all_fields.get(&format!("{}.{}", prefix, field_name)))
}

/// Generate facts from already-extracted metadata
fn generate_facts_from_metadata(
    _content_hash: &ContentHash,
    metadata: &TrackMetadata,
    all_fields: &HashMap<String, String>,
) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
    let mut facts = Vec::new();

    // Infer origin from path and comment
    let origin = FactOrigin::infer(&metadata.file_path, &metadata.comment);
    let source = FactSource::new("mdma-library", env!("CARGO_PKG_VERSION"), origin);

    // File location
    facts.push((
        MusicValue::FilePath(metadata.file_path.clone()),
        source.clone(),
    ));

    // Basic metadata
    if let Some(ref title) = metadata.title {
        facts.push((MusicValue::Title(Title::new(title)), source.clone()));
    }

    if let Some(ref artist) = metadata.artist {
        facts.push((MusicValue::Artist(Artist::new(artist)), source.clone()));
    }

    if let Some(ref album) = metadata.album {
        facts.push((MusicValue::Album(Album::new(album)), source.clone()));
    }

    if let Some(ref album_artist) = metadata.album_artist {
        facts.push((
            MusicValue::AlbumArtist(Artist::new(album_artist)),
            source.clone(),
        ));
    }

    if let Some(track_number) = metadata.track_number {
        facts.push((
            MusicValue::TrackNumber(TrackNumber(track_number)),
            source.clone(),
        ));
    }

    if let Some(year) = metadata.year {
        facts.push((MusicValue::Year(Year(year)), source.clone()));
    }

    // DJ-specific metadata
    if let Some(bpm) = metadata.bpm {
        if let Ok(bpm_value) = Bpm::from_f32(bpm) {
            facts.push((MusicValue::Bpm(bpm_value), source.clone()));
        }
    }

    if let Some(ref key_str) = metadata.key {
        if let Ok(key) = Key::from_traditional(key_str) {
            facts.push((MusicValue::Key(key), source.clone()));
        }
    }

    if let Some(ref genre) = metadata.genre {
        let (main_genre, style_descriptors, full_genre) = parse_genre(genre);

        facts.push((MusicValue::MainGenre(main_genre), source.clone()));

        for descriptor in style_descriptors {
            facts.push((MusicValue::StyleDescriptor(descriptor), source.clone()));
        }

        facts.push((MusicValue::FullGenre(full_genre), source.clone()));
    }

    // Catalog info (from all_fields for Beatport)
    if let Some(isrc) = find_field(all_fields, "Isrc") {
        facts.push((MusicValue::Isrc(Isrc(isrc.clone())), source.clone()));
    }

    if let Some(label) = find_field(all_fields, "Label") {
        facts.push((MusicValue::Label(label.clone()), source.clone()));
    }

    // Extract recording date/year
    if let Some(recording_date) = all_fields.get("VorbisComments.Unknown(\"recording_date\")") {
        if let Some(year_str) = recording_date.split('-').next() {
            if let Ok(year) = year_str.parse::<u32>() {
                facts.push((MusicValue::RecordingYear(Year(year)), source.clone()));
            }
        }

        if recording_date.len() == 10 && recording_date.matches('-').count() == 2 {
            facts.push((
                MusicValue::RecordingDate(recording_date.clone()),
                source.clone(),
            ));
        }
    }

    // URLs (Beatport)
    if let Some(track_url) = all_fields.get("VorbisComments.Unknown(\"track_url\")") {
        facts.push((
            MusicValue::BeatportTrackUrl(track_url.clone()),
            source.clone(),
        ));
    }

    if let Some(label_url) = all_fields.get("VorbisComments.Unknown(\"label_url\")") {
        facts.push((
            MusicValue::BeatportLabelUrl(label_url.clone()),
            source.clone(),
        ));
    }

    if let Some(fileowner) = all_fields.get("VorbisComments.Unknown(\"fileowner\")") {
        facts.push((
            MusicValue::BeatportTrackId(fileowner.clone()),
            source.clone(),
        ));
    }

    // Bandcamp URL (from comment)
    if let Some(ref comment) = metadata.comment {
        if comment.starts_with("Visit https://") && comment.contains(".bandcamp.com") {
            let url = comment.trim_start_matches("Visit ");
            facts.push((MusicValue::BandcampUrl(url.to_string()), source.clone()));
        } else {
            facts.push((MusicValue::Comment(comment.clone()), source.clone()));
        }
    }

    // Audio properties
    if let Some(bit_depth) = metadata.bit_depth {
        facts.push((MusicValue::BitDepth(BitDepth(bit_depth)), source.clone()));
    }

    if let Some(channels) = metadata.channels {
        facts.push((
            MusicValue::Channels(Channels(channels as u8)),
            source.clone(),
        ));
    }

    if let Some(sample_rate) = metadata.sample_rate {
        facts.push((
            MusicValue::SampleRate(SampleRate(sample_rate)),
            source.clone(),
        ));
    }

    if let Some(duration) = metadata.duration {
        facts.push((
            MusicValue::DurationSeconds(DurationSeconds(duration.as_secs() as u32)),
            source.clone(),
        ));
    }

    if let Some(bitrate) = metadata.bitrate {
        facts.push((MusicValue::Bitrate(Bitrate(bitrate)), source.clone()));
    }

    // File properties
    if let Some(file_size) = metadata.file_size_bytes {
        facts.push((
            MusicValue::FileSizeBytes(FileSizeBytes(file_size)),
            source.clone(),
        ));
    }

    facts.push((
        MusicValue::HasAlbumArt(metadata.has_picture),
        source.clone(),
    ));

    // Encoder info
    if let Some(encoder) = find_field(all_fields, "EncoderSoftware") {
        facts.push((MusicValue::EncoderSoftware(encoder.clone()), source.clone()));
    }

    if let Some(encoded_by) = find_field(all_fields, "EncodedBy") {
        facts.push((MusicValue::EncodedBy(encoded_by.clone()), source.clone()));
    }

    Ok(facts)
}

/// Parse genre string into components
///
/// Examples:
/// - "Techno (Peak Time / Driving)" → ("Techno", ["Peak Time", "Driving"], full)
/// - "Progressive House" → ("Progressive House", [], full)
fn parse_genre(genre: &str) -> (String, Vec<String>, String) {
    let full_genre = genre.to_string();

    if let Some((main, style_part)) = genre.split_once(" (") {
        let style_part = style_part.trim_end_matches(')');
        let descriptors: Vec<String> = style_part
            .split(" / ")
            .map(|s| s.trim().to_string())
            .collect();

        (main.to_string(), descriptors, full_genre)
    } else {
        (genre.to_string(), vec![], full_genre)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_genre_with_descriptors() {
        let (main, descriptors, full) = parse_genre("Techno (Peak Time / Driving)");
        assert_eq!(main, "Techno");
        assert_eq!(descriptors, vec!["Peak Time", "Driving"]);
        assert_eq!(full, "Techno (Peak Time / Driving)");
    }

    #[test]
    fn parse_genre_simple() {
        let (main, descriptors, full) = parse_genre("Progressive House");
        assert_eq!(main, "Progressive House");
        assert_eq!(descriptors.len(), 0);
        assert_eq!(full, "Progressive House");
    }
}
