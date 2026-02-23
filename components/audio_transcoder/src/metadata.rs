use std::path::Path;

use lofty::{Accessor, AudioFile, ItemKey, ItemValue, Probe, Tag, TagItem, TagType, TaggedFileExt};

use crate::{ExportFormat, TrackMetadata, TranscoderError};

/// Inject metadata tags into an already-written audio file.
///
/// Uses lofty to apply ID3v2 for AIFF, RIFF INFO / ID3v2 for WAV,
/// and Vorbis comments for FLAC.
pub fn inject_metadata(
    path: &Path,
    format: &ExportFormat,
    meta: &TrackMetadata,
) -> Result<(), TranscoderError> {
    let tag_type = tag_type_for_format(format);

    // Open the file and get or create the tag
    let mut tagged_file = Probe::open(path)?.read()?;

    // Try to get an existing tag of the right type, otherwise create one
    let tag = if let Some(t) = tagged_file.tag_mut(tag_type) {
        t
    } else {
        tagged_file.insert_tag(Tag::new(tag_type));
        tagged_file
            .tag_mut(tag_type)
            .expect("tag was just inserted")
    };

    if let Some(title) = &meta.title {
        tag.set_title(title.clone());
    }
    if let Some(artist) = &meta.artist {
        tag.set_artist(artist.clone());
    }
    if let Some(album) = &meta.album {
        tag.set_album(album.clone());
    }
    if let Some(bpm) = meta.bpm {
        // Store BPM as a string tag item; the key name varies by tag type
        let bpm_key = ItemKey::Unknown("BPM".to_string());
        let bpm_value = ItemValue::Text(format!("{:.2}", bpm));
        let _ = tag.insert(TagItem::new(bpm_key, bpm_value));
    }
    if let Some(key) = &meta.key {
        let key_item_key = ItemKey::Unknown("INITIALKEY".to_string());
        let key_value = ItemValue::Text(key.clone());
        let _ = tag.insert(TagItem::new(key_item_key, key_value));
    }

    tagged_file.save_to_path(path)?;
    Ok(())
}

fn tag_type_for_format(format: &ExportFormat) -> TagType {
    match format {
        ExportFormat::Aiff => TagType::Id3v2,
        ExportFormat::Wav => TagType::Id3v2,
        ExportFormat::Flac => TagType::VorbisComments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{transcode_with_metadata, BitDepth, ExportFormat, TrackMetadata, TranscodeParams};
    use tempfile::NamedTempFile;

    fn make_wav_file(channels: u16, sample_rate: u32, bit_depth: BitDepth) -> NamedTempFile {
        let params = TranscodeParams {
            format: ExportFormat::Wav,
            channels,
            sample_rate,
            bit_depth,
        };
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32 / 500.0 - 1.0) * 0.5).collect();
        let meta = TrackMetadata {
            title: Some("Test Title".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            bpm: Some(128.0),
            key: Some("Am".to_string()),
        };
        let tmp = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        transcode_with_metadata(tmp.path(), &params, &samples, &meta).unwrap();
        tmp
    }

    #[test]
    fn metadata_title_roundtrip_wav() {
        let tmp = make_wav_file(2, 44100, BitDepth::Sixteen);
        let tagged = Probe::open(tmp.path()).unwrap().read().unwrap();
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("expected a tag");
        assert_eq!(tag.title().as_deref(), Some("Test Title"));
    }

    #[test]
    fn metadata_artist_roundtrip_wav() {
        let tmp = make_wav_file(2, 44100, BitDepth::Sixteen);
        let tagged = Probe::open(tmp.path()).unwrap().read().unwrap();
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("expected a tag");
        assert_eq!(tag.artist().as_deref(), Some("Test Artist"));
    }

    #[test]
    fn metadata_album_roundtrip_wav() {
        let tmp = make_wav_file(2, 44100, BitDepth::Sixteen);
        let tagged = Probe::open(tmp.path()).unwrap().read().unwrap();
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("expected a tag");
        assert_eq!(tag.album().as_deref(), Some("Test Album"));
    }

    fn make_aiff_file() -> NamedTempFile {
        let params = TranscodeParams {
            format: ExportFormat::Aiff,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        };
        let samples: Vec<f32> = vec![0.0f32; 1000];
        let meta = TrackMetadata {
            title: Some("AIFF Title".to_string()),
            artist: Some("AIFF Artist".to_string()),
            album: None,
            bpm: None,
            key: None,
        };
        let tmp = tempfile::Builder::new().suffix(".aiff").tempfile().unwrap();
        transcode_with_metadata(tmp.path(), &params, &samples, &meta).unwrap();
        tmp
    }

    #[test]
    fn metadata_title_roundtrip_aiff() {
        let tmp = make_aiff_file();
        let tagged = Probe::open(tmp.path()).unwrap().read().unwrap();
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("expected a tag");
        assert_eq!(tag.title().as_deref(), Some("AIFF Title"));
    }
}
