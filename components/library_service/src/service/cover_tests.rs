//! Tests of the cover heal: a cover is present when the track has a `CoverArtPath` fact
//! and that file exists; the rest is repaired (fact, no file) or extracted (no fact).

use super::tests::{make_empty_service, FIXTURE_S16_L8};
use super::*;
use lofty::{MimeType, Picture, PictureType, Tag, TagExt, TagType, TaggedFileExt};
use pretty_assertions::assert_eq;

/// Not a decodable image; the heal moves bytes, it never decodes them.
const ART: &[u8] = b"\xFF\xD8\xFF\xE0 pretend this is a jpeg";

fn summary_of(f: impl FnOnce(&mut CoverSummary)) -> CoverSummary {
    let mut s = CoverSummary::default();
    f(&mut s);
    s
}

/// Put a FLAC at `blob_rel` under the music dir, with `art` embedded as its front
/// cover if given.
fn write_blob(service: &LibraryService, blob_rel: &str, art: Option<&[u8]>) {
    let path = service.music_dir.join(blob_rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, FIXTURE_S16_L8).unwrap();
    if let Some(art) = art {
        let mut tagged = lofty::Probe::open(&path).unwrap().read().unwrap();
        if tagged.primary_tag_mut().is_none() {
            tagged.insert_tag(Tag::new(TagType::VorbisComments));
        }
        let picture = Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            None,
            art.to_vec(),
        );
        let tag = tagged.primary_tag_mut().unwrap();
        tag.push_picture(picture);
        tag.save_to_path(&path).unwrap();
    }
}

/// Index a track with `blob_rel` and, if given, a cover fact naming `cover_rel`.
fn index_track(service: &LibraryService, hash: &str, blob_rel: &str, cover_rel: Option<&str>) {
    let mut entry = IndexedTrackInfo::new_empty(hash.to_owned());
    entry.blob_path = PathBuf::from(blob_rel);
    entry.cover_art_path = cover_rel.map(PathBuf::from);
    service.tracks.lock().unwrap().push(entry);
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_missing_cover_file_is_repaired_without_writing_a_fact() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/aa/aa.flac", Some(ART));
    index_track(
        &service,
        "sha256:aa",
        "blobs/aa/aa.flac",
        Some("cover-art/aa.jpg"),
    );
    let facts = service.facts_count();

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.repaired = 1));
    assert_eq!(
        std::fs::read(service.music_dir.join("cover-art/aa.jpg")).unwrap(),
        ART
    );
    assert_eq!(service.facts_count(), facts, "no fact is written");
    assert_eq!(
        file_names(&service.music_dir.join("cover-art")),
        vec!["aa.jpg"],
        "no temp file is left behind"
    );
}

#[test]
fn a_present_cover_file_is_left_untouched() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/bb/bb.flac", Some(ART));
    index_track(
        &service,
        "sha256:bb",
        "blobs/bb/bb.flac",
        Some("cover-art/bb.jpg"),
    );
    std::fs::create_dir_all(service.music_dir.join("cover-art")).unwrap();
    std::fs::write(
        service.music_dir.join("cover-art/bb.jpg"),
        b"the curated one",
    )
    .unwrap();

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.present = 1));
    assert_eq!(
        std::fs::read(service.music_dir.join("cover-art/bb.jpg")).unwrap(),
        b"the curated one"
    );
}

#[test]
fn a_zero_length_cover_file_is_not_present() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/ee/ee.flac", Some(ART));
    index_track(
        &service,
        "sha256:ee",
        "blobs/ee/ee.flac",
        Some("cover-art/ee.jpg"),
    );
    std::fs::create_dir_all(service.music_dir.join("cover-art")).unwrap();
    std::fs::write(service.music_dir.join("cover-art/ee.jpg"), b"").unwrap();

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.repaired = 1));
    assert_eq!(
        std::fs::read(service.music_dir.join("cover-art/ee.jpg")).unwrap(),
        ART
    );
}

#[test]
fn a_track_without_a_cover_fact_is_extracted_and_asserted() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/cc/cc.flac", Some(ART));
    index_track(&service, "sha256:cc", "blobs/cc/cc.flac", None);
    let facts = service.facts_count();

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.extracted = 1));
    assert_eq!(
        std::fs::read(service.music_dir.join("cover-art/cc.jpg")).unwrap(),
        ART
    );
    assert_eq!(service.facts_count(), facts + 1, "one CoverArtPath fact");
}

#[test]
fn a_blob_without_art_leaves_the_fact_alone_and_is_counted() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/dd/dd.flac", None);
    index_track(
        &service,
        "sha256:dd",
        "blobs/dd/dd.flac",
        Some("cover-art/dd.jpg"),
    );
    let facts = service.facts_count();

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.no_art = 1));
    assert!(!service.music_dir.join("cover-art/dd.jpg").exists());
    assert_eq!(service.facts_count(), facts);
}

#[test]
fn an_unreadable_blob_counts_as_failed() {
    let (service, _meta, _acid) = make_empty_service();
    index_track(
        &service,
        "sha256:ff",
        "blobs/ff/missing.flac",
        Some("cover-art/ff.jpg"),
    );

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.failed = 1));
    assert!(!service.music_dir.join("cover-art/ff.jpg").exists());
}

#[test]
fn a_cover_fact_outside_the_cover_store_is_never_written_to() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/11/11.flac", Some(ART));
    index_track(
        &service,
        "sha256:11",
        "blobs/11/11.flac",
        Some("../outside.jpg"),
    );

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.failed = 1));
    assert!(!service.music_dir.join("../outside.jpg").exists());
}

#[test]
fn a_retired_track_is_skipped() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/22/22.flac", Some(ART));
    index_track(
        &service,
        "sha256:22",
        "blobs/22/22.flac",
        Some("cover-art/22.jpg"),
    );
    service.tracks.lock().unwrap()[0].retired_by = Some("sha256:winner".to_owned());

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, CoverSummary::default());
    assert!(!service.music_dir.join("cover-art/22.jpg").exists());
}

#[test]
fn tracks_sharing_a_cover_path_are_one_repair_and_the_first_blob_with_art_wins() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/33/33.flac", None);
    write_blob(&service, "blobs/44/44.flac", Some(ART));
    for (hash, blob) in [
        ("sha256:33", "blobs/33/33.flac"),
        ("sha256:44", "blobs/44/44.flac"),
    ] {
        index_track(&service, hash, blob, Some("cover-art/shared.jpg"));
    }

    let summary = service.heal_covers_with(&service.acid_client);

    assert_eq!(summary, summary_of(|s| s.repaired = 1));
    assert_eq!(
        std::fs::read(service.music_dir.join("cover-art/shared.jpg")).unwrap(),
        ART
    );
}

#[test]
fn reindex_covers_reports_the_heal_in_its_message() {
    let (service, _meta, _acid) = make_empty_service();
    write_blob(&service, "blobs/55/55.flac", Some(ART));
    index_track(
        &service,
        "sha256:55",
        "blobs/55/55.flac",
        Some("cover-art/55.jpg"),
    );

    let response = service.handle_request(LibraryRequest::ReindexCovers);

    match response {
        LibraryResponse::IngestResult(IngestResult::Success {
            hash: None,
            message,
        }) => {
            assert_eq!(
                message,
                "Cover art: 0 present, 1 repaired, 0 extracted, 0 without art, 0 failed"
            );
        }
        other => panic!("expected IngestResult::Success, got {other:?}"),
    }
}
