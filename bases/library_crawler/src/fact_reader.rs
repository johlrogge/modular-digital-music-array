// bases/library_crawler/src/fact_reader.rs
use color_eyre::Result;
use music_facts::{ContentHash, FactSource, MusicValue};
use stainless_facts::{aggregate_facts, Fact, FactAggregator, FactStreamReader};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Aggregated track information reconstructed from facts
#[derive(Debug, Default)]
pub struct AggregatedTrack {
    pub entity: Option<ContentHash>,
    pub file_path: Option<PathBuf>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub year: Option<u32>,
    pub bpm: Option<String>, // Display as string
    pub key: Option<String>,
    pub main_genre: Option<String>,
    pub style_descriptors: Vec<String>,
    pub full_genre: Option<String>,
    pub label: Option<String>,
    pub recording_year: Option<u32>,
    pub recording_date: Option<String>,
    pub isrc: Option<String>,
    pub beatport_track_url: Option<String>,
    pub beatport_label_url: Option<String>,
    pub beatport_track_id: Option<String>,
    pub bandcamp_url: Option<String>,
    pub comment: Option<String>,
    pub duration_seconds: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub bitrate: Option<u32>,
    pub file_size_bytes: Option<u64>,
    pub has_album_art: bool,
    pub encoder_software: Option<String>,
    pub encoded_by: Option<String>,
    pub fact_count: usize,
}

/// Implement FactAggregator for AggregatedTrack
impl FactAggregator<ContentHash, MusicValue, FactSource> for AggregatedTrack {
    fn assert(&mut self, value: &MusicValue, _source: &FactSource) {
        use MusicValue::*;

        self.fact_count += 1;

        match value {
            FilePath(path) => self.file_path = Some(path.clone()),
            Title(s) => self.title = Some(s.clone()),
            Artist(s) => self.artist = Some(s.clone()),
            Album(s) => self.album = Some(s.clone()),
            AlbumArtist(s) => self.album_artist = Some(s.clone()),
            TrackNumber(n) => self.track_number = Some(n.0),
            Year(y) => self.year = Some(y.0),
            Bpm(bpm) => self.bpm = Some(format!("{:.2}", bpm.as_f32())),
            Key(key) => self.key = Some(format!("{} (Camelot: {})", key, key.to_camelot())),
            MainGenre(s) => self.main_genre = Some(s.clone()),
            StyleDescriptor(s) => {
                if !self.style_descriptors.contains(s) {
                    self.style_descriptors.push(s.clone());
                }
            }
            FullGenre(s) => self.full_genre = Some(s.clone()),
            Isrc(isrc) => self.isrc = Some(isrc.0.clone()),
            Label(s) => self.label = Some(s.clone()),
            RecordingYear(y) => self.recording_year = Some(y.0),
            RecordingDate(s) => self.recording_date = Some(s.clone()),
            BeatportTrackUrl(s) => self.beatport_track_url = Some(s.clone()),
            BeatportLabelUrl(s) => self.beatport_label_url = Some(s.clone()),
            BandcampUrl(s) => self.bandcamp_url = Some(s.clone()),
            Comment(s) => self.comment = Some(s.clone()),
            BeatportTrackId(s) => self.beatport_track_id = Some(s.clone()),
            BitDepth(bd) => self.bit_depth = Some(bd.0),
            Channels(ch) => self.channels = Some(ch.0),
            SampleRate(sr) => self.sample_rate = Some(sr.0),
            DurationSeconds(ds) => self.duration_seconds = Some(ds.0),
            Bitrate(br) => self.bitrate = Some(br.0),
            FileSizeBytes(fsb) => self.file_size_bytes = Some(fsb.0),
            HasAlbumArt(has) => self.has_album_art = *has,
            EncoderSoftware(s) => self.encoder_software = Some(s.clone()),
            EncodedBy(s) => self.encoded_by = Some(s.clone()),
        }
    }

    fn retract(&mut self, value: &MusicValue, _source: &FactSource) {
        use MusicValue::*;

        // For cardinality-one fields, retract sets to None
        match value {
            FilePath(_) => self.file_path = None,
            Title(_) => self.title = None,
            Artist(_) => self.artist = None,
            Album(_) => self.album = None,
            AlbumArtist(_) => self.album_artist = None,
            TrackNumber(_) => self.track_number = None,
            Year(_) => self.year = None,
            Bpm(_) => self.bpm = None,
            Key(_) => self.key = None,
            MainGenre(_) => self.main_genre = None,
            StyleDescriptor(s) => {
                // For cardinality-many fields, remove the specific value
                self.style_descriptors.retain(|desc| desc != s);
            }
            FullGenre(_) => self.full_genre = None,
            Isrc(_) => self.isrc = None,
            Label(_) => self.label = None,
            RecordingYear(_) => self.recording_year = None,
            RecordingDate(_) => self.recording_date = None,
            BeatportTrackUrl(_) => self.beatport_track_url = None,
            BeatportLabelUrl(_) => self.beatport_label_url = None,
            BandcampUrl(_) => self.bandcamp_url = None,
            Comment(_) => self.comment = None,
            BeatportTrackId(_) => self.beatport_track_id = None,
            BitDepth(_) => self.bit_depth = None,
            Channels(_) => self.channels = None,
            SampleRate(_) => self.sample_rate = None,
            DurationSeconds(_) => self.duration_seconds = None,
            Bitrate(_) => self.bitrate = None,
            FileSizeBytes(_) => self.file_size_bytes = None,
            HasAlbumArt(_) => self.has_album_art = false,
            EncoderSoftware(_) => self.encoder_software = None,
            EncodedBy(_) => self.encoded_by = None,
        }
    }

    fn assert_unknown(
        &mut self,
        _attribute: &str,
        _value: &serde_json::Value,
        _source: &FactSource,
    ) {
        // Unknown attributes are gracefully ignored for forward compatibility
    }

    fn retract_unknown(
        &mut self,
        _attribute: &str,
        _value: &serde_json::Value,
        _source: &FactSource,
    ) {
        // Unknown attributes are gracefully ignored
    }
}

/// Read and aggregate facts from a fact stream file
pub fn read_and_aggregate(path: impl AsRef<Path>) -> Result<HashMap<ContentHash, AggregatedTrack>> {
    let mut reader = FactStreamReader::open(path)?;

    let mut facts = Vec::new();

    // Read all facts from the stream
    while let Some(result) = reader.next() {
        let fact: Fact<ContentHash, MusicValue, FactSource> = result?;
        facts.push(fact);
    }

    // Use stainless-facts aggregate_facts function
    let mut aggregated: HashMap<ContentHash, AggregatedTrack> = aggregate_facts(facts);

    // Set entity field on each track
    for (entity, track) in aggregated.iter_mut() {
        track.entity = Some(entity.clone());
    }

    Ok(aggregated)
}

impl AggregatedTrack {
    /// Get display name for track
    pub fn display_name(&self) -> String {
        match (&self.artist, &self.title) {
            (Some(artist), Some(title)) => format!("{} - {}", artist, title),
            (Some(artist), None) => artist.clone(),
            (None, Some(title)) => title.clone(),
            (None, None) => self.entity.as_ref().unwrap().0.clone(),
        }
    }

    /// Format duration as MM:SS
    pub fn format_duration(&self) -> String {
        match self.duration_seconds {
            Some(seconds) => {
                let minutes = seconds / 60;
                let secs = seconds % 60;
                format!("{}:{:02}", minutes, secs)
            }
            None => "Unknown".to_string(),
        }
    }

    /// Format file size as MB
    pub fn format_file_size(&self) -> String {
        match self.file_size_bytes {
            Some(bytes) => {
                let mb = bytes as f64 / 1_048_576.0;
                format!("{:.1} MB", mb)
            }
            None => "Unknown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use music_facts::FactOrigin;
    use music_primitives::Bpm;
    use stainless_facts::{Fact, FactStreamWriter, Operation};
    use tempfile::NamedTempFile;

    #[test]
    fn aggregation_combines_facts_correctly() {
        let temp = NamedTempFile::new().unwrap();
        let content_hash = ContentHash("test_hash_123".to_string());
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let now = Utc::now();

        // Write some facts
        let facts = vec![
            Fact::new(
                content_hash.clone(),
                MusicValue::Title("Test Track".to_string()),
                now,
                source.clone(),
                Operation::Assert,
            ),
            Fact::new(
                content_hash.clone(),
                MusicValue::Artist("Test Artist".to_string()),
                now,
                source.clone(),
                Operation::Assert,
            ),
            Fact::new(
                content_hash.clone(),
                MusicValue::Bpm(Bpm::from_f32(128.0).unwrap()),
                now,
                source.clone(),
                Operation::Assert,
            ),
        ];

        let mut writer = FactStreamWriter::open(temp.path()).unwrap();
        writer.write_batch(&facts).unwrap();
        drop(writer);

        // Read and aggregate
        let aggregated = read_and_aggregate(temp.path()).unwrap();

        assert_eq!(aggregated.len(), 1);
        let track = aggregated.get(&content_hash).unwrap();

        assert_eq!(track.title, Some("Test Track".to_string()));
        assert_eq!(track.artist, Some("Test Artist".to_string()));
        assert_eq!(track.bpm, Some("128.00".to_string()));
        assert_eq!(track.fact_count, 3);
    }

    #[test]
    fn retraction_removes_fact() {
        let temp = NamedTempFile::new().unwrap();
        let content_hash = ContentHash("test_hash_456".to_string());
        let source = FactSource::new("test", "1.0.0", FactOrigin::Unknown);
        let now = Utc::now();

        // Assert then retract
        let facts = vec![
            Fact::new(
                content_hash.clone(),
                MusicValue::Title("First Title".to_string()),
                now,
                source.clone(),
                Operation::Assert,
            ),
            Fact::new(
                content_hash.clone(),
                MusicValue::Title("First Title".to_string()),
                now,
                source.clone(),
                Operation::Retract,
            ),
            Fact::new(
                content_hash.clone(),
                MusicValue::Title("Second Title".to_string()),
                now,
                source.clone(),
                Operation::Assert,
            ),
        ];

        let mut writer = FactStreamWriter::open(temp.path()).unwrap();
        writer.write_batch(&facts).unwrap();
        drop(writer);

        // Read and aggregate
        let aggregated = read_and_aggregate(temp.path()).unwrap();
        let track = aggregated.get(&content_hash).unwrap();

        // Should have the second title, not the first
        assert_eq!(track.title, Some("Second Title".to_string()));
    }
}
