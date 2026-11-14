//! Tests of the duplicate scan and merge (ADR-013), against an in-process ACID server.

use super::dedup::{live_facts, same_member};
use super::tests::{
    identity_facts_in_acid, make_empty_service, make_service_with_playlists_dir, replaces_targets,
    retire, run_backfill, test_sources, user_source, write_test_wav, FIXTURE_S16_L8,
    FIXTURE_S16_L8_TAGS,
};
use super::*;
use crate::ipc::{
    ArchiveEmptyReport, ArchiveListing, ConfirmationReason, DedupApplyReport, DedupDisposition,
    DedupGroup, IdentityBackfillStatus,
};
use chrono::{DateTime, TimeZone, Utc};
use music_facts::{
    Album, Bpm, CueKind, DurationSeconds, FactSource, Key, MusicValue, StartReason, StopReason,
    Title,
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

/// The source of a client that ingests (and resyncs) what it downloads.
fn client_source() -> FactSource {
    FactSource::new("test-client", "test", "Client")
}

/// A client-source fact, as an ingest with a writer writes it.
fn src(v: MusicValue) -> (MusicValue, FactSource) {
    (v, client_source())
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
fn bookmarks_are_the_same_member_when_they_carry_the_same_mark() {
    let bm = |scope: Option<&str>, d| MusicValue::Bookmarked {
        scope: scope.map(str::to_owned),
        timestamp: day(d),
    };
    assert!(same_member(&bm(Some("Banger"), 1), &bm(Some("banger"), 2)));
    assert!(same_member(&bm(None, 1), &bm(Some("liked"), 2)));
    assert!(!same_member(&bm(None, 1), &bm(Some("banger"), 1)));
    // Legacy scopes compare by raw text.
    assert!(same_member(&bm(Some(""), 1), &bm(Some(""), 2)));
    assert!(!same_member(&bm(Some(""), 1), &bm(Some("  "), 1)));
}

#[test]
fn live_facts_retract_cancels_every_equal_value_of_the_same_source_only() {
    let h = hash("live_facts_retract");
    let line = |source: FactSource, op| {
        serde_json::to_string(&stainless_facts::Fact::new(
            h.clone(),
            album("X"),
            day(1),
            source,
            op,
        ))
        .unwrap()
    };
    // s1 and s3 are two versions of one source, s2 is another source.
    let s1 = FactSource::new("tool-1", "0.1.0", "Unknown");
    let s2 = FactSource::new("tool-2", "1.0.0", "Unknown");
    let s3 = FactSource::new("tool-1", "9.9.9", "Unknown");
    let other = serde_json::to_string(&stainless_facts::Fact::new(
        h.clone(),
        album("Y"),
        day(1),
        s1.clone(),
        stainless_facts::Operation::Assert,
    ))
    .unwrap();

    let lines = vec![
        line(s1.clone(), stainless_facts::Operation::Assert),
        line(s1.clone(), stainless_facts::Operation::Assert),
        line(s2.clone(), stainless_facts::Operation::Assert),
        other,
        line(s3, stainless_facts::Operation::Retract),
    ];
    let live: Vec<(MusicValue, FactSource)> = live_facts(&lines)
        .into_iter()
        .map(|l| (l.value, l.source))
        .collect();
    // Both tool-1 assertions of X are gone; tool-2's X and tool-1's Y stay.
    assert_eq!(live, vec![(album("X"), s2), (album("Y"), s1)]);
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
        .write_merge_facts(&gate, &new, &old, vec![], &test_sources().dedup, carry)
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
    let old = service.ingest_file_internal(&old_wav, None, &[]).unwrap();
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
    assert_eq!(report.blobs_archived, 1, "{report:?}");
    assert!(!old_blob.exists());
    assert_eq!(
        std::fs::read(archived(&service, &old_blob)).unwrap(),
        b"old",
        "the loser's blob is moved, bytes intact"
    );
    assert!(new_blob.exists(), "the winner's blob is kept");
    assert!(!archived(&service, &new_blob).exists());
}

/// Where the archive keeps the file at `abs` (under `music_dir`).
fn archived(service: &LibraryService, abs: &Path) -> std::path::PathBuf {
    service
        .music_dir
        .join("archive")
        .join(abs.strip_prefix(&service.music_dir).unwrap())
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

    assert!(!own_abs.exists(), "unshared cover art leaves cover-art/");
    assert_eq!(
        std::fs::read(archived(&service, &own_abs)).unwrap(),
        b"jpg",
        "unshared cover art is archived"
    );
    assert!(shared_abs.exists(), "shared cover art is kept");
    assert!(!archived(&service, &shared_abs).exists());
}

fn archive_listing(service: &LibraryService) -> ArchiveListing {
    match service.handle_request(LibraryRequest::ArchiveList) {
        LibraryResponse::ArchiveListing(l) => l,
        other => panic!("expected ArchiveListing, got {other:?}"),
    }
}

fn empty_archive(service: &LibraryService) -> ArchiveEmptyReport {
    empty_archive_up_to(service, None)
}

fn empty_archive_up_to(service: &LibraryService, up_to: Option<String>) -> ArchiveEmptyReport {
    match service.handle_request(LibraryRequest::ArchiveEmpty { up_to }) {
        LibraryResponse::ArchiveEmptied(r) => r,
        other => panic!("expected ArchiveEmptied, got {other:?}"),
    }
}

fn set_mtime(path: &Path, when: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn a_name_collision_in_the_archive_keeps_both_files() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed_with_blob(&service, "old", base(1, Some("p1"), 1));
    seed_with_blob(&service, "new", base(1, Some("p1"), 2));
    let earlier = archived(&service, &blob_file(&service, &old));
    std::fs::create_dir_all(earlier.parent().unwrap()).unwrap();
    std::fs::write(&earlier, b"earlier").unwrap();

    let (report, _) = apply(&service);

    assert_eq!(
        (report.blobs_archived, report.errors.len()),
        (1, 0),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read(&earlier).unwrap(),
        b"earlier",
        "never overwritten"
    );
    let listing = archive_listing(&service);
    let mut contents: Vec<Vec<u8>> = listing
        .entries
        .iter()
        .map(|e| std::fs::read(service.music_dir.join("archive").join(&e.path)).unwrap())
        .collect();
    contents.sort();
    assert_eq!(contents, vec![b"earlier".to_vec(), b"old".to_vec()]);
    assert!(
        listing.entries.iter().all(|e| e.hash == "old"),
        "{listing:?}"
    );
}

#[test]
fn the_archive_lists_hash_size_and_the_time_it_was_archived() {
    let (service, _d, _acid) = make_empty_service();
    assert_eq!(archive_listing(&service), ArchiveListing::default());
    let old = seed_with_blob(&service, "old", base(1, Some("p1"), 1));
    seed_with_blob(&service, "new", base(1, Some("p1"), 2));
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 30);
    set_mtime(&blob_file(&service, &old), long_ago);

    apply(&service);

    let listing = archive_listing(&service);
    assert_eq!(listing.entries.len(), 1, "{listing:?}");
    assert_eq!(listing.total_bytes, 3);
    let e = &listing.entries[0];
    assert_eq!((e.hash.as_str(), e.size_bytes), ("old", 3));
    assert!(e.path.starts_with("blobs/"), "{e:?}");
    let at = DateTime::parse_from_rfc3339(e.archived_at.as_deref().unwrap()).unwrap();
    assert!(
        Utc::now().signed_duration_since(at).num_seconds() < 60,
        "archived time is when it was moved, not the file's old mtime: {e:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_file_archived_after_the_cutoff_is_never_deleted_even_with_an_old_mtime() {
    use std::os::unix::fs::PermissionsExt;
    let (service, _d, _acid) = make_empty_service();
    let old = seed_with_blob(&service, "old", base(1, Some("p1"), 1));
    seed_with_blob(&service, "new", base(1, Some("p1"), 2));
    let src = blob_file(&service, &old);
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 30);
    set_mtime(&src, long_ago);
    // A read-only source: the archived time must still be stamped.
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
    let cutoff = (Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();

    apply(&service);
    assert!(archived(&service, &src).exists());

    let report = empty_archive_up_to(&service, Some(cutoff));

    assert_eq!(report.files_deleted, 0, "{report:?}");
    assert!(
        archived(&service, &src).exists(),
        "archived after the cutoff"
    );
}

#[test]
fn emptying_the_archive_deletes_only_the_archive() {
    let (service, _d, _acid) = make_empty_service();
    let old = seed_with_blob(&service, "old", base(1, Some("p1"), 1));
    let new = seed_with_blob(&service, "new", base(1, Some("p1"), 2));
    let other = |rel: &str| {
        let p = service.music_dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"keep").unwrap();
        p
    };
    let (inbox, art, archive_like) = (
        other("inbox/song.flac"),
        other("cover-art/live.jpg"),
        other("archive-not/x.flac"),
    );
    apply(&service);
    assert!(archived(&service, &blob_file(&service, &old)).exists());

    let report = empty_archive(&service);

    assert_eq!(
        (report.files_deleted, report.bytes_freed),
        (1, 3),
        "{report:?}"
    );
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(archive_listing(&service), ArchiveListing::default());
    assert!(!archived(&service, &blob_file(&service, &old)).exists());
    assert!(blob_file(&service, &new).exists(), "live blobs stay");
    assert!(inbox.exists() && art.exists() && archive_like.exists());
    // Idempotent, and nothing to do on a library that never archived.
    assert_eq!(empty_archive(&service).files_deleted, 0);
}

#[cfg(unix)]
#[test]
fn emptying_the_archive_never_follows_a_symlink_out_of_it() {
    let (service, _d, _acid) = make_empty_service();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.flac");
    std::fs::write(&victim, b"precious").unwrap();
    let archive = service.music_dir.join("archive");
    std::fs::create_dir_all(archive.join("blobs")).unwrap();
    std::os::unix::fs::symlink(outside.path(), archive.join("blobs/escape")).unwrap();
    std::os::unix::fs::symlink(&victim, archive.join("blobs/link.flac")).unwrap();

    let report = empty_archive(&service);

    assert!(
        victim.exists(),
        "a symlink target outside the archive survives"
    );
    assert!(outside.path().exists());
    assert!(report.errors.is_empty(), "{report:?}");
    for gone in ["blobs/escape", "blobs/link.flac"] {
        assert!(
            archive.join(gone).symlink_metadata().is_err(),
            "{gone} removed"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_archive_root_is_never_walked_or_emptied() {
    let (service, _d, _acid) = make_empty_service();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.flac");
    std::fs::write(&victim, b"precious").unwrap();
    std::fs::create_dir_all(&service.music_dir).unwrap();
    std::os::unix::fs::symlink(outside.path(), service.music_dir.join("archive")).unwrap();

    assert_eq!(
        archive_listing(&service),
        ArchiveListing::default(),
        "nothing is listed through the link"
    );
    let report = empty_archive(&service);

    assert!(victim.exists(), "the link's target is untouched");
    assert_eq!(report.files_deleted, 0, "{report:?}");
    assert!(
        !report.errors.is_empty(),
        "the refusal is reported: {report:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_archive_directory_is_reported_not_hidden() {
    use std::os::unix::fs::PermissionsExt;
    let (service, _d, _acid) = make_empty_service();
    let locked = service.music_dir.join("archive/blobs");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("a.flac"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let readable_anyway = std::fs::read_dir(&locked).is_ok();
    let report = empty_archive(&service);

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !readable_anyway {
        assert!(
            !report.errors.is_empty(),
            "a skipped subtree is reported: {report:?}"
        );
        assert!(locked.join("a.flac").exists());
    }
}

#[test]
fn a_confirmed_emptying_deletes_only_what_was_listed() {
    let (service, _d, _acid) = make_empty_service();
    let archive = service.music_dir.join("archive/blobs");
    std::fs::create_dir_all(&archive).unwrap();
    let listed_at = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let later = listed_at + std::time::Duration::from_secs(60);
    for (name, at) in [("listed.flac", listed_at), ("later.flac", later)] {
        std::fs::write(archive.join(name), name).unwrap();
        set_mtime(&archive.join(name), at);
    }
    let cutoff = archive_listing(&service)
        .entries
        .iter()
        .find(|e| e.path.ends_with("listed.flac"))
        .and_then(|e| e.archived_at.clone())
        .unwrap();

    let report = empty_archive_up_to(&service, Some(cutoff));

    assert_eq!(report.files_deleted, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(!archive.join("listed.flac").exists());
    assert!(
        archive.join("later.flac").exists(),
        "archived after the cutoff"
    );
    assert_eq!(empty_archive_up_to(&service, None).files_deleted, 1);
    assert!(!archive.join("later.flac").exists());

    let refused = empty_archive_up_to(&service, Some("yesterday".into()));
    assert!(!refused.errors.is_empty(), "{refused:?}");
}

#[test]
fn a_loser_that_comes_back_before_archiving_keeps_its_files() {
    let (service, _d, _acid) = make_empty_service();
    let art_rel = "cover-art/old.jpg".to_owned();
    let art = service.music_dir.join(&art_rel);
    std::fs::create_dir_all(art.parent().unwrap()).unwrap();
    std::fs::write(&art, b"jpg").unwrap();
    let old_facts = || {
        [
            base(1, Some("p1"), 1),
            vec![
                src(MusicValue::FilePath("x.flac".into())),
                src(MusicValue::CoverArtPath(art_rel.clone())),
            ],
        ]
        .concat()
    };
    let old = seed_with_blob(&service, "old", old_facts());
    let new = seed_with_blob(&service, "new", base(1, Some("p1"), 2));
    // The loser's bytes are ingested again between the merge and the archiving.
    *service.merge_pre_archive_hook.lock().unwrap() = Some(Box::new({
        let facts = old_facts();
        move |service, loser| write(service, loser, facts.clone())
    }));

    let (report, _) = apply(&service);

    assert_eq!(report.merged, 1, "{report:?}");
    assert_eq!(report.blobs_archived, 0, "{report:?}");
    assert!(blob_file(&service, &old).exists(), "the revived blob stays");
    assert!(art.exists(), "the revived cover art stays");
    assert!(!archived(&service, &blob_file(&service, &old)).exists());
    assert!(blob_file(&service, &new).exists());
}

/// A cover-art file `cover-art/<name>.jpg` under `music_dir`; returns its relative path.
fn cover_file(service: &LibraryService, name: &str) -> String {
    let rel = format!("cover-art/{name}.jpg");
    let abs = service.music_dir.join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, b"jpg").unwrap();
    rel
}

#[test]
fn cover_art_that_the_album_cache_hands_to_other_tracks_is_kept() {
    let (service, _d, _acid) = make_empty_service();
    let rel = cover_file(&service, "album");
    seed(
        &service,
        "old",
        [
            base(1, Some("p1"), 1),
            vec![src(album("A")), src(MusicValue::CoverArtPath(rel.clone()))],
        ]
        .concat(),
    );
    seed(
        &service,
        "new",
        [base(1, Some("p1"), 2), vec![src(album("A"))]].concat(),
    );
    // As built at startup: the album's cover is the old copy's.
    service
        .album_cover_cache
        .lock()
        .unwrap()
        .insert("A".into(), rel.clone());

    apply(&service);

    assert!(
        service.music_dir.join(&rel).exists(),
        "still shown for the album"
    );
    assert!(!archived(&service, &service.music_dir.join(&rel)).exists());
    assert_eq!(
        service.album_cover_cache.lock().unwrap().get("A"),
        Some(&rel)
    );
}

#[test]
fn an_album_cache_entry_is_repointed_when_its_cover_is_archived() {
    let (service, _d, _acid) = make_empty_service();
    let (old_rel, new_rel) = (cover_file(&service, "old"), cover_file(&service, "new"));
    seed(
        &service,
        "old",
        [
            base(1, Some("p1"), 1),
            vec![
                src(album("A")),
                src(MusicValue::CoverArtPath(old_rel.clone())),
            ],
        ]
        .concat(),
    );
    seed(
        &service,
        "new",
        [
            base(1, Some("p1"), 2),
            vec![
                src(album("A")),
                src(MusicValue::CoverArtPath(new_rel.clone())),
            ],
        ]
        .concat(),
    );
    service
        .album_cover_cache
        .lock()
        .unwrap()
        .insert("A".into(), old_rel.clone());

    apply(&service);

    assert!(
        archived(&service, &service.music_dir.join(&old_rel)).exists(),
        "unused cover art is archived"
    );
    assert_eq!(
        service.album_cover_cache.lock().unwrap().get("A"),
        Some(&new_rel),
        "the cache no longer names an archived file"
    );
}

#[test]
fn only_plain_paths_below_the_stores_may_be_archived() {
    use super::dedup::is_store_path;
    assert!(is_store_path(Path::new("blobs/ab/abcd.flac"), "blobs"));
    assert!(is_store_path(Path::new("cover-art/x.jpg"), "cover-art"));
    for bad in [
        "blobs/../secret",
        "/etc/passwd",
        "/blobs/x",
        "cover-art/x.jpg",
        "archive/blobs/x",
        "other/x",
    ] {
        assert!(!is_store_path(Path::new(bad), "blobs"), "{bad}");
    }

    let (service, _d, _acid) = make_empty_service();
    let victim = service.music_dir.join("secret.txt");
    std::fs::create_dir_all(&service.music_dir).unwrap();
    std::fs::write(&victim, b"x").unwrap();
    let mut report = DedupApplyReport::default();
    let gate = service.retire_gate.lock().unwrap();
    service.archive_retired_files(
        &gate,
        &RetiredFiles {
            blob: Some("blobs/../secret.txt".into()),
            cover_art: Some("cover-art/../secret.txt".into()),
        },
        &mut report,
    );
    assert!(victim.exists(), "a traversing path moves nothing");
    assert_eq!(report, DedupApplyReport::default());
    assert!(!service.music_dir.join("archive").exists());
}

#[test]
fn only_plain_paths_below_the_archive_may_be_deleted() {
    use super::dedup::is_archive_path;
    assert!(is_archive_path(Path::new("blobs/ab/abcd.flac")));
    for bad in ["", "..", "../x", "blobs/../../x", "/abs", "./x"] {
        assert!(!is_archive_path(Path::new(bad)), "{bad:?}");
    }
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
    let dedup = |v| (v, test_sources().dedup);
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

    assert_eq!(first.blobs_archived, 1, "{first:?}");
    assert_eq!(second.blobs_archived, 0);
    assert!(!merge_orphan.exists());
    assert_eq!(
        std::fs::read(archived(&service, &merge_orphan)).unwrap(),
        b"aa11merged",
        "the orphan cleanup archives instead of deleting"
    );
    assert!(!archived(&service, &plain_orphan).exists());
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
    assert_eq!((report.merged, report.blobs_archived), (1, 1), "{report:?}");
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
        source: client_source(),
        attributes: AttributeList::try_new(["Album"]).unwrap(),
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
    assert_eq!(second.blobs_archived, 0);
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
        (report.merged, report.blobs_archived, report.errors.len()),
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
    assert!(archived(&service, &blob_file(&service, &prerelease)).exists());
    assert!(blob_file(&service, &release).exists());
}

// ---------------------------------------------------------------------------
// Self-healing merge at ingest (ADR-013, "Where it runs")
// ---------------------------------------------------------------------------

/// Ingest `bytes` the way a downloading client does: its source and the ItemId travel
/// with the request.
fn ingest_item(
    service: &LibraryService,
    name: &str,
    bytes: &[u8],
    item: Option<&str>,
) -> ContentHash {
    let inbox = service.music_dir.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let path = inbox.join(name);
    std::fs::write(&path, bytes).unwrap();
    let facts: Vec<MusicValue> = item
        .map(|item_id| MusicValue::ItemId(item_id.into()))
        .into_iter()
        .collect();
    let writer = item.map(|_| client_source());
    service
        .ingest_file_internal(&path, writer.as_ref(), &facts)
        .unwrap()
}

fn old_blob(service: &LibraryService, h: &ContentHash) -> std::path::PathBuf {
    service
        .music_dir
        .join(blob_path_for(h.as_str(), Path::new("x.flac")).unwrap())
}

#[test]
fn ingest_merges_a_new_copy_into_the_live_copy_of_the_same_purchase() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    assert!(old_blob(&service, &old).exists());

    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert_ne!(old, new);
    assert!(gone(&service, &old), "the old copy is retired");
    assert!(service.get_track(&new).is_ok(), "the new copy wins");
    assert_eq!(track_count(&service, "p1"), 1);
    assert!(replaces_targets(&service, &new).contains(&old.as_str().to_owned()));
    assert!(
        !old_blob(&service, &old).exists(),
        "the old blob is removed"
    );
    assert!(old_blob(&service, &new).exists());
}

#[test]
fn ingest_does_not_merge_across_item_ids() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p2"));

    assert!(service.get_track(&old).is_ok());
    assert!(service.get_track(&new).is_ok());
    assert!(old_blob(&service, &old).exists());
    assert!(replaces_targets(&service, &new).is_empty());
}

#[test]
fn ingest_does_not_merge_when_either_side_has_no_item_id() {
    let (service, _d, _acid) = make_empty_service();
    let plain_old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, None);
    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));
    assert!(service.get_track(&plain_old).is_ok());
    assert!(service.get_track(&new).is_ok());
    assert!(replaces_targets(&service, &new).is_empty());

    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    let plain_new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, None);
    assert!(service.get_track(&old).is_ok());
    assert!(service.get_track(&plain_new).is_ok());
    assert!(replaces_targets(&service, &plain_new).is_empty());
}

#[test]
fn ingest_leaves_a_soft_deleted_copy_for_the_scan() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    let deleted = service.handle_track_delete(&old);
    assert!(
        matches!(deleted, LibraryResponse::TrackDeleted),
        "{deleted:?}"
    );

    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert!(replaces_targets(&service, &new).is_empty());
    assert!(old_blob(&service, &old).exists(), "the blob is kept");
    assert!(service.is_live(&old), "the track is not retired");
}

#[test]
fn an_item_id_that_arrives_after_the_audio_hash_still_merges() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    // The new copy lands first, with its AudioHash but no ItemId: no merge yet.
    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, None);
    assert!(service.get_track(&old).is_ok());

    // The same file arrives again, now carrying the purchase.
    let again = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert_eq!(again, new, "identical bytes keep their content hash");
    assert!(gone(&service, &old));
    assert_eq!(track_count(&service, "p1"), 1);
    assert!(replaces_targets(&service, &new).contains(&old.as_str().to_owned()));
}

#[test]
fn ingesting_identical_bytes_again_is_a_no_op() {
    let (service, _d, _acid) = make_empty_service();
    let first = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));
    let second = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert_eq!(first, second);
    assert!(service.get_track(&first).is_ok());
    assert!(old_blob(&service, &first).exists());
    assert!(replaces_targets(&service, &first).is_empty());
}

#[test]
fn ingest_carries_a_user_fact_that_only_the_old_copy_has() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    write(&service, &old, vec![usr(bpm(128.0))]);

    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert!(gone(&service, &old));
    assert_eq!(count_in_acid(&service, &new, &bpm(128.0)), 1);
    assert_eq!(
        service.get_track(&new).unwrap().bpm.map(|b| b.as_f32()),
        Some(128.0)
    );
}

fn upload(service: &LibraryService, name: &str, bytes: &[u8]) -> ContentHash {
    let inbox = service.music_dir.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let path = inbox.join(name);
    std::fs::write(&path, bytes).unwrap();
    service.ingest_file_internal(&path, None, &[]).unwrap()
}

#[test]
fn the_heal_does_not_merge_a_track_that_holds_two_item_ids_with_copies_of_each() {
    let (service, _d, _acid) = make_empty_service();
    // `new` holds X and Y from an earlier confirmed merge; C1 has X, C2 has Y.
    let c1 = seed(&service, "c1", base(1, Some("x"), 1));
    let c2 = seed(&service, "c2", base(1, Some("y"), 2));
    let mut facts = base(1, Some("x"), 10);
    facts.push(src(MusicValue::ItemId("y".into())));
    let new = seed(&service, "new", facts);

    service.self_heal_after_ingest(&new);

    // The scan sees no ItemId common to all three (DifferentItemIds).
    assert!(service.is_live(&c1));
    assert!(service.is_live(&c2));
    assert!(service.is_live(&new));
    assert!(replaces_targets(&service, &new).is_empty());
}

#[test]
fn the_newest_copy_wins_when_an_old_copy_gains_the_item_id_of_a_newer_one() {
    let (service, _d, _acid) = make_empty_service();
    let a = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, None);
    let b = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));
    assert!(service.get_track(&a).is_ok());
    assert!(service.get_track(&b).is_ok());

    // A's bytes arrive again, now carrying the purchase.
    let again = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));

    assert_eq!(again, b, "ingest returns the winner's hash");
    assert!(gone(&service, &a), "the older copy loses");
    assert!(service.get_track(&b).is_ok(), "the newer copy wins");
    assert!(!old_blob(&service, &a).exists());
    assert!(old_blob(&service, &b).exists());
    assert!(replaces_targets(&service, &b).contains(&a.as_str().to_owned()));
}

#[test]
fn an_item_id_lands_on_a_track_that_already_has_a_source_and_the_heal_runs() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));
    // First seen as an upload: a Source, but no ItemId.
    let up = upload(&service, "A - Song.flac", FIXTURE_S16_L8);
    assert!(service.get_track(&old).is_ok());
    assert!(service.get_track(&up).is_ok());

    // Bandcamp delivers the same bytes with the purchase.
    let again = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert_eq!(again, up);
    assert!(gone(&service, &old), "the heal ran and merged the old copy");
    assert_eq!(track_count(&service, "p1"), 1);
}

#[test]
fn track_replace_is_unaffected_by_the_self_heal() {
    let (service, _d, _acid) = make_empty_service();
    let inbox = service.music_dir.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let old_path = inbox.join("A - Song.flac");
    std::fs::write(&old_path, FIXTURE_S16_L8_TAGS).unwrap();
    let old = service.ingest_file_internal(&old_path, None, &[]).unwrap();
    let new_path = inbox.join("A - Song (new).flac");
    std::fs::write(&new_path, FIXTURE_S16_L8).unwrap();

    let resp = service.handle_request(LibraryRequest::TrackReplace {
        old_hash: old.clone(),
        new_file_path: new_path.to_string_lossy().to_string(),
    });

    let LibraryResponse::TrackReplaced { new_hash, .. } = resp else {
        panic!("expected TrackReplaced, got {resp:?}");
    };
    assert_ne!(new_hash, old);
    assert!(gone(&service, &old));
    assert!(service.get_track(&new_hash).is_ok());
    let replaced = replaces_targets(&service, &new_hash);
    assert_eq!(
        replaced.iter().filter(|h| *h == old.as_str()).count(),
        1,
        "{replaced:?}"
    );
}

#[test]
fn the_heal_merges_two_old_copies_of_one_purchase_and_carries_from_both() {
    let (service, _d, _acid) = make_empty_service();
    let o1 = seed_with_blob(&service, "o1", base(1, Some("p1"), 1));
    let o2 = seed_with_blob(&service, "o2", base(1, Some("p1"), 2));
    write(&service, &o1, vec![usr(bpm(128.0))]);
    write(
        &service,
        &o2,
        vec![usr(MusicValue::Key(
            Key::from_traditional("A Minor").unwrap(),
        ))],
    );
    let new = seed_with_blob(&service, "new", base(1, Some("p1"), 10));

    service.self_heal_after_ingest(&new);

    assert!(gone(&service, &o1));
    assert!(gone(&service, &o2));
    assert!(service.get_track(&new).is_ok());
    let replaced = replaces_targets(&service, &new);
    assert!(replaced.contains(&o1.as_str().to_owned()), "{replaced:?}");
    assert!(replaced.contains(&o2.as_str().to_owned()), "{replaced:?}");
    assert_eq!(count_in_acid(&service, &new, &bpm(128.0)), 1);
    assert_eq!(
        count_in_acid(
            &service,
            &new,
            &MusicValue::Key(Key::from_traditional("A Minor").unwrap())
        ),
        1
    );
    assert!(!blob_file(&service, &o1).exists());
    assert!(!blob_file(&service, &o2).exists());
}

#[test]
fn a_failed_self_heal_merge_does_not_fail_the_ingest() {
    let (service, _d, _acid) = make_empty_service();
    let old = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8_TAGS, Some("p1"));

    // The old copy is retired after the candidates were found, so the merge refuses.
    let weak = Arc::downgrade(&service);
    let victim = old.clone();
    *service.merge_pre_write_hook.lock().unwrap() = Some(Box::new(move || {
        if let Some(service) = weak.upgrade() {
            retire(&service, &victim).unwrap();
        }
    }));

    let new = ingest_item(&service, "A - Song.flac", FIXTURE_S16_L8, Some("p1"));

    assert!(
        service.get_track(&new).is_ok(),
        "the ingest still succeeded"
    );
    assert!(old_blob(&service, &new).exists());
    assert!(replaces_targets(&service, &new).is_empty());
}
