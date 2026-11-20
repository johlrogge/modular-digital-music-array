//! Generate facts from audio file metadata

use audio_metadata::{discover_all_fields, extract_metadata, TrackMetadata};
use music_facts::{
    Album, AlbumArtPresence, Artist, BitDepth, Bitrate, Channels, DiscNumber, DurationMs,
    DurationSeconds, FactSource, FileSizeBytes, Isrc, MusicFormat, MusicValue, SampleRate, Title,
    TrackNumber, Year,
};
use music_primitives::{Bpm, Key};
use std::collections::HashMap;
use std::path::Path;

use crate::pipeline::{AudioFormat, IngestError, IngestWarning};

impl From<AudioFormat> for MusicFormat {
    fn from(fmt: AudioFormat) -> Self {
        match fmt {
            AudioFormat::Flac => MusicFormat::Flac,
            AudioFormat::Mp3 => MusicFormat::Mp3,
            AudioFormat::Aiff => MusicFormat::Aiff,
            AudioFormat::Wav => MusicFormat::Wav,
            AudioFormat::M4a => MusicFormat::M4a,
        }
    }
}

/// Generate facts from audio file metadata: the file facts, the tag facts and the
/// file-name facts, from one read of the file. The facts the library computes or
/// measures (file path, format, import time, stream properties) are stamped with the
/// `library` source; the facts the file's tags and name state are stamped with the
/// `content` source.
pub fn generate_facts(
    path: &Path,
    format: AudioFormat,
    library: &FactSource,
    content: &FactSource,
) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
    let (metadata, all_fields) = read_metadata(path)?;
    Ok(compose(&metadata, &all_fields, format, library, content))
}

fn compose(
    metadata: &TrackMetadata,
    all_fields: &HashMap<String, String>,
    format: AudioFormat,
    library: &FactSource,
    content: &FactSource,
) -> Vec<(MusicValue, FactSource)> {
    let mut facts = file_facts_from_metadata(metadata, format, library);
    facts.extend(tag_facts_from_metadata(metadata, all_fields, content));
    facts.extend(name_facts_from_metadata(metadata, content));
    facts
}

/// The facts of a file and the warnings raised while reading it.
#[derive(Debug)]
pub struct GeneratedFacts {
    pub facts: Vec<(MusicValue, FactSource)>,
    /// Things the ingest went on without, for the ingest result (ADR-037).
    pub warnings: Vec<IngestWarning>,
}

/// [`generate_facts`], except that an AIFF whose *tags* cannot be read is not a
/// failure (ADR-037): such a file is stored as an AIFF blob, and no AIFF is left
/// behind in the inbox.
///
/// Only the tag read may fail, and only for AIFF. The tolerance is deliberately
/// not for other formats: widening it (to FLAC, say) would change what an
/// unreadable FLAC does today (fail the ingest), which is a product decision, not
/// part of the AIFF blob fallback.
///
/// lofty reads tags and stream properties in one pass, so when it fails the stream
/// properties come from the file's own `COMM` chunk instead
/// ([`audio_hash::stream_info`]: what COMM declares, under any compression, float
/// included). If even that cannot be read (a corrupt file) the original error
/// stands: the audio properties are never guessed. The result then has the file
/// facts and the name facts (from the file name, as no tag states a title or an
/// artist), no tag facts, no `HasAlbumArt` (unknown, as the tags cannot be read),
/// and a warning.
pub fn generate_facts_tolerant(
    path: &Path,
    format: AudioFormat,
    library: &FactSource,
    content: &FactSource,
) -> Result<GeneratedFacts, IngestError> {
    let tag_error = match read_metadata(path) {
        Ok((metadata, all_fields)) => {
            return Ok(GeneratedFacts {
                facts: compose(&metadata, &all_fields, format, library, content),
                warnings: Vec::new(),
            })
        }
        Err(e) if format == AudioFormat::Aiff => e,
        Err(e) => return Err(e),
    };
    let detail = match &tag_error {
        IngestError::MetadataError(m) => m.clone(),
        other => other.to_string(),
    };
    let info =
        audio_hash::stream_info(path).map_err(|_| IngestError::MetadataError(detail.clone()))?;
    let metadata = metadata_from_stream_info(path, &info);
    let mut facts = file_facts_from_metadata(&metadata, format, library);
    // The tags could not be read, so whether there is album art is unknown, not "no".
    facts.retain(|(v, _)| !matches!(v, MusicValue::HasAlbumArt(_)));
    facts.extend(name_facts_from_metadata(&metadata, content));
    let warning = IngestWarning::TagsUnreadable { detail };
    tracing::warn!(path = %path.display(), %warning, "ingesting without tags");
    Ok(GeneratedFacts {
        facts,
        warnings: vec![warning],
    })
}

/// What `COMM` states, in the shape the file facts are made from. The duration is
/// `frames / rate` (`PcmInfo::duration`); the duration facts truncate it to a
/// millisecond like lofty's.
fn metadata_from_stream_info(path: &Path, info: &audio_hash::PcmInfo) -> TrackMetadata {
    TrackMetadata {
        artist: None,
        album: None,
        title: None,
        album_artist: None,
        track_number: None,
        disc_number: None,
        duration: Some(info.duration()),
        sample_rate: Some(info.sample_rate),
        channels: Some(u16::from(info.channels)),
        bit_depth: Some(info.bits_per_sample),
        bitrate: None,
        bpm: None,
        key: None,
        genre: None,
        year: None,
        comment: None,
        file_path: path.to_path_buf(),
        file_size_bytes: std::fs::metadata(path).ok().map(|m| m.len()),
        has_picture: false,
    }
}

/// The facts the file's tags state (title, artist, genre, BPM, key, ...), all
/// stamped with the `content` source. Only what the tags say: nothing derived from
/// the file's name or properties, so the same audio in another container with the
/// same tags yields the same facts.
pub fn tag_facts(
    path: &Path,
    content: &FactSource,
) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
    let (metadata, all_fields) = read_metadata(path)?;
    Ok(tag_facts_from_metadata(&metadata, &all_fields, content))
}

/// The facts the library derives from the file itself (path, import time, format,
/// stream properties, size, album-art presence), all stamped with the `library`
/// source. Describes `path` as stored, whatever its tags say.
pub fn file_facts(
    path: &Path,
    format: AudioFormat,
    library: &FactSource,
) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
    let metadata = extract_metadata(path).map_err(|e| IngestError::MetadataError(e.to_string()))?;
    Ok(file_facts_from_metadata(&metadata, format, library))
}

/// Facts derived from a file *name* when the tags lack them: title (and artist,
/// track number) parsed from the stem of `original_path`, which is the file as the
/// user supplied it, not a staged or hashed copy. Empty when the tags state a title
/// or an artist. A later manual edit overrides these via stainless-facts replace
/// semantics. Stamped with the `content` source.
pub fn name_facts(
    original_path: &Path,
    has_title: bool,
    has_artist: bool,
    has_track: bool,
    content: &FactSource,
) -> Vec<(MusicValue, FactSource)> {
    let mut facts = Vec::new();
    if has_title || has_artist {
        return facts;
    }
    let source = content.clone();
    if let Some(stem) = original_path.file_stem().and_then(|s| s.to_str()) {
        let parsed = crate::filename_parser::parse(stem);
        facts.push((MusicValue::Title(Title::new(&parsed.title)), source.clone()));
        if let Some(artist) = parsed.artist {
            facts.push((MusicValue::Artist(Artist::new(&artist)), source.clone()));
        }
        if !has_track {
            if let Some(n) = parsed.track_number {
                facts.push((MusicValue::TrackNumber(TrackNumber::new(n)), source));
            }
        }
    }
    facts
}

fn read_metadata(path: &Path) -> Result<(TrackMetadata, HashMap<String, String>), IngestError> {
    let metadata = extract_metadata(path).map_err(|e| IngestError::MetadataError(e.to_string()))?;
    let all_fields =
        discover_all_fields(path).map_err(|e| IngestError::MetadataError(e.to_string()))?;
    Ok((metadata, all_fields))
}

fn name_facts_from_metadata(
    metadata: &TrackMetadata,
    content: &FactSource,
) -> Vec<(MusicValue, FactSource)> {
    name_facts(
        &metadata.file_path,
        metadata.title.is_some(),
        metadata.artist.is_some(),
        metadata.track_number.is_some(),
        content,
    )
}

/// Look up a field name across all known tag type prefixes
fn find_field<'a>(all_fields: &'a HashMap<String, String>, field_name: &str) -> Option<&'a String> {
    const PREFIXES: &[&str] = &["VorbisComments", "Id3v2", "Id3v1", "ApeTag"];
    PREFIXES
        .iter()
        .find_map(|prefix| all_fields.get(&format!("{}.{}", prefix, field_name)))
}

/// Tag-stated facts from already-extracted metadata
fn tag_facts_from_metadata(
    metadata: &TrackMetadata,
    all_fields: &HashMap<String, String>,
    content: &FactSource,
) -> Vec<(MusicValue, FactSource)> {
    let mut facts = Vec::new();

    let source = content.clone();

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
            MusicValue::TrackNumber(TrackNumber::new(track_number)),
            source.clone(),
        ));
    }

    if let Some(disc_number) = metadata.disc_number {
        facts.push((
            MusicValue::DiscNumber(DiscNumber::new(disc_number)),
            source.clone(),
        ));
    }

    if let Some(year) = metadata.year {
        facts.push((MusicValue::Year(Year::new(year)), source.clone()));
    }

    // DJ-specific metadata
    if let Some(bpm) = metadata.bpm {
        if let Ok(bpm_value) = Bpm::from_f32(bpm) {
            facts.push((MusicValue::Bpm(bpm_value), source.clone()));
        }
    }

    if let Some(ref key_str) = metadata.key {
        // Any notation: our own exports write the short form (`Abm`, ID3v2 TKEY),
        // third-party taggers write Camelot, Open Key or the long form.
        if let Ok(key) = key_str.parse::<Key>() {
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
        facts.push((MusicValue::Isrc(Isrc::new(isrc.clone())), source.clone()));
    }

    if let Some(label) = find_field(all_fields, "Label") {
        facts.push((MusicValue::Label(label.clone()), source.clone()));
    }

    // Extract recording date/year
    if let Some(recording_date) = all_fields.get("VorbisComments.Unknown(\"recording_date\")") {
        if let Some(year_str) = recording_date.split('-').next() {
            if let Ok(year) = year_str.parse::<u32>() {
                facts.push((MusicValue::RecordingYear(Year::new(year)), source.clone()));
            }
        }

        if recording_date.len() == 10 && recording_date.matches('-').count() == 2 {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(recording_date, "%Y-%m-%d") {
                facts.push((MusicValue::RecordingDate(date), source.clone()));
            }
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

    // Encoder info
    if let Some(encoder) = find_field(all_fields, "EncoderSoftware") {
        facts.push((MusicValue::EncoderSoftware(encoder.clone()), source.clone()));
    }

    if let Some(encoded_by) = find_field(all_fields, "EncodedBy") {
        facts.push((MusicValue::EncodedBy(encoded_by.clone()), source.clone()));
    }

    facts
}

/// File-derived facts from already-extracted metadata: the path, the import time,
/// the format and the stream and file properties the library measures, which the
/// tags do not state. `HasAlbumArt` is file-side: after a conversion it describes
/// the stored file's PICTURE blocks.
fn file_facts_from_metadata(
    metadata: &TrackMetadata,
    format: AudioFormat,
    library: &FactSource,
) -> Vec<(MusicValue, FactSource)> {
    let mut facts = vec![(
        MusicValue::FilePath(metadata.file_path.clone()),
        library.clone(),
    )];
    facts.push((
        MusicValue::Format(MusicFormat::from(format)),
        library.clone(),
    ));
    facts.push((MusicValue::AddedAt(chrono::Utc::now()), library.clone()));

    if let Some(bit_depth) = metadata.bit_depth {
        facts.push((
            MusicValue::BitDepth(BitDepth::new(bit_depth)),
            library.clone(),
        ));
    }

    if let Some(channels) = metadata.channels {
        facts.push((
            MusicValue::Channels(Channels::new(channels as u8)),
            library.clone(),
        ));
    }

    if let Some(sample_rate) = metadata.sample_rate {
        facts.push((
            MusicValue::SampleRate(SampleRate::new(sample_rate)),
            library.clone(),
        ));
    }

    if let Some(duration) = metadata.duration {
        facts.push((
            MusicValue::DurationSeconds(DurationSeconds::new(duration.as_secs() as u32)),
            library.clone(),
        ));
        facts.push((
            MusicValue::DurationMs(DurationMs::from_duration(duration)),
            library.clone(),
        ));
    }

    if let Some(bitrate) = metadata.bitrate {
        facts.push((MusicValue::Bitrate(Bitrate::new(bitrate)), library.clone()));
    }

    if let Some(file_size) = metadata.file_size_bytes {
        facts.push((
            MusicValue::FileSizeBytes(FileSizeBytes::new(file_size)),
            library.clone(),
        ));
    }

    facts.push((
        MusicValue::HasAlbumArt(if metadata.has_picture {
            AlbumArtPresence::Present
        } else {
            AlbumArtPresence::Absent
        }),
        library.clone(),
    ));

    facts
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
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case(AudioFormat::Flac, "FLAC")]
    #[case(AudioFormat::Mp3, "MP3")]
    #[case(AudioFormat::Aiff, "AIFF")]
    #[case(AudioFormat::Wav, "WAV")]
    #[case(AudioFormat::M4a, "M4A")]
    fn format_fact_generated_for_audio_formats(
        #[case] audio_fmt: AudioFormat,
        #[case] expected_display: &str,
    ) {
        use music_facts::MusicFormat;
        let music_fmt = MusicFormat::from(audio_fmt);
        assert_eq!(music_fmt.to_string(), expected_display);
    }

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

    // ---------------------------------------------------------------------------
    // Filename-derived metadata fallback tests
    // ---------------------------------------------------------------------------

    /// All the facts of extracted metadata, composed as `generate_facts` does.
    fn generate_facts_from_metadata(
        metadata: &TrackMetadata,
        all_fields: &HashMap<String, String>,
        library: &FactSource,
        content: &FactSource,
    ) -> Result<Vec<(MusicValue, FactSource)>, IngestError> {
        let mut facts = file_facts_from_metadata(metadata, AudioFormat::Flac, library);
        facts.extend(tag_facts_from_metadata(metadata, all_fields, content));
        facts.extend(name_facts_from_metadata(metadata, content));
        Ok(facts)
    }

    fn make_metadata(
        title: Option<&str>,
        artist: Option<&str>,
        track_number: Option<u32>,
        file_path: &str,
    ) -> TrackMetadata {
        audio_metadata::TrackMetadata {
            title: title.map(|s| s.to_string()),
            artist: artist.map(|s| s.to_string()),
            album: None,
            album_artist: None,
            track_number,
            disc_number: None,
            duration: None,
            sample_rate: None,
            channels: None,
            bit_depth: None,
            bitrate: None,
            bpm: None,
            key: None,
            genre: None,
            year: None,
            comment: None,
            file_path: std::path::PathBuf::from(file_path),
            file_size_bytes: None,
            has_picture: false,
        }
    }

    fn has_title(facts: &[(MusicValue, FactSource)], expected: &str) -> bool {
        facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::Title(t) if t.as_str() == expected))
    }

    fn has_artist(facts: &[(MusicValue, FactSource)], expected: &str) -> bool {
        facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::Artist(a) if a.as_str() == expected))
    }

    fn has_track_number(facts: &[(MusicValue, FactSource)], expected: u32) -> bool {
        facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::TrackNumber(n) if n.value() == expected))
    }

    fn title_count(facts: &[(MusicValue, FactSource)]) -> usize {
        facts
            .iter()
            .filter(|(v, _)| matches!(v, MusicValue::Title(_)))
            .count()
    }

    fn artist_count(facts: &[(MusicValue, FactSource)]) -> usize {
        facts
            .iter()
            .filter(|(v, _)| matches!(v, MusicValue::Artist(_)))
            .count()
    }

    #[test]
    fn measured_properties_carry_the_library_source_and_tags_the_content_source() {
        let mut metadata = make_metadata(Some("Real Title"), None, None, "/music/inbox/x.flac");
        metadata.sample_rate = Some(44100);
        metadata.bit_depth = Some(16);
        let library = FactSource::new("mdma-library", "test", "FilesystemScan");
        let content = FactSource::new("a-client", "test", "Client");
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &library,
            &content,
        )
        .unwrap();
        for (value, source) in &facts {
            let measured = matches!(
                value,
                MusicValue::SampleRate(_)
                    | MusicValue::BitDepth(_)
                    | MusicValue::HasAlbumArt(_)
                    | MusicValue::FilePath(_)
                    | MusicValue::Format(_)
                    | MusicValue::AddedAt(_)
            );
            let expected = if measured { &library } else { &content };
            assert_eq!(source, expected, "source of {value:?}");
        }
        assert!(facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::SampleRate(_))));
        assert!(facts.iter().any(|(v, _)| matches!(v, MusicValue::Title(_))));
    }

    /// A tag key in any notation becomes the same Key fact; garbage becomes none.
    #[rstest]
    #[case("Abm", Some("G#m"))]
    #[case("C", Some("C"))]
    #[case("F#m", Some("F#m"))]
    #[case("Bb", Some("A#"))]
    #[case("8A", Some("Am"))]
    #[case("1m", Some("Am"))]
    #[case("A Minor", Some("Am"))]
    #[case("C Major", Some("C"))]
    #[case("nonsense", None)]
    fn tag_key_in_any_notation_becomes_a_key_fact(
        #[case] tag: &str,
        #[case] expected: Option<&str>,
    ) {
        let mut metadata = make_metadata(Some("T"), None, None, "/music/inbox/x.aiff");
        metadata.key = Some(tag.to_string());
        let library = FactSource::new("mdma-library", "test", "FilesystemScan");
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &library,
            &library,
        )
        .unwrap();
        let key = facts.iter().find_map(|(v, _)| match v {
            MusicValue::Key(k) => Some(*k),
            _ => None,
        });
        assert_eq!(key, expected.map(|e| e.parse::<Key>().unwrap()));
    }

    // ---------------------------------------------------------------------------
    // The tag_facts / file_facts split (ADR-037 decision 4), on a generated AIFF
    // ---------------------------------------------------------------------------

    use audio_fixtures::testkit::{id3, id3::Id3Version, AiffBuilder};

    /// One second of 16-bit stereo 44.1 kHz audio with the full ID3 tag.
    fn tagged_aiff(dir: &tempfile::TempDir, version: Id3Version) -> std::path::PathBuf {
        let path = dir.path().join("fixture.aiff");
        let bytes = AiffBuilder::new().id3(id3::full_tag(version)).build();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// What the fixture's ID3 tag states, as facts.
    fn expected_tag_facts(content: &FactSource) -> Vec<(MusicValue, FactSource)> {
        let facts = vec![
            MusicValue::Title(Title::new(id3::TITLE)),
            MusicValue::Artist(Artist::new(id3::ARTIST)),
            MusicValue::Album(Album::new(id3::ALBUM)),
            MusicValue::TrackNumber(TrackNumber::new(3)),
            MusicValue::DiscNumber(DiscNumber::new(1)),
            MusicValue::Year(Year::new(2024)),
            MusicValue::Bpm(Bpm::from_f32(128.0).unwrap()),
            MusicValue::Key(id3::KEY.parse::<Key>().unwrap()),
            MusicValue::MainGenre(id3::GENRE.to_string()),
            MusicValue::FullGenre(id3::GENRE.to_string()),
            MusicValue::Comment(id3::COMMENT.to_string()),
        ];
        facts.into_iter().map(|v| (v, content.clone())).collect()
    }

    /// What the generated fixture file is, as facts (without the import time).
    fn expected_file_facts(
        path: &std::path::Path,
        library: &FactSource,
    ) -> Vec<(MusicValue, FactSource)> {
        let facts = vec![
            MusicValue::FilePath(path.to_path_buf()),
            MusicValue::Format(MusicFormat::Aiff),
            MusicValue::BitDepth(BitDepth::new(16)),
            MusicValue::Channels(Channels::new(2)),
            MusicValue::SampleRate(SampleRate::new(44_100)),
            MusicValue::DurationSeconds(DurationSeconds::new(1)),
            MusicValue::DurationMs(DurationMs::new(1000)),
            // lofty derives the bitrate from the whole file size (tag included)
            MusicValue::Bitrate(Bitrate::new(1416)),
            MusicValue::FileSizeBytes(FileSizeBytes::new(std::fs::metadata(path).unwrap().len())),
            MusicValue::HasAlbumArt(AlbumArtPresence::Present),
        ];
        facts.into_iter().map(|v| (v, library.clone())).collect()
    }

    fn without_import_time(facts: Vec<(MusicValue, FactSource)>) -> Vec<(MusicValue, FactSource)> {
        facts
            .into_iter()
            .filter(|(v, _)| !matches!(v, MusicValue::AddedAt(_)))
            .collect()
    }

    fn sources() -> (FactSource, FactSource) {
        (
            FactSource::new("mdma-library", "test", "FilesystemScan"),
            FactSource::new("a-client", "test", "Client"),
        )
    }

    fn debug_sorted(facts: &[(MusicValue, FactSource)]) -> Vec<String> {
        let mut v: Vec<String> = facts.iter().map(|f| format!("{f:?}")).collect();
        v.sort();
        v
    }

    #[rstest]
    #[case(Id3Version::V23)]
    #[case(Id3Version::V24)]
    fn tag_facts_are_the_tags_and_all_carry_the_content_source(#[case] version: Id3Version) {
        let dir = tempfile::tempdir().unwrap();
        let path = tagged_aiff(&dir, version);
        let (_, content) = sources();
        let facts = tag_facts(&path, &content).unwrap();
        for (value, source) in &facts {
            assert_eq!(source, &content, "source of {value:?}");
            assert!(
                !matches!(
                    value,
                    MusicValue::FilePath(_)
                        | MusicValue::Format(_)
                        | MusicValue::AddedAt(_)
                        | MusicValue::FileSizeBytes(_)
                        | MusicValue::DurationSeconds(_)
                        | MusicValue::DurationMs(_)
                        | MusicValue::SampleRate(_)
                        | MusicValue::BitDepth(_)
                        | MusicValue::Channels(_)
                        | MusicValue::Bitrate(_)
                        | MusicValue::HasAlbumArt(_)
                ),
                "{value:?} is a file fact, not a tag fact"
            );
        }
        assert_eq!(
            debug_sorted(&facts),
            debug_sorted(&expected_tag_facts(&content))
        );
    }

    /// ADR-037 check 5.4 compares tag facts across containers and names: an untagged
    /// file gives no tag facts whatever it is called, and the facts of a tagged file
    /// do not depend on its name.
    #[test]
    fn tag_facts_do_not_depend_on_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let (_, content) = sources();
        let tagged = tagged_aiff(&dir, Id3Version::V24);
        let renamed = dir.path().join("0123456789abcdef.aiff");
        std::fs::copy(&tagged, &renamed).unwrap();
        assert_eq!(
            debug_sorted(&tag_facts(&tagged, &content).unwrap()),
            debug_sorted(&tag_facts(&renamed, &content).unwrap())
        );

        let untagged = dir.path().join("01 - Yagya - Empty Streets.aiff");
        std::fs::write(&untagged, AiffBuilder::new().build()).unwrap();
        assert_eq!(tag_facts(&untagged, &content).unwrap(), vec![]);
    }

    /// An untagged file's title, artist and track come from its (original) name.
    #[test]
    fn name_facts_come_from_the_original_name_and_only_for_untagged_files() {
        let (library, content) = sources();
        let dir = tempfile::tempdir().unwrap();
        let untagged = dir.path().join("01 - Yagya - Empty Streets.aiff");
        std::fs::write(&untagged, AiffBuilder::new().build()).unwrap();

        let facts = generate_facts(&untagged, AudioFormat::Aiff, &library, &content).unwrap();
        assert!(has_title(&facts, "Empty Streets"));
        assert!(has_artist(&facts, "Yagya"));
        assert!(has_track_number(&facts, 1));

        let name = Path::new("/music/inbox/01 - Yagya - Empty Streets.aiff");
        assert_eq!(name_facts(name, true, false, false, &content), vec![]);
        assert_eq!(name_facts(name, false, true, false, &content), vec![]);
        let facts = name_facts(name, false, false, true, &content);
        assert!(has_title(&facts, "Empty Streets"));
        assert!(!facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::TrackNumber(_))));
    }

    #[test]
    fn file_facts_are_the_file_and_all_carry_the_library_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = tagged_aiff(&dir, Id3Version::V24);
        let (library, _) = sources();
        let facts = file_facts(&path, AudioFormat::Aiff, &library).unwrap();
        for (value, source) in &facts {
            assert_eq!(source, &library, "source of {value:?}");
        }
        assert!(facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::AddedAt(_))));
        assert_eq!(
            debug_sorted(&without_import_time(facts)),
            debug_sorted(&expected_file_facts(&path, &library))
        );
    }

    /// Characterisation: the complete fact set, with sources, for the tagged fixture.
    #[test]
    fn generate_facts_yields_exactly_the_file_and_tag_facts_with_their_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = tagged_aiff(&dir, Id3Version::V24);
        let (library, content) = sources();
        let facts = generate_facts(&path, AudioFormat::Aiff, &library, &content).unwrap();
        let mut expected = expected_file_facts(&path, &library);
        expected.extend(expected_tag_facts(&content));
        assert_eq!(
            debug_sorted(&without_import_time(facts)),
            debug_sorted(&expected)
        );
    }

    // ---------------------------------------------------------------------------
    // Unreadable tags of an AIFF blob (ADR-037 phase 4)
    // ---------------------------------------------------------------------------

    /// An AIFF whose odd-length `COMT` chunk makes lofty 0.18 lose its place.
    fn odd_comt_aiff(
        dir: &tempfile::TempDir,
        name: &str,
        builder: AiffBuilder,
    ) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let bytes = builder
            .chunk(b"COMT", audio_fixtures::testkit::comt_payload())
            .id3(id3::full_tag(Id3Version::V24))
            .build();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn an_aiff_with_unreadable_tags_gets_file_and_name_facts_and_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = odd_comt_aiff(
            &dir,
            "01 - Yagya - Empty Streets.aiff",
            AiffBuilder::new().frames(44_100),
        );
        let (library, content) = sources();
        // the strict path still fails: this is what lofty cannot read
        assert!(matches!(
            generate_facts(&path, AudioFormat::Aiff, &library, &content),
            Err(IngestError::MetadataError(_))
        ));

        let got = generate_facts_tolerant(&path, AudioFormat::Aiff, &library, &content).unwrap();
        assert!(matches!(
            got.warnings.as_slice(),
            [IngestWarning::TagsUnreadable { .. }]
        ));
        // the album art is unknown, not absent
        assert!(!got
            .facts
            .iter()
            .any(|(v, _)| matches!(v, MusicValue::HasAlbumArt(_))));

        let facts = without_import_time(got.facts);
        let values: Vec<&MusicValue> = facts.iter().map(|(v, _)| v).collect();
        // the file facts, from COMM, in the library source
        for want in [
            MusicValue::FilePath(path.clone()),
            MusicValue::Format(MusicFormat::Aiff),
            MusicValue::BitDepth(BitDepth::new(16)),
            MusicValue::Channels(Channels::new(2)),
            MusicValue::SampleRate(SampleRate::new(44_100)),
            MusicValue::DurationSeconds(DurationSeconds::new(1)),
            MusicValue::DurationMs(DurationMs::new(1000)),
            MusicValue::FileSizeBytes(FileSizeBytes::new(std::fs::metadata(&path).unwrap().len())),
        ] {
            let (_, source) = facts
                .iter()
                .find(|(v, _)| *v == want)
                .unwrap_or_else(|| panic!("{want:?} missing in {values:?}"));
            assert_eq!(source, &library, "{want:?}");
        }
        // the name facts, from the original file name, in the content source
        assert!(has_title(&facts, "Empty Streets"));
        assert!(has_artist(&facts, "Yagya"));
        assert!(has_track_number(&facts, 1));
        for (v, source) in &facts {
            if matches!(
                v,
                MusicValue::Title(_) | MusicValue::Artist(_) | MusicValue::TrackNumber(_)
            ) {
                assert_eq!(source, &content, "{v:?}");
            }
        }
        // and nothing the tags would have stated
        assert!(!values.iter().any(|v| matches!(
            v,
            MusicValue::Album(_) | MusicValue::Bpm(_) | MusicValue::Key(_)
        )));
    }

    #[test]
    fn readable_tags_give_no_warning_and_the_strict_facts() {
        let dir = tempfile::tempdir().unwrap();
        let path = tagged_aiff(&dir, Id3Version::V24);
        let (library, content) = sources();
        let got = generate_facts_tolerant(&path, AudioFormat::Aiff, &library, &content).unwrap();
        assert!(got.warnings.is_empty());
        let strict = generate_facts(&path, AudioFormat::Aiff, &library, &content).unwrap();
        assert_eq!(
            debug_sorted(&without_import_time(got.facts)),
            debug_sorted(&without_import_time(strict))
        );
    }

    #[test]
    fn float_and_ulaw_aiff_c_with_unreadable_tags_still_get_their_properties() {
        use audio_fixtures::testkit::Codec;
        let dir = tempfile::tempdir().unwrap();
        let (library, content) = sources();
        for (name, codec, bits) in [("f.aifc", Codec::Fl32, 32), ("u.aifc", Codec::Ulaw, 16)] {
            let path = odd_comt_aiff(&dir, name, AiffBuilder::new().frames(44_100).aifc(codec));
            let got =
                generate_facts_tolerant(&path, AudioFormat::Aiff, &library, &content).unwrap();
            let values: Vec<&MusicValue> = got.facts.iter().map(|(v, _)| v).collect();
            assert!(values.contains(&&MusicValue::BitDepth(BitDepth::new(bits))));
            assert!(values.contains(&&MusicValue::SampleRate(SampleRate::new(44_100))));
            assert!(values.contains(&&MusicValue::DurationMs(DurationMs::new(1000))));
        }
    }

    /// The fallback must state what lofty states for a file lofty can read.
    #[rstest]
    #[case(44_100, 16, 2, 44_100)]
    #[case(44_100, 16, 2, 1_001)] // 22.69 ms: the truncation
    #[case(48_000, 24, 1, 12_345)] // 257.18 ms
    #[case(96_000, 24, 2, 1_234)] // 12.85 ms
    #[case(44_100, 8, 1, 99_999)] // 2267.76 ms
    fn the_fallback_properties_equal_lofty_s(
        #[case] rate: u32,
        #[case] bits: u16,
        #[case] channels: u16,
        #[case] frames: u32,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.aiff");
        let builder = AiffBuilder::new()
            .rate(rate)
            .bits(bits)
            .channels(channels)
            .frames(frames);
        std::fs::write(&path, builder.build()).unwrap();
        let lofty = extract_metadata(&path).unwrap();
        let info = audio_hash::stream_info(&path).unwrap();
        let fallback = metadata_from_stream_info(&path, &info);
        assert_eq!(fallback.sample_rate, lofty.sample_rate);
        assert_eq!(fallback.bit_depth, lofty.bit_depth);
        assert_eq!(fallback.channels, lofty.channels);
        let ms = |m: &TrackMetadata| DurationMs::from_duration(m.duration.unwrap());
        assert_eq!(ms(&fallback), ms(&lofty), "{frames} frames at {rate} Hz");
    }

    #[test]
    fn unreadable_tags_are_only_tolerated_when_the_properties_are_readable() {
        let dir = tempfile::tempdir().unwrap();
        let (library, content) = sources();
        // not an AIFF at all
        let junk = dir.path().join("junk.aiff");
        std::fs::write(&junk, b"definitely not audio").unwrap();
        assert!(matches!(
            generate_facts_tolerant(&junk, AudioFormat::Aiff, &library, &content),
            Err(IngestError::MetadataError(_))
        ));
        // other formats keep failing on unreadable tags
        let mp3 = dir.path().join("x.mp3");
        std::fs::write(&mp3, b"definitely not audio").unwrap();
        assert!(matches!(
            generate_facts_tolerant(&mp3, AudioFormat::Mp3, &library, &content),
            Err(IngestError::MetadataError(_))
        ));
    }

    #[test]
    fn exact_duration_is_emitted_next_to_the_whole_seconds() {
        let mut metadata = make_metadata(Some("T"), None, None, "/music/inbox/x.flac");
        metadata.duration = Some(std::time::Duration::from_millis(367_123));
        let library = FactSource::new("mdma-library", "test", "FilesystemScan");
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &library,
            &library,
        )
        .unwrap();
        let values: Vec<&MusicValue> = facts.iter().map(|(v, _)| v).collect();
        assert!(values.contains(&&MusicValue::DurationMs(DurationMs::new(367_123))));
        assert!(values.contains(&&MusicValue::DurationSeconds(DurationSeconds::new(367))));
    }

    #[test]
    fn fallback_emits_title_artist_track_for_three_part_stem() {
        let metadata = make_metadata(
            None,
            None,
            None,
            "/music/inbox/01 - Yagya - Empty Streets.flac",
        );
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &FactSource::new("mdma-library", "test", "FilesystemScan"),
            &FactSource::new("mdma-library", "test", "Unknown"),
        )
        .unwrap();
        assert!(
            has_title(&facts, "Empty Streets"),
            "expected title 'Empty Streets'"
        );
        assert!(has_artist(&facts, "Yagya"), "expected artist 'Yagya'");
        assert!(has_track_number(&facts, 1), "expected track number 1");
    }

    #[test]
    fn fallback_preserves_title_with_dashes() {
        let metadata = make_metadata(
            None,
            None,
            None,
            "/music/inbox/Yagya - Don't Call - Reprise.flac",
        );
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &FactSource::new("mdma-library", "test", "FilesystemScan"),
            &FactSource::new("mdma-library", "test", "Unknown"),
        )
        .unwrap();
        assert!(
            has_title(&facts, "Don't Call - Reprise"),
            "expected title with internal dash"
        );
        assert!(has_artist(&facts, "Yagya"), "expected artist 'Yagya'");
    }

    #[test]
    fn fallback_emits_only_title_when_no_dash() {
        let metadata = make_metadata(None, None, None, "/music/inbox/Empty Streets.wav");
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &FactSource::new("mdma-library", "test", "FilesystemScan"),
            &FactSource::new("mdma-library", "test", "Unknown"),
        )
        .unwrap();
        assert!(
            has_title(&facts, "Empty Streets"),
            "expected title 'Empty Streets'"
        );
        assert_eq!(
            artist_count(&facts),
            0,
            "no artist fact expected for single-segment stem"
        );
    }

    #[test]
    fn fallback_skipped_when_embedded_title_present() {
        let metadata = make_metadata(
            Some("Real Title"),
            None,
            None,
            "/music/inbox/Yagya - Empty Streets.wav",
        );
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &FactSource::new("mdma-library", "test", "FilesystemScan"),
            &FactSource::new("mdma-library", "test", "Unknown"),
        )
        .unwrap();
        // Embedded title should be used, filename fallback should not fire
        assert!(
            has_title(&facts, "Real Title"),
            "embedded title should be present"
        );
        assert_eq!(title_count(&facts), 1, "only one title fact expected");
        // No artist from filename either (condition requires both to be None)
        assert_eq!(artist_count(&facts), 0, "no artist fact expected");
    }

    #[test]
    fn fallback_skipped_when_embedded_artist_present() {
        let metadata = make_metadata(
            None,
            Some("Real Artist"),
            None,
            "/music/inbox/Yagya - Empty Streets.wav",
        );
        let facts = generate_facts_from_metadata(
            &metadata,
            &std::collections::HashMap::new(),
            &FactSource::new("mdma-library", "test", "FilesystemScan"),
            &FactSource::new("mdma-library", "test", "Unknown"),
        )
        .unwrap();
        // Artist is present, so fallback must not fire — no Title or extra Artist from filename
        assert!(
            has_artist(&facts, "Real Artist"),
            "embedded artist should be present"
        );
        assert_eq!(
            title_count(&facts),
            0,
            "no title fact when only artist is embedded"
        );
        assert_eq!(artist_count(&facts), 1, "only the embedded artist fact");
    }
}
