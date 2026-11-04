//! Tests of the duplicate scan and merge (ADR-013), against an in-process ACID server.

use super::dedup::{dedup_fact_source, live_facts};
use super::tests::{
    identity_facts_in_acid, make_empty_service, make_service_with_playlists_dir, replaces_targets,
    retire, run_backfill, user_source, write_test_wav,
};
use super::*;
use crate::ipc::{
    ConfirmationReason, DedupApplyReport, DedupDisposition, DedupGroup, IdentityBackfillStatus,
};
use chrono::{DateTime, TimeZone, Utc};
use music_facts::{
    Album, Bpm, CueKind, DurationSeconds, FactOrigin, FactSource, Key, MusicValue, StartReason,
    StopReason, Title,
};
use pretty_assertions::assert_eq;
use std::path::Path;

type Facts = Vec<(MusicValue, FactSource)>;

fn day(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, d, 0, 0, 0).unwrap()
}

fn audio(n: u8) -> MusicValue {
    MusicValue::AudioHash(music_primitives::AudioHash::from_sha256_v1([n; 32]))
}

fn pcm(n: u8) -> MusicValue {
    MusicValue::PcmHash(music_primitives::PcmHash::from_sha256_v1([n; 32]))
}

/// A source-origin (Bandcamp) fact, as ingest writes it.
fn src(v: MusicValue) -> (MusicValue, FactSource) {
    (
        v,
        FactSource::new("bandcamp-ingest", "test", FactOrigin::bandcamp(None)),
    )
}

fn usr(v: MusicValue) -> (MusicValue, FactSource) {
    (v, user_source())
}

fn album(name: &str) -> MusicValue {
    MusicValue::Album(Album::new(name))
}

fn bpm(v: f32) -> MusicValue {
    MusicValue::Bpm(Bpm::from_f32(v).unwrap())
}

fn hash(name: &str) -> ContentHash {
    ContentHash::new(format!("sha256:{name}"))
}

/// The facts of a track from one purchase: identity, item id and when it was added.
fn base(audio_n: u8, item: Option<&str>, added_day: u32) -> Facts {
    let mut f = vec![
        src(audio(audio_n)),
        src(MusicValue::AddedAt(day(added_day))),
    ];
    if let Some(item) = item {
        f.push(src(MusicValue::ItemId(item.into())));
    }
    f
}

/// Write `facts` for `name` to ACID and project them. A short sleep keeps the ACID
/// fact timestamps of consecutive writes strictly ordered.
fn write(service: &LibraryService, h: &ContentHash, facts: Facts) {
    std::thread::sleep(std::time::Duration::from_millis(3));
    service.acid_client.write_music_facts(h, &facts).unwrap();
    service.catch_up().unwrap();
}

fn seed(service: &LibraryService, name: &str, facts: Facts) -> ContentHash {
    let h = hash(name);
    write(service, &h, facts);
    h
}

fn blob_file(service: &LibraryService, h: &ContentHash) -> std::path::PathBuf {
    service
        .music_dir
        .join(blob_path_for(h.as_str(), Path::new("x.flac")).unwrap())
}

/// A track that has a blob on disk: FilePath fact plus the file itself.
fn seed_with_blob(service: &LibraryService, name: &str, mut facts: Facts) -> ContentHash {
    facts.push(src(MusicValue::FilePath("x.flac".into())));
    let h = seed(service, name, facts);
    let blob = blob_file(service, &h);
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, name.as_bytes()).unwrap();
    h
}

fn scan(service: &LibraryService) -> Vec<DedupGroup> {
    match service.handle_request(LibraryRequest::DedupScan { apply: false }) {
        LibraryResponse::DedupScan(r) => {
            assert!(r.applied.is_none());
            r.groups
        }
        other => panic!("expected DedupScan, got {other:?}"),
    }
}

fn apply(service: &LibraryService) -> (DedupApplyReport, Vec<DedupGroup>) {
    match service.handle_request(LibraryRequest::DedupScan { apply: true }) {
        LibraryResponse::DedupScan(r) => (r.applied.expect("apply report"), r.groups),
        other => panic!("expected DedupScan, got {other:?}"),
    }
}

fn hashes(group: &DedupGroup) -> Vec<String> {
    group
        .members
        .iter()
        .map(|m| m.hash.as_str().to_owned())
        .collect()
}

fn gone(service: &LibraryService, h: &ContentHash) -> bool {
    matches!(
        service.get_track(h),
        Err(ProtocolError::TrackNotFound { .. })
    )
}

fn asserted_in_acid(service: &LibraryService, h: &ContentHash) -> Vec<(MusicValue, FactSource)> {
    let lines = service.acid_client.read_entity(h.as_str()).unwrap();
    live_facts(&lines)
        .into_iter()
        .map(|f| (f.value, f.source))
        .collect()
}

fn count_in_acid(service: &LibraryService, h: &ContentHash, v: &MusicValue) -> usize {
    asserted_in_acid(service, h)
        .iter()
        .filter(|(x, _)| x == v)
        .count()
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

#[test]
fn scan_groups_tracks_with_equal_audio_hash_newest_first() {
    let (service, _d, _acid) = make_empty_service();
    let a = seed(&service, "aaa", base(1, Some("p1"), 1));
    let b = seed(&service, "bbb", base(1, Some("p1"), 2));
    seed(&service, "ccc", base(2, Some("p1"), 3));

    let groups = scan(&service);

    assert_eq!(groups.len(), 1);
    assert_eq!(hashes(&groups[0]), vec![b.to_string(), a.to_string()]);
    assert_eq!(groups[0].winner, b);
    assert_eq!(groups[0].disposition, DedupDisposition::Automatic);
    assert_eq!(groups[0].members[0].item_id.as_deref(), Some("p1"));
    assert_eq!(groups[0].members[1].added, Some(day(1).to_rfc3339()));
}

#[test]
fn scan_needs_confirmation_for_other_item_ids_missing_ids_and_soft_deleted() {
    let (service, _d, _acid) = make_empty_service();
    // 1: two purchases
    seed(&service, "a1", base(1, Some("p1"), 1));
    seed(&service, "a2", base(1, Some("p2"), 2));
    // 2: one member without an item id
    seed(&service, "b1", base(2, Some("p1"), 1));
    seed(&service, "b2", base(2, None, 2));
    // 3: one purchase, but one copy is soft-deleted
    seed(&service, "c1", base(3, Some("p1"), 1));
    let mut deleted = base(3, Some("p1"), 2);
    deleted.push(usr(MusicValue::Deleted { timestamp: day(5) }));
    seed(&service, "c2", deleted);

    let groups = scan(&service);

    assert_eq!(groups.len(), 3);
    let reasons = |winner: &str| match &groups
        .iter()
        .find(|g| g.winner == hash(winner))
        .unwrap()
        .disposition
    {
        DedupDisposition::NeedsConfirmation { reasons } => reasons.clone(),
        DedupDisposition::Automatic => panic!("{winner} must need confirmation"),
    };
    assert_eq!(reasons("a2"), vec![ConfirmationReason::DifferentItemIds]);
    assert_eq!(reasons("b2"), vec![ConfirmationReason::NoItemId]);
    assert_eq!(reasons("c2"), vec![ConfirmationReason::SoftDeleted]);
    assert!(groups.iter().all(|g| !g.is_automatic()));
}

#[test]
fn scan_lists_automatic_groups_first() {
    let (service, _d, _acid) = make_empty_service();
    seed(&service, "a1", base(1, Some("p1"), 1));
    seed(&service, "a2", base(1, Some("p2"), 2));
    seed(&service, "z1", base(2, Some("p9"), 1));
    seed(&service, "z2", base(2, Some("p9"), 2));

    let groups = scan(&service);

    assert_eq!(groups.len(), 2);
    assert!(groups[0].is_automatic(), "{groups:?}");
    assert!(!groups[1].is_automatic());
}

#[test]
fn scan_connects_groups_through_audio_and_pcm_hash() {
    let (service, _d, _acid) = make_empty_service();
    seed(&service, "a", base(1, Some("p1"), 1));
    let mut b = base(1, Some("p1"), 2);
    b.push(src(pcm(7)));
    seed(&service, "b", b);
    // Different AudioHash, but the same PcmHash as b: still one group.
    let mut c = base(2, Some("p1"), 3);
    c.push(src(pcm(7)));
    seed(&service, "c", c);

    let groups = scan(&service);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].members.len(), 3);
}

#[test]
fn audio_hash_and_pcm_hash_with_the_same_digest_are_not_equal() {
    let (service, _d, _acid) = make_empty_service();
    seed(
        &service,
        "a",
        vec![src(audio(4)), src(MusicValue::AddedAt(day(1)))],
    );
    seed(
        &service,
        "b",
        vec![src(pcm(4)), src(MusicValue::AddedAt(day(1)))],
    );
    assert!(scan(&service).is_empty());
}

#[test]
fn winner_is_latest_added_at_and_ties_go_to_the_lowest_hash() {
    let (service, _d, _acid) = make_empty_service();
    seed(&service, "bbb", base(1, Some("p1"), 5));
    seed(&service, "aaa", base(1, Some("p1"), 5));
    seed(&service, "zzz", base(1, Some("p1"), 2));

    let groups = scan(&service);

    assert_eq!(groups[0].winner, hash("aaa"));
}

// ---------------------------------------------------------------------------
// Merge: fact rules
// ---------------------------------------------------------------------------

#[test]
fn winner_keeps_its_newest_tags() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed(
        &service,
        "old",
        [base(1, Some("p1"), 1), vec![src(album("Part III & IV"))]].concat(),
    );
    let new = seed(
        &service,
        "new",
        [base(1, Some("p1"), 2), vec![src(album("Part 3 & 4"))]].concat(),
    );

    let (report, left) = apply(&service);

    assert_eq!((report.merged, report.errors.len()), (1, 0), "{report:?}");
    assert!(left.is_empty());
    assert_eq!(
        service.get_track(&new).unwrap().album.as_deref(),
        Some("Part 3 & 4")
    );
    assert!(gone(&service, &old));
}

#[test]
fn user_facts_that_exist_only_on_the_loser_are_carried() {
    let (service, _d, _acid) = make_empty_service();
    let grid = MusicValue::BeatGrid {
        first_beat_ms: 120,
        bpm: Bpm::from_f32(128.0).unwrap(),
        beats_per_bar: 4,
    };
    seed(
        &service,
        "old",
        [
            base(1, Some("p1"), 1),
            vec![
                usr(bpm(128.0)),
                usr(MusicValue::Key(Key::from_traditional("A Minor").unwrap())),
                usr(grid),
            ],
        ]
        .concat(),
    );
    let new = seed(&service, "new", base(1, Some("p1"), 2));

    apply(&service);

    let t = service.get_track(&new).unwrap();
    assert_eq!(t.bpm.map(|b| b.as_f32()), Some(128.0));
    assert!(t.key.is_some());
    assert_eq!(t.beat_grid.map(|g| g.first_beat_ms), Some(120));
    // The carried facts keep their user origin (a source retract must not touch them).
    let carried = asserted_in_acid(&service, &new);
    let bpm_fact = carried.iter().find(|(v, _)| *v == bpm(128.0)).unwrap();
    assert_eq!(bpm_fact.1, user_source());
}

#[test]
fn a_newer_user_fact_beats_the_winners_older_source_fact() {
    let (service, _d, _acid) = make_empty_service();
    let new = seed(
        &service,
        "new",
        [base(1, Some("p1"), 2), vec![src(bpm(120.0))]].concat(),
    );
    // Written later than the winner's own Bpm, although the copy is older.
    seed(
        &service,
        "old",
        [base(1, Some("p1"), 1), vec![usr(bpm(128.0))]].concat(),
    );

    apply(&service);

    assert_eq!(
        service.get_track(&new).unwrap().bpm.map(|b| b.as_f32()),
        Some(128.0)
    );
}

#[test]
fn an_older_loser_fact_does_not_override_the_winners_newer_one() {
    let (service, _d, _acid) = make_empty_service();
    seed(
        &service,
        "old",
        [base(1, Some("p1"), 1), vec![usr(bpm(128.0))]].concat(),
    );
    let new = seed(
        &service,
        "new",
        [base(1, Some("p1"), 2), vec![src(bpm(120.0))]].concat(),
    );

    apply(&service);

    assert_eq!(
        service.get_track(&new).unwrap().bpm.map(|b| b.as_f32()),
        Some(120.0)
    );
    assert_eq!(count_in_acid(&service, &new, &bpm(128.0)), 0);
}

#[test]
fn multi_valued_facts_are_united_without_duplicates() {
    let (service, _d, _acid) = make_empty_service();
    let cue = |ms| MusicValue::MemoryCue {
        position_ms: ms,
        kind: CueKind::Hot,
        label: None,
        index: None,
    };
    seed(
        &service,
        "old",
        [
            base(1, Some("p1"), 1),
            vec![
                src(MusicValue::StyleDescriptor("Deep".into())),
                src(MusicValue::StyleDescriptor("Dub".into())),
                usr(cue(1000)),
                usr(cue(2000)),
            ],
        ]
        .concat(),
    );
    let new = seed(
        &service,
        "new",
        [
            base(1, Some("p1"), 2),
            vec![
                src(MusicValue::StyleDescriptor("Deep".into())),
                usr(cue(1000)),
            ],
        ]
        .concat(),
    );

    apply(&service);

    let tracks = service.tracks.lock().unwrap();
    let t = tracks.iter().find(|t| t.content_hash == new).unwrap();
    let mut styles = t.styles.clone();
    styles.sort();
    assert_eq!(styles, vec!["Deep".to_string(), "Dub".to_string()]);
    let mut cues: Vec<u32> = t.memory_cues.iter().map(|c| c.position_ms).collect();
    cues.sort();
    assert_eq!(cues, vec![1000, 2000]);
}

#[test]
fn file_derived_facts_are_never_copied_from_the_loser() {
    let (service, _d, _acid) = make_empty_service();
    let dur = |s| MusicValue::DurationSeconds(DurationSeconds::new(s));
    let new = seed(
        &service,
        "new",
        [base(1, Some("p1"), 2), vec![src(dur(199))]].concat(),
    );
    // Written after the winner's own duration, so "latest wins" would take all of
    // these from the loser if file-derived facts were not exempt.
    seed(
        &service,
        "old",
        [
            base(1, Some("p1"), 1),
            vec![
                src(dur(200)),
                src(MusicValue::FilePath("old.flac".into())),
                src(MusicValue::EncoderSoftware("newer encoder".into())),
            ],
        ]
        .concat(),
    );

    apply(&service);

    let facts = asserted_in_acid(&service, &new);
    assert_eq!(
        facts
            .iter()
            .filter(|(v, _)| matches!(
                v,
                MusicValue::DurationSeconds(_)
                    | MusicValue::FilePath(_)
                    | MusicValue::EncoderSoftware(_)
            ))
            .count(),
        1,
        "only the winner's own duration: {facts:?}"
    );
    assert_eq!(count_in_acid(&service, &new, &dur(199)), 1);
}

#[test]
fn the_latest_value_across_a_group_of_three_wins() {
    let (service, _d, _acid) = make_empty_service();
    let w = seed(
        &service,
        "w",
        [base(1, Some("p1"), 3), vec![src(album("A"))]].concat(),
    );
    seed(
        &service,
        "l1",
        [base(1, Some("p1"), 1), vec![src(album("B"))]].concat(),
    );
    // The newest assertion of the whole group sits on the last loser.
    seed(
        &service,
        "l2",
        [base(1, Some("p1"), 2), vec![src(album("C"))]].concat(),
    );

    let (report, _) = apply(&service);

    assert_eq!(report.merged, 2, "{report:?}");
    assert_eq!(service.get_track(&w).unwrap().album.as_deref(), Some("C"));
    assert_eq!(service.tracks_count(), 1);
}

#[test]
fn the_winner_gets_the_latest_added_at_of_a_confirmed_group() {
    let (service, _d, _acid) = make_empty_service();
    let w = seed(&service, "w", base(1, Some("p1"), 1));
    let l = seed(&service, "l", base(1, Some("p9"), 9));

    // The user picks the older copy as winner.
    let resp = service.handle_request(LibraryRequest::DedupMerge {
        winner: w.clone(),
        losers: vec![l],
    });

    assert!(matches!(resp, LibraryResponse::DedupMerged(_)), "{resp:?}");
    assert_eq!(
        service.get_track(&w).unwrap().added,
        Some(day(9).to_rfc3339())
    );
}

// ---------------------------------------------------------------------------
// Merge: Replaces, playlists, idempotence
// ---------------------------------------------------------------------------

#[test]
fn winner_replaces_the_loser_and_what_the_loser_replaced() {
    let (service, _d, _acid) = make_empty_service();
    let ancient = hash("ancient");
    let old = seed(
        &service,
        "old",
        [
            base(1, Some("p1"), 1),
            vec![usr(MusicValue::Replaces(ancient.clone()))],
        ]
        .concat(),
    );
    let new = seed(&service, "new", base(1, Some("p1"), 2));

    apply(&service);

    let mut targets = replaces_targets(&service, &new);
    targets.sort();
    let mut want = vec![old.to_string(), ancient.to_string()];
    want.sort();
    assert_eq!(targets, want, "each exactly once");
    // The loser itself is asserted by the dedup tool; the inherited link keeps its source.
    let facts = asserted_in_acid(&service, &new);
    let source_of = |h: &ContentHash| {
        facts
            .iter()
            .find(|(v, _)| *v == MusicValue::Replaces(h.clone()))
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(source_of(&old).tool, "mdma-dedup");
    assert_eq!(source_of(&ancient), user_source());
    assert!(gone(&service, &old), "the loser no longer resolves");
}

#[test]
fn playlists_are_rewritten_from_the_loser_to_the_winner() {
    let (service, metadata, _acid) = make_service_with_playlists_dir();
    let old = seed(&service, "old", base(1, Some("p1"), 1));
    let new = seed(&service, "new", base(1, Some("p1"), 2));
    let plist = metadata.path().join("playlists").join("set.plist");
    std::fs::write(&plist, format!("{}  Artist - Song\n", old.as_str())).unwrap();

    apply(&service);

    assert_eq!(
        std::fs::read_to_string(plist).unwrap(),
        format!("{}  Artist - Song\n", new.as_str())
    );
}

#[test]
fn a_rerun_after_a_crash_before_the_retire_completes_without_duplicating_facts() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed(
        &service,
        "old",
        [base(1, Some("p1"), 1), vec![usr(bpm(128.0))]].concat(),
    );
    let new = seed(&service, "new", base(1, Some("p1"), 2));
    // Step 2 of the merge (carry + Replaces) is durable; the crash comes before step 3.
    let carry = service
        .plan_group_carry(&new, std::slice::from_ref(&old))
        .unwrap();
    let gate = service.retire_gate.lock().unwrap();
    service
        .write_merge_facts(&gate, &new, &old, vec![], &dedup_fact_source(), carry)
        .unwrap();
    drop(gate);
    assert_eq!(scan(&service).len(), 1, "the group is still detectable");

    let (report, _) = apply(&service);

    assert_eq!((report.merged, report.errors.len()), (1, 0), "{report:?}");
    assert_eq!(count_in_acid(&service, &new, &bpm(128.0)), 1);
    assert_eq!(
        count_in_acid(&service, &new, &MusicValue::Replaces(old.clone())),
        1
    );
    assert!(gone(&service, &old));
}

// ---------------------------------------------------------------------------
// Merge: play history follows Replaces
// ---------------------------------------------------------------------------

fn play_times(service: &LibraryService, h: &ContentHash) -> Vec<(bool, DateTime<Utc>)> {
    use stainless_facts::{Fact, Operation};
    service
        .acid_client
        .read_entity(h.as_str())
        .unwrap()
        .iter()
        .filter_map(|l| serde_json::from_str::<Fact<ContentHash, MusicValue, FactSource>>(l).ok())
        .filter(|f| f.operation() == Operation::Assert)
        .filter_map(|f| match f.value() {
            MusicValue::TrackStarted(_) => Some((true, *f.timestamp())),
            MusicValue::TrackStopped(_) => Some((false, *f.timestamp())),
            _ => None,
        })
        .collect()
}

fn played(service: &LibraryService, h: &ContentHash) -> (Option<String>, Option<String>) {
    let t = service.get_track(h).unwrap();
    (t.started, t.stopped)
}

#[test]
fn plays_recorded_on_the_loser_show_on_the_winner_with_their_original_time() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed(&service, "old", base(1, Some("p1"), 1));
    write(
        &service,
        &old,
        vec![
            src(MusicValue::TrackStarted(StartReason::OnRequest)),
            src(MusicValue::TrackStopped(StopReason::OnSkip)),
        ],
    );
    let new = seed(&service, "new", base(1, Some("p1"), 2));
    let times = play_times(&service, &old);
    let started = times.iter().find(|t| t.0).unwrap().1;
    let stopped = times.iter().find(|t| !t.0).unwrap().1;

    apply(&service);

    assert_eq!(
        played(&service, &new),
        (Some(started.to_rfc3339()), Some(stopped.to_rfc3339())),
        "the merge's own retractions must not count as plays"
    );
    // Not copied: the winner holds no play facts of its own.
    assert!(play_times(&service, &new).is_empty());
}

#[test]
fn the_latest_play_across_winner_and_loser_counts() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed(&service, "old", base(1, Some("p1"), 1));
    write(
        &service,
        &old,
        vec![src(MusicValue::TrackStarted(StartReason::OnRequest))],
    );
    let new = seed(&service, "new", base(1, Some("p1"), 2));
    write(
        &service,
        &new,
        vec![src(MusicValue::TrackStarted(StartReason::ByQueue))],
    );
    let own = play_times(&service, &new)[0].1;

    apply(&service);

    assert_eq!(played(&service, &new).0, Some(own.to_rfc3339()));
}

#[test]
fn track_replace_keeps_the_plays_of_the_replaced_track() {
    let (service, _d, _acid) = make_empty_service();
    let inbox = service.music_dir.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let old_wav = inbox.join("Old Artist - Old Song.wav");
    write_test_wav(&old_wav, 5);
    let old = service.ingest_file_internal(&old_wav, None).unwrap();
    write(
        &service,
        &old,
        vec![usr(MusicValue::TrackStarted(StartReason::OnRequest))],
    );
    let started = play_times(&service, &old)[0].1;
    let new_wav = inbox.join("New Artist - New Song.wav");
    write_test_wav(&new_wav, 7);

    let resp = service.handle_request(LibraryRequest::TrackReplace {
        old_hash: old,
        new_file_path: new_wav.to_string_lossy().to_string(),
    });

    let LibraryResponse::TrackReplaced { new_hash, .. } = resp else {
        panic!("expected TrackReplaced, got {resp:?}");
    };
    assert_eq!(played(&service, &new_hash).0, Some(started.to_rfc3339()));
}

#[test]
fn plays_follow_the_replaces_chain_through_retired_intermediates() {
    let (service, _d, _acid) = make_empty_service();
    let a = seed(&service, "a", vec![src(MusicValue::Title(Title::new("A")))]);
    write(
        &service,
        &a,
        vec![src(MusicValue::TrackStarted(StartReason::OnRequest))],
    );
    let started = play_times(&service, &a)[0].1;
    // c replaces b which replaced a; b is gone, c inherited Replaces(a).
    let c = seed(
        &service,
        "c",
        vec![
            src(MusicValue::Title(Title::new("C"))),
            src(MusicValue::Replaces(hash("b"))),
            src(MusicValue::Replaces(a.clone())),
        ],
    );
    retire(&service, &a).unwrap();

    service.refresh_event_timestamps();

    assert_eq!(played(&service, &c).0, Some(started.to_rfc3339()));
}

#[test]
fn play_events_under_short_hashes_resolve_to_the_full_hash() {
    let (service, _d, _acid) = make_empty_service();
    let full = "ab".repeat(32);
    let track = seed(
        &service,
        &full,
        vec![src(MusicValue::Title(Title::new("T")))],
    );
    for key in [&full[..12], &full[..8]] {
        let short = ContentHash::new(key);
        write(
            &service,
            &short,
            vec![src(MusicValue::TrackStarted(StartReason::OnRequest))],
        );
    }

    service.refresh_event_timestamps();

    assert!(played(&service, &track).0.is_some());
}

// ---------------------------------------------------------------------------
// Blob and cover art removal
// ---------------------------------------------------------------------------

#[test]
fn the_loser_blob_is_removed_only_after_its_facts_are_retracted() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed_with_blob(&service, "old", base(1, Some("p1"), 1));
    let new = seed_with_blob(&service, "new", base(1, Some("p1"), 2));
    let (old_blob, new_blob) = (blob_file(&service, &old), blob_file(&service, &new));
    // Inside the retire the loser's facts are read but not yet retracted: not durable.
    let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe: Box<dyn Fn() + Send + Sync> = {
        let (seen, old_blob) = (Arc::clone(&seen), old_blob.clone());
        Box::new(move || seen.store(old_blob.exists(), Ordering::SeqCst))
    };
    *service.retire_mid_hook.lock().unwrap() = Some(probe);

    let (report, _) = apply(&service);

    assert!(
        seen.load(Ordering::SeqCst),
        "blob was deleted before the retire"
    );
    assert_eq!(report.blobs_removed, 1, "{report:?}");
    assert!(!old_blob.exists());
    assert!(new_blob.exists(), "the winner's blob is kept");
}

#[test]
fn cover_art_is_removed_unless_another_live_track_uses_it() {
    let (service, _d, _acid) = make_empty_service();
    let art = |name: &str| {
        let rel = format!("cover-art/{name}.jpg");
        let abs = service.music_dir.join(&rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b"jpg").unwrap();
        (rel, abs)
    };
    let (own_rel, own_abs) = art("own");
    let (shared_rel, shared_abs) = art("shared");
    seed(
        &service,
        "lone-old",
        [
            base(1, Some("p1"), 1),
            vec![src(MusicValue::CoverArtPath(own_rel))],
        ]
        .concat(),
    );
    seed(&service, "lone-new", base(1, Some("p1"), 2));
    seed(
        &service,
        "shared-old",
        [
            base(2, Some("p2"), 1),
            vec![src(MusicValue::CoverArtPath(shared_rel.clone()))],
        ]
        .concat(),
    );
    seed(&service, "shared-new", base(2, Some("p2"), 2));
    // A third, live track on the same image.
    seed(
        &service,
        "other",
        vec![
            src(MusicValue::CoverArtPath(shared_rel)),
            src(MusicValue::Title(Title::new("O"))),
        ],
    );

    apply(&service);

    assert!(!own_abs.exists(), "unshared cover art is removed");
    assert!(shared_abs.exists(), "shared cover art is kept");
}

fn plant(service: &LibraryService, hex: &str) -> std::path::PathBuf {
    let path = service
        .music_dir
        .join("blobs")
        .join(&hex[..2])
        .join(format!("{hex}.flac"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, hex).unwrap();
    path
}

#[test]
fn orphan_cleanup_removes_only_merge_orphans_and_is_idempotent() {
    let (service, _d, _acid) = make_empty_service();
    let dedup = |v| (v, dedup_fact_source());
    let merge_orphan = plant(&service, "aa11merged");
    let plain_orphan = plant(&service, "bb22plain");
    let replace_leftover = plant(&service, "cc33replaced");
    let live_again = plant(&service, "dd44live");
    seed(
        &service,
        "winner",
        vec![
            src(MusicValue::Title(Title::new("W"))),
            dedup(MusicValue::Replaces(hash("aa11merged"))),
            // An ADR-004 replace leftover: Replaces from the user, not from dedup.
            usr(MusicValue::Replaces(hash("cc33replaced"))),
            dedup(MusicValue::Replaces(hash("dd44live"))),
        ],
    );
    // dd44live has live facts again (re-ingested), so it is not an orphan.
    seed(
        &service,
        "dd44live",
        vec![src(MusicValue::Title(Title::new("L")))],
    );

    let (first, _) = apply(&service);
    let (second, _) = apply(&service);

    assert_eq!(first.blobs_removed, 1, "{first:?}");
    assert_eq!(second.blobs_removed, 0);
    assert!(!merge_orphan.exists());
    assert!(plain_orphan.exists(), "other NoLiveFacts orphans stay");
    assert!(replace_leftover.exists(), "ADR-004 leftovers stay");
    assert!(live_again.exists(), "a live track's blob stays");
}

// ---------------------------------------------------------------------------
// Confirmed merges
// ---------------------------------------------------------------------------

#[test]
fn a_confirmed_merge_accepts_short_hashes_and_merges_across_item_ids() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed_with_blob(&service, "aaaa1", base(1, Some("p1"), 1));
    let new = seed_with_blob(&service, "bbbb1", base(1, Some("p2"), 2));
    assert!(!scan(&service)[0].is_automatic());

    let resp = service.handle_request(LibraryRequest::DedupMerge {
        winner: ContentHash::new("bbbb"),
        losers: vec![ContentHash::new("aaaa")],
    });

    let LibraryResponse::DedupMerged(report) = resp else {
        panic!("expected DedupMerged, got {resp:?}");
    };
    assert_eq!((report.merged, report.blobs_removed), (1, 1), "{report:?}");
    assert!(gone(&service, &old));
    assert!(service.get_track(&new).is_ok());
}

#[test]
fn a_confirmed_merge_of_tracks_that_are_not_duplicates_is_refused() {
    let (service, _d, _acid) = make_empty_service();
    let a = seed_with_blob(&service, "aaaa1", base(1, Some("p1"), 1));
    let b = seed_with_blob(&service, "bbbb1", base(2, Some("p1"), 2));

    let resp = service.handle_request(LibraryRequest::DedupMerge {
        winner: b.clone(),
        losers: vec![a.clone()],
    });

    assert!(matches!(resp, LibraryResponse::Error(_)), "{resp:?}");
    assert!(service.get_track(&a).is_ok());
    assert!(blob_file(&service, &a).exists());
}

#[test]
fn a_confirmed_merge_needs_at_least_one_distinct_loser() {
    let (service, _d, _acid) = make_empty_service();
    let a = seed(&service, "aaaa1", base(1, Some("p1"), 1));
    seed(&service, "bbbb1", base(1, Some("p1"), 2));
    for losers in [vec![], vec![a.clone()]] {
        let resp = service.handle_request(LibraryRequest::DedupMerge {
            winner: a.clone(),
            losers,
        });
        assert!(matches!(resp, LibraryResponse::Error(_)), "{resp:?}");
    }
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// A backfill write must not land inside a merge's retire and resurrect the loser.
#[test]
fn a_concurrent_identity_backfill_does_not_resurrect_a_merged_loser() {
    let (service, _d, _acid) = make_empty_service();
    // The loser has a real blob and no identity facts yet, so the backfill plans it.
    let loser = {
        let h = hash("loser0000001");
        write(
            &service,
            &h,
            vec![
                src(MusicValue::Title(Title::new("L"))),
                src(MusicValue::FilePath("x.flac".into())),
            ],
        );
        let blob = blob_file(&service, &h);
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(
            blob,
            include_bytes!("../../../audio_hash/testdata/pcm/fixtures/s16_l8.flac"),
        )
        .unwrap();
        h
    };
    let winner = seed(&service, "winner000001", base(1, Some("p1"), 2));

    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    *service.retire_mid_hook.lock().unwrap() = Some(Box::new(move || {
        parked_tx.send(()).unwrap();
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30));
    }));

    let merge = {
        let (service, winner, loser) = (Arc::clone(&service), winner.clone(), loser.clone());
        std::thread::spawn(move || service.merge_group(&winner, &[loser]))
    };
    // The merge has read the loser and is parked.
    parked_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the merge never reached its retire");

    let started = service.handle_request(LibraryRequest::IdentityBackfill { include_pcm: false });
    assert!(matches!(
        started,
        LibraryResponse::IdentityBackfillStarted(_)
    ));
    std::thread::sleep(std::time::Duration::from_millis(300));
    release_tx.send(()).unwrap();
    let report = merge.join().unwrap();
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    while matches!(
        service.identity_backfill_status(),
        IdentityBackfillStatus::Running(_)
    ) {
        assert!(Instant::now() < deadline, "backfill did not finish");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    assert_eq!(report.merged, 1, "{report:?}");
    assert!(gone(&service, &loser), "the backfill resurrected the loser");
    assert!(identity_facts_in_acid(&service, &loser).is_empty());
}

#[test]
fn a_backfill_run_after_a_merge_leaves_the_winner_alone() {
    let (service, _d, _acid) = make_empty_service();
    seed(&service, "old", base(1, Some("p1"), 1));
    let new = seed(&service, "new", base(1, Some("p1"), 2));
    apply(&service);
    let status = run_backfill(&service, false);
    assert_eq!(status.failed, 0);
    assert!(service.get_track(&new).is_ok());
}

/// The merge checks both tracks are live under the gate, so a winner retired after
/// the merge was planned is refused instead of being resurrected by `Replaces`.
#[test]
fn a_merge_refuses_a_winner_retired_after_it_was_planned() {
    let (service, _d, _acid) = make_empty_service();
    let loser = seed_with_blob(&service, "loser0000001", base(1, Some("p1"), 1));
    let winner = seed_with_blob(&service, "winner000001", base(1, Some("p1"), 2));

    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    *service.merge_pre_write_hook.lock().unwrap() = Some(Box::new(move || {
        let _ = parked_tx.send(());
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30));
    }));

    let merge = {
        let (service, winner, loser) = (Arc::clone(&service), winner.clone(), loser.clone());
        std::thread::spawn(move || service.merge_group(&winner, &[loser]))
    };
    parked_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the merge never reached its write");
    retire(&service, &winner).unwrap();
    release_tx.send(()).unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(merge.join().unwrap());
    });
    let report = done_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the merge never finished");

    assert_eq!((report.merged, report.errors.len()), (0, 1), "{report:?}");
    assert!(gone(&service, &winner), "the merge resurrected the winner");
    assert!(service.get_track(&loser).is_ok(), "the loser is kept");
    assert!(blob_file(&service, &loser).exists());
}

// ---------------------------------------------------------------------------
// Carry rules and orphan cleanup, regression
// ---------------------------------------------------------------------------

#[test]
fn a_confirmed_merge_keeps_every_purchase_known_to_the_source() {
    let (service, _d, _acid) = make_empty_service();
    // The winner's own ItemId is the newer assertion, so a scalar rule would not
    // carry the loser's.
    let l = seed(&service, "l", base(1, Some("p2"), 1));
    let w = seed(&service, "w", base(1, Some("p1"), 2));

    let resp = service.handle_request(LibraryRequest::DedupMerge {
        winner: w,
        losers: vec![l],
    });

    assert!(matches!(resp, LibraryResponse::DedupMerged(_)), "{resp:?}");
    let resp = service.handle_request(LibraryRequest::HasFacts {
        fact_type: FactType::new("ItemId"),
        values: vec!["p1".into(), "p2".into()],
    });
    let LibraryResponse::FactsExist { mut existing, .. } = resp else {
        panic!("expected FactsExist, got {resp:?}");
    };
    existing.sort();
    assert_eq!(existing, vec!["p1".to_string(), "p2".to_string()]);
}

/// Two purchases of one file, merged by confirmation: the winner (p1) now also holds
/// p2, and the carried p2 is the newest ItemId fact, so the display scalar shows p2.
fn merged_across_item_ids(service: &LibraryService) -> ContentHash {
    let l = seed(service, "l", base(1, Some("p2"), 1));
    let w = seed(
        service,
        "w",
        [base(1, Some("p1"), 2), vec![src(album("Album One"))]].concat(),
    );
    let resp = service.handle_request(LibraryRequest::DedupMerge {
        winner: w.clone(),
        losers: vec![l],
    });
    assert!(matches!(resp, LibraryResponse::DedupMerged(_)), "{resp:?}");
    w
}

fn track_count(service: &LibraryService, item_id: &str) -> usize {
    match service.handle_request(LibraryRequest::GetTrackCountForItemId {
        item_id: item_id.into(),
    }) {
        LibraryResponse::TrackCountForItemId(n) => n,
        other => panic!("expected TrackCountForItemId, got {other:?}"),
    }
}

#[test]
fn a_merged_track_counts_under_every_item_id_it_holds() {
    let (service, _d, _acid) = make_empty_service();
    merged_across_item_ids(&service);

    assert_eq!(track_count(&service, "p1"), 1);
    assert_eq!(track_count(&service, "p2"), 1);
}

#[test]
fn a_fresh_copy_of_a_purchase_is_automatic_after_a_cross_item_id_merge() {
    let (service, _d, _acid) = make_empty_service();
    let w = merged_across_item_ids(&service);
    let fresh = seed(&service, "fresh", base(1, Some("p1"), 3));

    let groups = scan(&service);

    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(
        groups[0].disposition,
        DedupDisposition::Automatic,
        "{groups:?}"
    );
    assert_eq!(groups[0].winner, fresh);
    assert!(hashes(&groups[0]).contains(&w.to_string()));
}

#[test]
fn a_group_without_a_shared_item_id_needs_confirmation_even_if_ids_overlap_pairwise() {
    let (service, _d, _acid) = make_empty_service();
    // a holds {p1, p2}, b holds {p2, p3}, c holds {p3}: no id is common to all.
    seed(
        &service,
        "a",
        [
            base(1, Some("p1"), 1),
            vec![src(MusicValue::ItemId("p2".into()))],
        ]
        .concat(),
    );
    seed(
        &service,
        "b",
        [
            base(1, Some("p2"), 2),
            vec![src(MusicValue::ItemId("p3".into()))],
        ]
        .concat(),
    );
    seed(&service, "c", base(1, Some("p3"), 3));

    let groups = scan(&service);

    assert_eq!(
        groups[0].disposition,
        DedupDisposition::NeedsConfirmation {
            reasons: vec![ConfirmationReason::DifferentItemIds]
        }
    );
}

#[test]
fn retract_source_facts_still_finds_a_track_merged_under_another_item_id() {
    let (service, _d, _acid) = make_empty_service();
    let w = merged_across_item_ids(&service);
    assert_eq!(
        service.get_track(&w).unwrap().album.as_deref(),
        Some("Album One")
    );

    let resp = service.handle_request(LibraryRequest::RetractSourceFacts {
        item_id: "p1".into(),
        source_name: "bandcamp".into(),
    });

    assert!(
        matches!(resp, LibraryResponse::SourceFactsRetracted),
        "{resp:?}"
    );
    assert_eq!(
        service.get_track(&w).unwrap().album,
        None,
        "the resync missed the track"
    );
}

// ---------------------------------------------------------------------------
// Overlapping merges
// ---------------------------------------------------------------------------

/// Merge A into B parks inside its retire, holding the gate. Merge B into A must wait
/// on the gate, and once released it finds its loser retired: one merge wins.
#[test]
fn overlapping_merges_of_the_same_pair_leave_exactly_one_track() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::time::Duration;

    let (service, _d, _acid) = make_empty_service();
    let a = seed_with_blob(&service, "aaaa0000001", base(1, Some("p1"), 1));
    let b = seed_with_blob(&service, "bbbb0000001", base(1, Some("p1"), 2));

    let (parked_tx, parked_rx) = channel::<()>();
    let (release_tx, release_rx) = channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let first = AtomicBool::new(true);
    *service.retire_mid_hook.lock().unwrap() = Some(Box::new(move || {
        // Only the first retire parks.
        if first.swap(false, Ordering::SeqCst) {
            let _ = parked_tx.send(());
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30));
        }
    }));

    let spawn_merge = |winner: &ContentHash, loser: &ContentHash| {
        let (service, winner, loser) = (Arc::clone(&service), winner.clone(), loser.clone());
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(service.merge_group(&winner, &[loser]));
        });
        rx
    };

    let a_into_b = spawn_merge(&b, &a);
    parked_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("merge A into B never reached its retire");
    let b_into_a = spawn_merge(&a, &b);
    assert!(
        matches!(
            b_into_a.recv_timeout(Duration::from_millis(500)),
            Err(RecvTimeoutError::Timeout)
        ),
        "merge B into A got past the gate"
    );

    release_tx.send(()).unwrap();
    let first = a_into_b
        .recv_timeout(Duration::from_secs(30))
        .expect("merge A into B never finished");
    let second = b_into_a
        .recv_timeout(Duration::from_secs(30))
        .expect("merge B into A never finished");

    assert_eq!((first.merged, first.errors.len()), (1, 0), "{first:?}");
    assert_eq!((second.merged, second.errors.len()), (0, 1), "{second:?}");
    let live = [&a, &b]
        .iter()
        .filter(|h| service.get_track(h).is_ok())
        .count();
    assert_eq!(live, 1, "exactly one of the pair is live");
    assert!(service.get_track(&b).is_ok());
    let blobs = [&a, &b]
        .iter()
        .filter(|h| blob_file(&service, h).exists())
        .count();
    assert_eq!(blobs, 1, "exactly one blob remains");
}

#[test]
fn a_user_replaces_the_loser_carried_is_not_a_merge_orphan() {
    let (service, _d, _acid) = make_empty_service();
    let x = plant(&service, "ee55replacedbyuser");
    let loser = seed(
        &service,
        "loser",
        [
            base(1, Some("p1"), 1),
            vec![usr(MusicValue::Replaces(hash("ee55replacedbyuser")))],
        ]
        .concat(),
    );
    let winner = seed(&service, "winner", base(1, Some("p1"), 2));

    let (report, _) = apply(&service);
    let (second, _) = apply(&service);

    assert_eq!((report.merged, report.errors.len()), (1, 0), "{report:?}");
    assert!(gone(&service, &loser));
    assert_eq!(
        count_in_acid(
            &service,
            &winner,
            &MusicValue::Replaces(hash("ee55replacedbyuser"))
        ),
        1,
        "the user's link is inherited"
    );
    assert_eq!(second.blobs_removed, 0);
    assert!(
        x.exists(),
        "an ADR-004 replaced blob is not the dedup cleanup's business"
    );
}

// ---------------------------------------------------------------------------
// Short-hash play events
// ---------------------------------------------------------------------------

#[test]
fn a_short_hash_shared_by_two_tracks_belongs_to_neither() {
    let (service, _d, _acid) = make_empty_service();
    let a = seed(
        &service,
        &format!("abcdef01{}", "a".repeat(56)),
        vec![src(MusicValue::Title(Title::new("A")))],
    );
    let b = seed(
        &service,
        &format!("abcdef01{}", "b".repeat(56)),
        vec![src(MusicValue::Title(Title::new("B")))],
    );
    write(
        &service,
        &ContentHash::new("abcdef01"),
        vec![src(MusicValue::TrackStarted(StartReason::OnRequest))],
    );

    service.refresh_event_timestamps();

    assert_eq!(played(&service, &a).0, None);
    assert_eq!(played(&service, &b).0, None);
}

// ---------------------------------------------------------------------------
// Tagliabue: one purchase, re-downloaded after the prerelease became the release
// ---------------------------------------------------------------------------

#[test]
fn tagliabue_prerelease_and_release_merge_into_one_track() {
    let (service, _d, _acid) = make_empty_service();
    let prerelease = seed_with_blob(
        &service,
        "prerelease",
        [
            base(1, Some("p387309426"), 10),
            vec![
                src(album("Espansione - Part III & IV")),
                src(MusicValue::Title(Title::new("Espansione"))),
                usr(bpm(122.0)),
            ],
        ]
        .concat(),
    );
    let release = seed_with_blob(
        &service,
        "release",
        [
            base(1, Some("p387309426"), 20),
            vec![
                src(album("Espansione - Part 3 & 4")),
                src(MusicValue::Title(Title::new("Espansione"))),
            ],
        ]
        .concat(),
    );

    let (report, left) = apply(&service);

    assert_eq!(
        (report.merged, report.blobs_removed, report.errors.len()),
        (1, 1, 0),
        "{report:?}"
    );
    assert!(left.is_empty());
    let tracks = service.list_tracks(None);
    assert_eq!(tracks.len(), 1);
    let t = &tracks[0];
    assert_eq!(t.content_hash, release);
    assert_eq!(t.album.as_deref(), Some("Espansione - Part 3 & 4"));
    assert_eq!(t.bpm.map(|b| b.as_f32()), Some(122.0));
    assert_eq!(t.added, Some(day(20).to_rfc3339()));
    assert!(gone(&service, &prerelease));
    assert!(!blob_file(&service, &prerelease).exists());
    assert!(blob_file(&service, &release).exists());
}
