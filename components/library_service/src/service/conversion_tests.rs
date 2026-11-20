//! The AIFF conversion worker against the real `flac` (devenv and CI provide it) and
//! generated AIFF files (`audio-fixtures`), through a service on an in-process ACID
//! server. AIFF is not ingestible by policy yet (ADR-037 phase 8), so these services
//! accept it through their `ConversionConfig`.

use super::conversion::{Job, JobEnd};
use super::tests::{new_acid, service_over, AcidAddrs, AcidGuard};
use super::*;
use audio_fixtures::testkit::id3::{self, Id3Version};
use audio_fixtures::testkit::{AiffBuilder, Codec};
use library_ipc_protocol::{ConversionState, IngestWarning};
use lossless_encoder::Encoder;
use music_facts::{FactSource, MusicFormat};
use pretty_assertions::assert_eq;
use std::time::Duration;

fn fast_retry(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    }
}

fn config() -> ConversionConfig {
    ConversionConfig::default()
        .accepting_aiff(true)
        .with_retry(fast_retry(3))
}

fn broken_flac() -> ConversionConfig {
    config().with_encoder(Encoder::with_flac_binary("/nonexistent/flac"))
}

fn client() -> FactSource {
    FactSource::new("a-client", "1.0", "Client")
}

struct Rig {
    service: Arc<LibraryService>,
    music: tempfile::TempDir,
    addrs: AcidAddrs,
    _acid: AcidGuard,
}

impl Rig {
    fn new(config: ConversionConfig) -> Rig {
        let music = tempfile::tempdir().unwrap();
        let (acid, addrs) = new_acid();
        let service = service_over(music.path(), &addrs, config);
        Rig {
            service,
            music,
            addrs,
            _acid: acid,
        }
    }

    fn inbox(&self) -> PathBuf {
        self.music.path().join("inbox")
    }

    fn staging(&self) -> PathBuf {
        self.music.path().join("staging")
    }

    fn put(&self, name: &str, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(self.inbox()).unwrap();
        let path = self.inbox().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn ingest(&self, name: &str) -> IngestResult {
        self.ingest_with(name, Vec::new())
    }

    fn ingest_with(&self, name: &str, facts: Vec<MusicValue>) -> IngestResult {
        match self.service.handle_request(LibraryRequest::IngestFile {
            path: InboxPath::new(name).unwrap(),
            source: None,
            writer: Some(client()),
            facts,
        }) {
            LibraryResponse::IngestResult(result) => result,
            other => panic!("{other:?}"),
        }
    }

    fn conversions(&self) -> Vec<(String, ConversionState)> {
        self.service
            .conversion_status()
            .into_iter()
            .map(|c| (c.path.as_str().to_string(), c.state))
            .collect()
    }

    fn blob_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.map(|e| e.unwrap()) {
                if e.path().is_dir() {
                    walk(&e.path(), out);
                } else {
                    out.push(e.path());
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.music.path().join("blobs"), &mut out);
        out
    }
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hash_of(bytes: &[u8]) -> ContentHash {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x");
    std::fs::write(&path, bytes).unwrap();
    crate::pipeline::compute_hash(&path).unwrap()
}

fn tagged_aiff() -> Vec<u8> {
    AiffBuilder::new()
        .frames(20_000)
        .id3(id3::full_tag(Id3Version::V24))
        .build()
}

fn only_track(service: &LibraryService) -> IndexedTrackInfo {
    let tracks = service.tracks.lock().unwrap();
    assert_eq!(tracks.len(), 1);
    tracks[0].clone()
}

fn facts_of<'a>(
    track: &'a IndexedTrackInfo,
    wanted: impl Fn(&MusicValue) -> bool + 'a,
) -> impl Iterator<Item = &'a (MusicValue, FactSource)> {
    track.provenance.iter().filter(move |(v, _)| wanted(v))
}

// --- a verified conversion ---------------------------------------------------

#[test]
fn a_verified_conversion_stores_the_flac_with_the_facts_of_the_aiff_and_removes_it() {
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let source = rig.put("Whatever Name.aiff", &bytes);

    assert!(matches!(
        rig.ingest("Whatever Name.aiff"),
        IngestResult::Pending { .. }
    ));
    wait_for("the source to be removed", || !source.exists());

    let track = only_track(&rig.service);
    // The blob is the FLAC.
    assert_eq!(track.blob_path.extension().unwrap(), "flac");
    let blob = rig.music.path().join(&track.blob_path);
    assert!(blob.exists());
    assert_eq!(rig.blob_files(), vec![blob.clone()]);
    assert_eq!(
        track.content_hash,
        crate::pipeline::compute_hash(&blob).unwrap()
    );

    // The tag facts of the AIFF carry the content source (the writer's), the file
    // facts are the FLAC's under the library's.
    let (title, source_of_title) = facts_of(&track, |v| matches!(v, MusicValue::Title(_)))
        .next()
        .expect("a Title from the tags");
    assert!(
        matches!(title, MusicValue::Title(t) if t.as_str() == id3_title()),
        "{title:?}"
    );
    assert!(source_of_title.same_source(&client()));
    let formats: Vec<_> = facts_of(&track, |v| matches!(v, MusicValue::Format(_))).collect();
    assert_eq!(formats.len(), 1, "{formats:?}");
    assert_eq!(formats[0].0, MusicValue::Format(MusicFormat::Flac));
    assert!(formats[0].1.same_source(&rig.service.sources.scan));
    // (FilePath is a scalar, not a provenance entry: the blob path above is its fold.)

    // PcmHash (of the AIFF, which the FLAC equals), EncodedFrom and AudioHash.
    let source_path = rig.music.path().join("copy.aiff");
    std::fs::write(&source_path, &bytes).unwrap();
    assert_eq!(
        facts_of(&track, |v| matches!(v, MusicValue::PcmHash(_)))
            .map(|(v, _)| v.clone())
            .collect::<Vec<_>>(),
        vec![MusicValue::PcmHash(
            audio_hash::pcm_hash(&source_path).unwrap()
        )]
    );
    assert_eq!(
        facts_of(&track, |v| matches!(v, MusicValue::EncodedFrom(_)))
            .map(|(v, _)| v.clone())
            .collect::<Vec<_>>(),
        vec![MusicValue::EncodedFrom(hash_of(&bytes))]
    );
    assert_eq!(
        facts_of(&track, |v| matches!(v, MusicValue::AudioHash(_))).count(),
        1
    );

    // The index of aliases answers for the AIFF, as the fold does.
    assert_eq!(
        rig.service.known_entity(&hash_of(&bytes)),
        Some(track.content_hash.clone())
    );
    // Nothing is left in staging, and nothing waits in the worker.
    assert_eq!(staged(&rig), Vec::<String>::new());
    assert_eq!(rig.conversions(), vec![]);
}

fn id3_title() -> String {
    // The title of `id3::full_tag`, as lofty reads it back.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.aiff");
    std::fs::write(&path, tagged_aiff()).unwrap();
    let facts = crate::fact_generator::tag_facts(&path, &FactSource::new("t", "1", "T")).unwrap();
    facts
        .into_iter()
        .find_map(|(v, _)| match v {
            MusicValue::Title(t) => Some(t.as_str().to_string()),
            _ => None,
        })
        .expect("the fixture tag has a title")
}

/// The files in staging, bar the directory of the job contexts.
fn staged(rig: &Rig) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(rig.staging())
        .map(|dir| {
            dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name != "jobs")
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn the_name_facts_come_from_the_original_file_name() {
    let rig = Rig::new(config());
    // No tags: the title and artist come from the name the user supplied, not from
    // the hash-named file in staging.
    let bytes = AiffBuilder::new().frames(5_000).build();
    let source = rig.put("01 - Yagya - Empty Streets.aiff", &bytes);
    rig.ingest("01 - Yagya - Empty Streets.aiff");
    wait_for("the source to be removed", || !source.exists());

    let track = only_track(&rig.service);
    assert_eq!(track.blob_path.extension().unwrap(), "flac");
    for (wanted, what) in [
        (
            (|v: &MusicValue| matches!(v, MusicValue::Title(t) if t.as_str() == "Empty Streets"))
                as fn(&MusicValue) -> bool,
            "title",
        ),
        (
            |v| matches!(v, MusicValue::Artist(a) if a.as_str() == "Yagya"),
            "artist",
        ),
    ] {
        let fact = facts_of(&track, wanted)
            .next()
            .unwrap_or_else(|| panic!("a {what} fact from the file name: {:?}", track.provenance));
        assert!(fact.1.same_source(&client()), "{what}: {fact:?}");
    }
}

// --- dedup -------------------------------------------------------------------

#[test]
fn reingesting_a_converted_aiff_is_a_duplicate_through_the_encoded_from_index() {
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let first = rig.put("a.aiff", &bytes);
    rig.ingest("a.aiff");
    wait_for("the source to be removed", || !first.exists());
    let track = only_track(&rig.service);

    // The same bytes again: answered at once as a duplicate of the FLAC track, not
    // queued (so nothing is decoded), and the inbox file goes.
    let again = rig.put("b.aiff", &bytes);
    match rig.ingest("b.aiff") {
        IngestResult::Success { hash, .. } => assert_eq!(hash, Some(track.content_hash.clone())),
        other => panic!("expected a duplicate, got {other:?}"),
    }
    assert!(!again.exists());
    assert_eq!(rig.conversions(), vec![]);
    assert_eq!(rig.service.tracks_count(), 1);
    assert_eq!(staged(&rig), Vec::<String>::new());
}

/// The index answers what the fold does, after an ingest and after a retract of the
/// fact.
#[test]
fn the_alias_index_agrees_with_track_encoded_from_through_a_retract() {
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let source = rig.put("a.aiff", &bytes);
    rig.ingest("a.aiff");
    wait_for("the source to be removed", || !source.exists());
    let aiff_hash = hash_of(&bytes);
    let flac_hash = only_track(&rig.service).content_hash;

    let agrees = |expect: Option<&ContentHash>| {
        let by_fold =
            library_projection::track_encoded_from(&rig.service.tracks.lock().unwrap(), &aiff_hash)
                .map(|t| t.content_hash.clone());
        let by_index = rig
            .service
            .encoded_aliases
            .lock()
            .unwrap()
            .get(&hash_key(aiff_hash.as_str()))
            .cloned();
        assert_eq!(by_fold, by_index);
        assert_eq!(by_index.as_ref(), expect);
    };
    agrees(Some(&flac_hash));

    // A restart reads the same index from the log.
    let restarted = service_over(rig.music.path(), &rig.addrs, config());
    assert_eq!(restarted.known_entity(&aiff_hash), Some(flac_hash.clone()));

    rig.service
        .retract_and_project(
            &flac_hash,
            &[(
                MusicValue::EncodedFrom(aiff_hash.clone()),
                rig.service.sources.derived.clone(),
            )],
        )
        .unwrap();
    agrees(None);
    assert_eq!(rig.service.known_entity(&aiff_hash), None);
}

#[test]
fn an_aiff_in_the_inbox_at_startup_is_deduped_before_any_encode() {
    // The first service converts; a restart finds the same AIFF in the inbox again
    // (a crash after the facts, before the source was removed). Its encoder is
    // broken: only the dedup before any work can clean the file up.
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let first = rig.put("a.aiff", &bytes);
    rig.ingest("a.aiff");
    wait_for("the source to be removed", || !first.exists());

    let again = rig.put("a.aiff", &bytes);
    let restarted = service_over(rig.music.path(), &rig.addrs, broken_flac());
    wait_for("the duplicate to be removed", || !again.exists());
    assert_eq!(restarted.conversion_status(), vec![]);
}

// --- refusal and environment -------------------------------------------------

#[test]
fn a_refused_aifc_is_stored_as_an_aiff_blob() {
    let rig = Rig::new(config());
    // libFLAC 1.5.0 cannot round-trip AIFF-C (ADR-037 decision 5).
    let bytes = AiffBuilder::new().frames(3_000).aifc(Codec::Sowt).build();
    let source = rig.put("c.aiff", &bytes);
    assert!(matches!(rig.ingest("c.aiff"), IngestResult::Pending { .. }));
    wait_for("the AIFF to be stored", || {
        rig.service.tracks_count() == 1 && !source.exists()
    });

    let track = only_track(&rig.service);
    assert_eq!(track.blob_path.extension().unwrap(), "aiff");
    assert_eq!(
        std::fs::read(rig.music.path().join(&track.blob_path)).unwrap(),
        bytes,
        "the AIFF is stored unchanged"
    );
    assert_eq!(track.content_hash, hash_of(&bytes));
    assert_eq!(
        facts_of(&track, |v| matches!(v, MusicValue::EncodedFrom(_))).count(),
        0
    );
    assert_eq!(staged(&rig), Vec::<String>::new());
}

#[test]
fn a_refusal_ends_the_job_with_a_warning_naming_the_reason() {
    let rig = Rig::new(config());
    let bytes = AiffBuilder::new().frames(3_001).aifc(Codec::Fl32).build();
    let source = rig.put("f.aiff", &bytes);

    let end = rig
        .service
        .convert_job(&Job::new(source.clone(), Some(client()), Vec::new()));
    match end {
        JobEnd::Stored { hash, warnings } => {
            assert_eq!(hash, hash_of(&bytes));
            let wire = super::conversion::wire_warnings(&warnings);
            match wire.as_slice() {
                [IngestWarning::StoredAsAiff { reason }] => {
                    assert!(reason.contains("float or compressed"), "{reason}")
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_environment_error_leaves_the_file_retries_and_then_surfaces_the_error() {
    let rig = Rig::new(broken_flac());
    let bytes = AiffBuilder::new().frames(3_000).build();
    let source = rig.put("e.aiff", &bytes);
    assert!(matches!(rig.ingest("e.aiff"), IngestResult::Pending { .. }));

    wait_for("the job to give up", || {
        matches!(
            rig.conversions().as_slice(),
            [(_, ConversionState::Failed { .. })]
        )
    });
    match rig.conversions().as_slice() {
        [(path, ConversionState::Failed { message })] => {
            assert_eq!(path, "e.aiff");
            assert!(message.contains("flac"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // Never an AIFF blob because of it: the file is still in the inbox.
    assert!(source.exists());
    assert_eq!(rig.service.tracks_count(), 0);
    assert_eq!(rig.blob_files(), Vec::<PathBuf>::new());

    // The failure shows in the status.
    match rig.service.handle_request(LibraryRequest::GetStatus) {
        LibraryResponse::Status(status) => {
            assert_eq!(status.conversions.len(), 1);
            assert_eq!(status.inbox_queue_size, 1);
        }
        other => panic!("{other:?}"),
    }

    // Ingesting it again starts over.
    assert!(matches!(rig.ingest("e.aiff"), IngestResult::Pending { .. }));
}

#[test]
fn aiff_is_refused_by_the_default_policy() {
    let rig = Rig::new(ConversionConfig::default());
    rig.put("a.aiff", &AiffBuilder::new().frames(1_000).build());
    assert!(matches!(rig.ingest("a.aiff"), IngestResult::Failure { .. }));
    assert_eq!(rig.conversions(), vec![]);
}

// --- the worker ----------------------------------------------------------------

#[test]
fn the_worker_can_be_stopped_while_a_job_waits_to_retry() {
    let slow = RetryPolicy {
        max_attempts: 5,
        base_delay: Duration::from_secs(3600),
        max_delay: Duration::from_secs(3600),
    };
    let rig = Rig::new(broken_flac().with_retry(slow));
    rig.put("w.aiff", &AiffBuilder::new().frames(2_000).build());
    rig.ingest("w.aiff");

    wait_for("the job to wait for its retry", || {
        matches!(
            rig.conversions().as_slice(),
            [(_, ConversionState::Retrying { attempt: 1, .. })]
        )
    });

    let started = Instant::now();
    rig.service.shutdown_conversions();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the worker ended promptly, not after the retry delay"
    );
    // It is not running: a new job is never taken.
    rig.put("x.aiff", &AiffBuilder::new().frames(2_100).build());
    rig.ingest("x.aiff");
    std::thread::sleep(Duration::from_millis(100));
    assert!(rig
        .conversions()
        .iter()
        .any(|(p, s)| p == "x.aiff" && *s == ConversionState::Queued));
}

#[test]
fn a_source_changed_after_verification_is_kept() {
    let mut config = config();
    config.before_source_removal = Some(Box::new(|path| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"edited meanwhile").unwrap();
    }));
    let rig = Rig::new(config);
    let bytes = tagged_aiff();
    let source = rig.put("k.aiff", &bytes);
    rig.ingest("k.aiff");
    wait_for("the conversion to be stored", || {
        rig.service.tracks_count() == 1 && rig.conversions().is_empty()
    });

    assert!(source.exists(), "the changed file is not removed");
    assert_ne!(std::fs::read(&source).unwrap(), bytes);
    let track = only_track(&rig.service);
    assert_eq!(track.blob_path.extension().unwrap(), "flac");
}

// --- the retire gate -------------------------------------------------------------

/// A track with an AudioHash, an ItemId, a blob and `extra`, written through ACID.
fn seed_track(
    service: &LibraryService,
    name: &str,
    day: u32,
    extra: Vec<MusicValue>,
) -> ContentHash {
    use chrono::TimeZone;
    let hash = ContentHash::new(format!("sha256:{name}"));
    let mut facts = vec![
        MusicValue::AudioHash(music_primitives::AudioHash::from_sha256_v1([1; 32])),
        MusicValue::AddedAt(chrono::Utc.with_ymd_and_hms(2026, 1, day, 0, 0, 0).unwrap()),
        MusicValue::ItemId("p1".into()),
        MusicValue::FilePath("x.flac".into()),
    ];
    facts.extend(extra);
    let facts: Vec<_> = facts.into_iter().map(|v| (v, client())).collect();
    std::thread::sleep(Duration::from_millis(3));
    service
        .acid_client
        .write_music_facts(&hash, &facts)
        .unwrap();
    service.catch_up().unwrap();
    let blob = service
        .music_dir
        .join(blob_path_for(hash.as_str(), Path::new("x.flac")).unwrap());
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(blob, name.as_bytes()).unwrap();
    hash
}

fn is_gone(service: &LibraryService, hash: &ContentHash) -> bool {
    matches!(
        service.get_track(hash),
        Err(ProtocolError::TrackNotFound { .. })
    )
}

/// A merge that retires the entity a duplicate job found, between the lookup and the
/// write, must not be undone by the write: the loser stays retired and the facts land
/// on the winner.
#[test]
fn a_duplicate_job_does_not_resurrect_an_entity_a_merge_retired() {
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let source = rig.put("d.aiff", &bytes);
    let loser = seed_track(
        &rig.service,
        "loser0000001",
        1,
        vec![MusicValue::EncodedFrom(hash_of(&bytes))],
    );
    let winner = seed_track(&rig.service, "winner000001", 2, Vec::new());
    assert_eq!(
        rig.service.known_entity(&hash_of(&bytes)),
        Some(loser.clone())
    );

    let (w, l) = (winner.clone(), loser.clone());
    *rig.service.duplicate_pre_write_hook.lock().unwrap() = Some(Box::new(move |service, _| {
        let report = service.merge_group(&w, std::slice::from_ref(&l));
        assert_eq!(report.merged, 1, "{report:?}");
    }));

    let other = FactSource::new("other-client", "1.0", "Client");
    let end = rig.service.convert_job(&Job::new(
        source.clone(),
        Some(other),
        vec![MusicValue::ItemId("p9".into())],
    ));
    match end {
        JobEnd::Stored { hash, .. } => assert_eq!(hash, winner),
        other => panic!("{other:?}"),
    }

    assert!(
        is_gone(&rig.service, &loser),
        "the write resurrected the loser"
    );
    let held: Vec<String> = rig
        .service
        .tracks
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.content_hash == winner)
        .expect("the winner is live")
        .item_ids()
        .map(str::to_string)
        .collect();
    assert!(held.contains(&"p9".to_string()), "{held:?}");
    assert!(!source.exists());
}

/// A merge that wrote `Replaces` but failed to retract the loser leaves it live and
/// retired: a duplicate of its bytes writes to the winner, not to it.
#[test]
fn a_duplicate_of_a_retired_track_writes_to_its_winner() {
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let source = rig.put("d.aiff", &bytes);
    let loser_hash = hash_of(&bytes);
    let loser = seed_track(
        &rig.service,
        loser_hash.as_str().strip_prefix("sha256:").unwrap(),
        1,
        Vec::new(),
    );
    let winner = seed_track(&rig.service, "winner000001", 2, Vec::new());
    rig.service
        .acid_client
        .write_music_facts(&winner, &[(MusicValue::Replaces(loser.clone()), client())])
        .unwrap();
    rig.service.catch_up().unwrap();
    let is_retired = |h: &ContentHash| {
        rig.service
            .tracks
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.content_hash == *h)
            .map(|t| t.is_retired())
    };
    assert_eq!(
        is_retired(&loser),
        Some(true),
        "the loser is live but retired"
    );

    let other = FactSource::new("other-client", "1.0", "Client");
    let end = rig.service.convert_job(&Job::new(
        source.clone(),
        Some(other),
        vec![MusicValue::ItemId("p9".into())],
    ));
    match end {
        JobEnd::Stored { hash, .. } => assert_eq!(hash, winner),
        other => panic!("{other:?}"),
    }
    let holds = |h: &ContentHash| {
        rig.service
            .tracks
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.content_hash == *h)
            .map(|t| t.item_ids().any(|i| i == "p9"))
    };
    assert_eq!(holds(&winner), Some(true));
    assert_eq!(
        holds(&loser),
        Some(false),
        "nothing is written to the retired track"
    );
}

// --- the context of a queued job ---------------------------------------------------

fn jobs_dir(music: &Path) -> PathBuf {
    music.join("staging").join("jobs")
}

fn job_files(music: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(jobs_dir(music))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn the_writer_and_facts_of_a_queued_job_survive_a_restart() {
    let slow = RetryPolicy {
        max_attempts: 5,
        base_delay: Duration::from_secs(3600),
        max_delay: Duration::from_secs(3600),
    };
    let rig = Rig::new(broken_flac().with_retry(slow));
    let source = rig.put("r.aiff", &tagged_aiff());
    rig.ingest_with("r.aiff", vec![MusicValue::ItemId("p7".into())]);
    wait_for("the job to wait for its retry", || {
        matches!(
            rig.conversions().as_slice(),
            [(_, ConversionState::Retrying { .. })]
        )
    });
    rig.service.shutdown_conversions();
    assert_eq!(job_files(rig.music.path()).len(), 1);

    // The restarted service converts it, with the request's writer and ItemId.
    let restarted = service_over(rig.music.path(), &rig.addrs, config());
    wait_for("the source to be removed", || !source.exists());
    let track = only_track(&restarted);
    let item = facts_of(&track, |v| matches!(v, MusicValue::ItemId(_)))
        .next()
        .expect("the ItemId of the request");
    assert_eq!(item.0, MusicValue::ItemId("p7".into()));
    assert!(item.1.same_source(&client()));
    wait_for("the context to be removed", || {
        job_files(rig.music.path()).is_empty()
    });
}

/// A second, anonymous request for a path that is queued (ingest-all, say) must not
/// replace the context the first request kept.
#[test]
fn a_second_request_for_a_queued_file_keeps_the_first_requests_context() {
    let slow = RetryPolicy {
        max_attempts: 5,
        base_delay: Duration::from_secs(3600),
        max_delay: Duration::from_secs(3600),
    };
    let rig = Rig::new(broken_flac().with_retry(slow));
    let source = rig.put("q.aiff", &tagged_aiff());
    rig.ingest_with("q.aiff", vec![MusicValue::ItemId("p5".into())]);
    wait_for("the job to wait for its retry", || {
        matches!(
            rig.conversions().as_slice(),
            [(_, ConversionState::Retrying { .. })]
        )
    });
    let anonymous = rig.service.handle_request(LibraryRequest::IngestFile {
        path: InboxPath::new("q.aiff").unwrap(),
        source: None,
        writer: None,
        facts: Vec::new(),
    });
    assert!(matches!(
        anonymous,
        LibraryResponse::IngestResult(IngestResult::Pending { .. })
    ));
    rig.service.shutdown_conversions();

    let restarted = service_over(rig.music.path(), &rig.addrs, config());
    wait_for("the source to be removed", || !source.exists());
    let track = only_track(&restarted);
    let item = facts_of(&track, |v| matches!(v, MusicValue::ItemId(_)))
        .next()
        .expect("the ItemId of the first request");
    assert_eq!(item.0, MusicValue::ItemId("p5".into()));
    assert!(item.1.same_source(&client()));
}

#[test]
fn orphan_job_contexts_are_cleaned_and_the_sweep_leaves_the_directory() {
    let music = tempfile::tempdir().unwrap();
    let jobs = jobs_dir(music.path());
    std::fs::create_dir_all(&jobs).unwrap();
    let gone = music.path().join("inbox").join("gone.aiff");
    std::fs::write(
        jobs.join("0000000000000001.json"),
        serde_json::json!({"path": gone, "writer": null, "facts": []}).to_string(),
    )
    .unwrap();
    std::fs::write(jobs.join("0000000000000002.json"), b"not json").unwrap();

    let (_acid, addrs) = new_acid();
    let _service = service_over(music.path(), &addrs, config());
    assert_eq!(job_files(music.path()), Vec::<String>::new());
    assert!(jobs.is_dir());
}

#[test]
fn a_duplicate_job_stamps_its_tags_with_its_writer() {
    let rig = Rig::new(config());
    let bytes = tagged_aiff();
    let first = rig.put("a.aiff", &bytes);
    rig.ingest("a.aiff");
    wait_for("the source to be removed", || !first.exists());

    let again = rig.put("b.aiff", &bytes);
    let other = FactSource::new("other-client", "1.0", "Client");
    let end = rig
        .service
        .convert_job(&Job::new(again.clone(), Some(other.clone()), Vec::new()));
    assert!(matches!(end, JobEnd::Stored { .. }), "{end:?}");
    assert!(!again.exists());

    let track = only_track(&rig.service);
    assert!(
        facts_of(&track, |v| matches!(v, MusicValue::Title(_))).any(|(_, s)| s.same_source(&other)),
        "the title is also stated by the job's writer"
    );
}

// --- startup ---------------------------------------------------------------------

#[test]
fn the_startup_sweep_removes_leftovers_and_never_promotes_them() {
    let music = tempfile::tempdir().unwrap();
    let staging = music.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let hex = "ab".repeat(32);
    let leftovers = [
        format!("{hex}.1-0.encoding.flac"),
        format!("{hex}.1-0.rebuilt.aiff"),
        format!("{hex}.flac"),
    ];
    for name in &leftovers {
        // A verified-looking FLAC is still not promoted: its facts were never written.
        std::fs::write(staging.join(name), super::tests::FIXTURE_S16_L8).unwrap();
    }
    std::fs::write(staging.join("notes.txt"), b"not ours").unwrap();
    std::fs::write(staging.join("abc.flac"), b"not a hash name").unwrap();

    let (_acid, addrs) = new_acid();
    let service = service_over(music.path(), &addrs, config());

    let mut left: Vec<String> = std::fs::read_dir(&staging)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["abc.flac", "notes.txt"]);
    assert_eq!(service.tracks_count(), 0);
    assert!(!music.path().join("blobs").exists(), "nothing was imported");
}

#[test]
fn aiffs_left_in_the_inbox_are_converted_after_a_restart() {
    let music = tempfile::tempdir().unwrap();
    let inbox = music.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let source = inbox.join("left.aiff");
    std::fs::write(&source, tagged_aiff()).unwrap();
    std::fs::write(inbox.join("song.mp3"), b"not touched by the worker").unwrap();

    let (_acid, addrs) = new_acid();
    let service = service_over(music.path(), &addrs, config());
    wait_for("the source to be removed", || !source.exists());
    assert_eq!(service.tracks_count(), 1);
    assert!(inbox.join("song.mp3").exists());
}
