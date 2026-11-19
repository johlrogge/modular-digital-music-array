//! The read side of the library projection (ADR-030 §1): the one mapping from the
//! folded index to `TrackInfo`, the one search, and the one order.
//!
//! The library service answers its reads with these functions under its locks, and a
//! client holding a replica answers the same reads from its own fold, so the node and
//! a local fold over the same lines cannot disagree.

use crate::{parse_fact_line, IndexedTrackInfo};
use acid_protocol::is_entity_match;
use library_ipc_protocol::{
    BeatGridInfo, Bpm, CueInfo, DurationSeconds, FactType, TrackInfo, TrackQuery,
};
use library_search::{matches_query, TrackFields};
use music_facts::{ContentHash, MarkFilter, MusicValue};
use std::collections::{HashMap, HashSet};

/// Album name -> cover art path, for tracks that have both.
///
/// A track without cover art of its own shows the cover of its album. The cover of
/// the track with the smallest content hash wins, whatever the order of the tracks.
/// A track's cover is its effective one (`IndexedTrackInfo::effective_cover_art_path`):
/// its own, else the one it inherits along its `Replaces` lineage.
#[derive(Clone, Debug, Default)]
pub struct AlbumCovers(HashMap<String, String>);

impl AlbumCovers {
    /// Build the cache from an indexed track list.
    pub fn build(tracks: &[IndexedTrackInfo]) -> Self {
        let mut by_album: HashMap<&str, Vec<&IndexedTrackInfo>> = HashMap::new();
        for track in tracks {
            if let Some(album) = &track.album {
                by_album.entry(album.as_str()).or_default().push(track);
            }
        }
        Self(
            by_album
                .into_iter()
                .filter_map(|(album, of_album)| Some((album.to_owned(), album_cover(of_album)?)))
                .collect(),
        )
    }

    /// The cover of `album`, if any track on it has one.
    pub fn get(&self, album: &str) -> Option<&str> {
        self.0.get(album).map(String::as_str)
    }
}

/// The cover an album shows: the cover of the track with the smallest content hash
/// among `of_album` (the tracks of one album) that has one, whatever their order.
fn album_cover<'a>(of_album: impl IntoIterator<Item = &'a IndexedTrackInfo>) -> Option<String> {
    of_album
        .into_iter()
        .filter_map(|t| Some((t.content_hash.as_str(), t.effective_cover_art_path()?)))
        .min_by_key(|(hash, _)| *hash)
        .map(|(_, cover)| cover.to_string_lossy().into_owned())
}

/// Convert an indexed track to the protocol `TrackInfo`.
pub fn track_info(t: &IndexedTrackInfo, covers: &AlbumCovers) -> TrackInfo {
    TrackInfo {
        content_hash: t.content_hash.clone(),
        title: t.title.clone(),
        artist: t.artist.clone(),
        album: t.album.clone(),
        duration: t.duration_seconds.map(DurationSeconds::new),
        audio: t.audio,
        bpm: t.bpm.and_then(|b| Bpm::from_f32(b).ok()),
        key: t.key,
        blob_path: Some(t.blob_path.to_string_lossy().to_string()),
        cover_art_path: t
            .effective_cover_art_path()
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| {
                t.album
                    .as_ref()
                    .and_then(|album| covers.get(album).map(str::to_owned))
            }),
        track_number: t.track_number,
        disc_number: t.disc_number,
        added: t.added_at.map(|dt| dt.to_rfc3339()),
        started: t.last_started.map(|dt| dt.to_rfc3339()),
        stopped: t.last_stopped.map(|dt| dt.to_rfc3339()),
        memory_cues: t
            .memory_cues
            .iter()
            .map(|c| CueInfo {
                position_ms: c.position_ms,
                kind: c.kind.clone(),
                label: c.label.clone(),
                index: c.index,
            })
            .collect(),
        beat_grid: t
            .beat_grid
            .and_then(|(first_beat_ms, bpm_f32, beats_per_bar)| {
                Bpm::from_f32(bpm_f32).ok().map(|bpm| BeatGridInfo {
                    first_beat_ms,
                    bpm,
                    beats_per_bar,
                })
            }),
        role: t.role,
        energy: t.energy,
        genre: t.genre.clone(),
        label: t.label.clone(),
        styles: t.styles.clone(),
        marks: t.marks(),
    }
}

/// The flat fields a `TrackQuery` is evaluated against. The one mapping from an
/// indexed track to `TrackFields`.
pub fn search_fields(t: &IndexedTrackInfo) -> TrackFields<'_> {
    TrackFields {
        title: t.title.as_deref(),
        artist: t.artist.as_deref(),
        album: t.album.as_deref(),
        label: t.label.as_deref(),
        genre: t.genre.as_deref(),
        styles: &t.styles,
        bpm: t.bpm,
        key: t.key,
        duration: t.duration_seconds,
        year: t.year,
        source: t.source.as_deref(),
        last_started: t.last_started,
        last_stopped: t.last_stopped,
        added: t.added_at,
        role: t.role,
        energy: t.energy.map(u8::from),
        marks: t.marks(),
    }
}

/// Visible tracks (hidden ones excluded) that satisfy `keep`, in catalog order,
/// optionally limited. The one path every track list takes.
fn select(
    tracks: &[IndexedTrackInfo],
    covers: &AlbumCovers,
    keep: impl Fn(&IndexedTrackInfo) -> bool,
    limit: Option<usize>,
) -> Vec<TrackInfo> {
    let mut found: Vec<TrackInfo> = tracks
        .iter()
        .filter(|t| !t.is_hidden() && keep(t))
        .map(|t| track_info(t, covers))
        .collect();
    sort_catalog(&mut found);
    if let Some(n) = limit {
        found.truncate(n);
    }
    found
}

/// Tracks matching `query` (hidden tracks excluded), in catalog order.
pub fn search(
    tracks: &[IndexedTrackInfo],
    covers: &AlbumCovers,
    query: &TrackQuery,
) -> Vec<TrackInfo> {
    select(
        tracks,
        covers,
        |t| matches_query(query, &search_fields(t)),
        None,
    )
}

/// All visible tracks, in catalog order, optionally limited.
pub fn visible(
    tracks: &[IndexedTrackInfo],
    covers: &AlbumCovers,
    limit: Option<usize>,
) -> Vec<TrackInfo> {
    select(tracks, covers, |_| true, limit)
}

/// Visible tracks carrying a live mark admitted by `filter`, in catalog order.
pub fn marked(
    tracks: &[IndexedTrackInfo],
    covers: &AlbumCovers,
    filter: &MarkFilter,
) -> Vec<TrackInfo> {
    select(tracks, covers, |t| t.has_mark(filter), None)
}

/// The values currently asserted for `fact_type`, sorted.
pub fn fact_values(
    fact_index: &HashMap<FactType, HashSet<String>>,
    fact_type: &FactType,
) -> Vec<String> {
    let mut values: Vec<String> = fact_index
        .get(fact_type)
        .map(|set| set.iter().cloned().collect())
        .unwrap_or_default();
    values.sort();
    values
}

/// Format a MusicValue for display (returns type name and string value).
pub fn format_fact_for_display(value: &MusicValue) -> (String, String) {
    (value.display_name().to_string(), value.to_string())
}

/// The facts of `entity` among the raw fact `lines`, formatted for display, in log
/// order. A line belongs to the entity by the one key rule of the fact log
/// (`acid_protocol::is_entity_match`); lines that are not facts are skipped.
pub fn entity_facts<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    entity: &ContentHash,
) -> Vec<(String, String)> {
    lines
        .into_iter()
        .filter(|line| line.contains(entity.as_str()) && is_entity_match(line, entity.as_str()))
        .filter_map(parse_fact_line)
        .map(|fact| format_fact_for_display(fact.value()))
        .collect()
}

/// Put `tracks` in catalog order (ADR-030 §1.2): artist, album, disc, track, then
/// content hash.
///
/// Text compares case-insensitively (`str::to_lowercase`); equal folded text is then
/// ordered by the original text, so the order is total. Disc and track compare as
/// numbers. A missing value sorts last, for every key.
pub fn sort_catalog(tracks: &mut [TrackInfo]) {
    tracks.sort_by_cached_key(|t| {
        (
            text_key(&t.artist),
            text_key(&t.album),
            number_key(t.disc_number),
            number_key(t.track_number),
            t.content_hash.as_str().to_owned(),
        )
    });
}

/// Sort key of an optional text: present before missing, then folded, then original.
fn text_key(text: &Option<String>) -> (bool, String, String) {
    (
        text.is_none(),
        text.as_deref().map(str::to_lowercase).unwrap_or_default(),
        text.clone().unwrap_or_default(),
    )
}

/// Sort key of an optional number: present before missing, then numeric.
fn number_key(number: Option<u32>) -> (bool, u32) {
    (number.is_none(), number.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{project_lines, MusicFact};
    use library_ipc_protocol::KeyQuery;
    use music_facts::{Artist, FactSource, Mark, Title};
    use pretty_assertions::assert_eq;
    use stainless_facts::{Fact, Operation};

    fn indexed(hash: &str) -> IndexedTrackInfo {
        IndexedTrackInfo::new_empty(hash.to_owned())
    }

    fn info(t: &IndexedTrackInfo) -> TrackInfo {
        track_info(t, &AlbumCovers::default())
    }

    fn hashes(tracks: &[TrackInfo]) -> Vec<&str> {
        tracks.iter().map(|t| t.content_hash.as_str()).collect()
    }

    fn catalog_entry(
        hash: &str,
        artist: Option<&str>,
        album: Option<&str>,
        disc: Option<u32>,
        track: Option<u32>,
    ) -> TrackInfo {
        let mut t = indexed(hash);
        t.artist = artist.map(str::to_owned);
        t.album = album.map(str::to_owned);
        t.disc_number = disc;
        t.track_number = track;
        info(&t)
    }

    fn sorted(mut tracks: Vec<TrackInfo>) -> Vec<TrackInfo> {
        sort_catalog(&mut tracks);
        tracks
    }

    #[test]
    fn track_info_carries_the_main_genre() {
        let mut with = indexed("h1");
        with.genre = Some("Techno".to_owned());

        assert_eq!(info(&with).genre.as_deref(), Some("Techno"));
        assert_eq!(info(&indexed("h2")).genre, None);
    }

    #[test]
    fn track_info_carries_the_exact_duration_next_to_the_seconds() {
        use music_facts::{DurationMs, DurationSeconds as Secs};
        let source = FactSource::new("test", "1.0.0", "Unknown");
        let at = chrono::Utc::now();
        let mut t = indexed("h1");
        assert_eq!(info(&t).audio.duration_ms, None);

        for v in [
            MusicValue::DurationSeconds(Secs::new(367)),
            MusicValue::DurationMs(DurationMs::new(367_123)),
        ] {
            crate::apply_fact_to_track(&mut t, &v, at, Operation::Assert, &source);
        }
        let got = info(&t);
        assert_eq!(got.audio.duration_ms, Some(DurationMs::new(367_123)));
        assert_eq!(got.duration.map(|d| d.value()), Some(367));

        let retract = MusicValue::DurationMs(DurationMs::new(367_123));
        crate::apply_fact_to_track(&mut t, &retract, at, Operation::Retract, &source);
        assert_eq!(info(&t).audio.duration_ms, None);
    }

    #[test]
    fn track_info_carries_the_bitrate_and_sample_rate_and_drops_them_on_retract() {
        use music_facts::{Bitrate, SampleRate};
        let source = FactSource::new("test", "1.0.0", "Unknown");
        let at = chrono::Utc::now();
        let mut t = indexed("h1");
        let (rate, hz) = (Bitrate::new(1411), SampleRate::new(44100));
        for v in [MusicValue::Bitrate(rate), MusicValue::SampleRate(hz)] {
            crate::apply_fact_to_track(&mut t, &v, at, Operation::Assert, &source);
        }
        let got = info(&t);
        assert_eq!(got.audio.bitrate, Some(rate));
        assert_eq!(got.audio.sample_rate, Some(hz));

        crate::apply_fact_to_track(
            &mut t,
            &MusicValue::Bitrate(rate),
            at,
            Operation::Retract,
            &source,
        );
        let got = info(&t);
        assert_eq!(got.audio.bitrate, None);
        assert_eq!(got.audio.sample_rate, Some(hz), "the other property stays");

        crate::apply_fact_to_track(
            &mut t,
            &MusicValue::SampleRate(hz),
            at,
            Operation::Retract,
            &source,
        );
        assert_eq!(info(&t).audio.sample_rate, None);
    }

    // ---- marks ------------------------------------------------------------------

    fn apply_mark(t: &mut IndexedTrackInfo, scope: Option<&str>, op: Operation) {
        let at = chrono::Utc::now();
        let value = MusicValue::Bookmarked {
            scope: scope.map(str::to_owned),
            timestamp: at,
        };
        let source = FactSource::new("test", "1.0.0", "Unknown");
        crate::apply_fact_to_track(t, &value, at, op, &source);
    }

    fn named(name: &str) -> Mark {
        name.parse().unwrap()
    }

    #[test]
    fn track_info_lists_live_marks_default_first_then_named_sorted() {
        let mut t = indexed("h1");
        for scope in [Some("opener"), None, Some("banger"), Some("banger")] {
            apply_mark(&mut t, scope, Operation::Assert);
        }
        assert_eq!(
            info(&t).marks,
            vec![Mark::Default, named("banger"), named("opener")]
        );
        assert_eq!(info(&indexed("h2")).marks, Vec::<Mark>::new());
    }

    #[test]
    fn track_info_leaves_out_retracted_and_legacy_marks() {
        let mut t = indexed("h1");
        apply_mark(&mut t, Some("banger"), Operation::Assert);
        apply_mark(&mut t, Some("opener"), Operation::Assert);
        apply_mark(&mut t, Some("Some Free Text!\u{200B}"), Operation::Assert);
        let retract = |t: &mut IndexedTrackInfo| {
            let value = t
                .provenance
                .iter()
                .find(|(v, _)| named("banger").matches(v))
                .map(|(v, _)| v.clone())
                .unwrap();
            let source = FactSource::new("test", "1.0.0", "Unknown");
            crate::apply_fact_to_track(t, &value, chrono::Utc::now(), Operation::Retract, &source);
        };
        retract(&mut t);
        assert_eq!(info(&t).marks, vec![named("opener")]);
    }

    #[test]
    fn scopes_that_fold_to_one_mark_are_listed_once() {
        let mut t = indexed("h1");
        for scope in [Some("Banger"), Some("banger"), Some("liked"), None] {
            apply_mark(&mut t, scope, Operation::Assert);
        }
        assert_eq!(info(&t).marks, vec![Mark::Default, named("banger")]);
    }

    #[test]
    fn marks_and_has_mark_agree() {
        let mut t = indexed("h1");
        for scope in [Some("banger"), Some("Free Text!\u{200B}"), None] {
            apply_mark(&mut t, scope, Operation::Assert);
        }
        for mark in [Mark::Default, named("banger"), named("opener")] {
            assert_eq!(
                t.marks().contains(&mark),
                t.has_mark(&MarkFilter::Only(mark.clone())),
                "{mark}"
            );
        }
    }

    #[test]
    fn search_matches_a_live_mark_and_only_that_mark() {
        let mut marked_track = indexed("sha256:a");
        apply_mark(&mut marked_track, Some("banger"), Operation::Assert);
        let tracks = vec![marked_track, indexed("sha256:b")];
        let covers = AlbumCovers::build(&tracks);
        let by = |mark: Mark| {
            let query = TrackQuery {
                mark: Some(mark),
                ..TrackQuery::default()
            };
            hashes(&search(&tracks, &covers, &query))
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(by(named("banger")), vec!["sha256:a"]);
        assert!(by(Mark::Default).is_empty());
    }

    // ---- order ------------------------------------------------------------------

    #[test]
    fn order_is_artist_then_album_then_disc_then_track_then_hash() {
        let out = sorted(vec![
            catalog_entry("h6", Some("B"), Some("X"), Some(1), Some(1)),
            catalog_entry("h5", Some("A"), Some("Y"), Some(1), Some(1)),
            catalog_entry("h4", Some("A"), Some("X"), Some(2), Some(1)),
            catalog_entry("h3", Some("A"), Some("X"), Some(1), Some(2)),
            catalog_entry("h2", Some("A"), Some("X"), Some(1), Some(1)),
            catalog_entry("h1", Some("A"), Some("X"), Some(1), Some(1)),
        ]);
        assert_eq!(hashes(&out), vec!["h1", "h2", "h3", "h4", "h5", "h6"]);
    }

    #[test]
    fn text_order_ignores_case_and_ties_go_to_the_original_text() {
        let out = sorted(vec![
            catalog_entry("h1", Some("beta"), None, None, None),
            catalog_entry("h2", Some("Alpha"), None, None, None),
            catalog_entry("h3", Some("alpha"), None, None, None),
            catalog_entry("h4", Some("Beta"), None, None, None),
        ]);
        // "Alpha" < "alpha" and "Beta" < "beta" in the original text.
        assert_eq!(hashes(&out), vec!["h2", "h3", "h4", "h1"]);
    }

    #[test]
    fn disc_and_track_compare_as_numbers() {
        let out = sorted(vec![
            catalog_entry("h1", Some("A"), Some("X"), Some(1), Some(10)),
            catalog_entry("h2", Some("A"), Some("X"), Some(1), Some(9)),
            catalog_entry("h3", Some("A"), Some("X"), Some(10), Some(1)),
            catalog_entry("h4", Some("A"), Some("X"), Some(2), Some(1)),
        ]);
        assert_eq!(hashes(&out), vec!["h2", "h1", "h4", "h3"]);
    }

    #[test]
    fn a_missing_value_sorts_last_for_every_key() {
        let out = sorted(vec![
            catalog_entry("h1", None, Some("X"), Some(1), Some(1)),
            catalog_entry("h2", Some("A"), None, Some(1), Some(1)),
            catalog_entry("h3", Some("A"), Some("X"), None, Some(1)),
            catalog_entry("h4", Some("A"), Some("X"), Some(1), None),
            catalog_entry("h5", Some("A"), Some("X"), Some(1), Some(1)),
        ]);
        assert_eq!(hashes(&out), vec!["h5", "h4", "h3", "h2", "h1"]);
    }

    // ---- search ------------------------------------------------------------------

    #[test]
    fn search_excludes_hidden_tracks() {
        let mut gone = indexed("sha256:gone");
        gone.deleted_at = Some(chrono::Utc::now());
        let tracks = vec![indexed("sha256:kept"), gone];
        let covers = AlbumCovers::build(&tracks);
        let found = search(&tracks, &covers, &TrackQuery::default());
        assert_eq!(hashes(&found), vec!["sha256:kept"]);
        assert_eq!(
            hashes(&visible(&tracks, &covers, None)),
            vec!["sha256:kept"]
        );
    }

    #[test]
    fn search_matches_a_key_on_the_wheel_and_orders_the_result() {
        let key = |hash: &str, k: &str| {
            let mut t = indexed(hash);
            t.key = Some(k.parse().unwrap());
            t.artist = Some("A".to_owned());
            t
        };
        let tracks = vec![
            key("sha256:b", "A minor"),
            key("sha256:far", "C minor"),
            key("sha256:a", "E minor"),
        ];
        let query = TrackQuery {
            key: Some("8A+-1".parse::<KeyQuery>().unwrap()),
            ..TrackQuery::default()
        };
        let found = search(&tracks, &AlbumCovers::build(&tracks), &query);
        assert_eq!(hashes(&found), vec!["sha256:a", "sha256:b"]);
    }

    #[test]
    fn visible_limits_after_ordering() {
        let tracks: Vec<_> = ["sha256:c", "sha256:a", "sha256:b"]
            .into_iter()
            .map(indexed)
            .collect();
        let found = visible(&tracks, &AlbumCovers::default(), Some(2));
        assert_eq!(hashes(&found), vec!["sha256:a", "sha256:b"]);
    }

    #[test]
    fn a_track_borrows_the_cover_of_its_album() {
        let mut with_cover = indexed("sha256:a");
        with_cover.album = Some("Album".to_owned());
        with_cover.cover_art_path = Some("cover-art/a.jpg".into());
        let mut without = indexed("sha256:b");
        without.album = Some("Album".to_owned());
        let tracks = vec![with_cover, without];
        let found = visible(&tracks, &AlbumCovers::build(&tracks), None);
        assert_eq!(found[1].cover_art_path.as_deref(), Some("cover-art/a.jpg"));
    }

    #[test]
    fn the_album_cover_does_not_depend_on_track_order() {
        let track = |hash: &str, cover: &str| {
            let mut t = indexed(hash);
            t.album = Some("Album".to_owned());
            t.cover_art_path = Some(cover.into());
            t
        };
        let a = track("sha256:a", "cover-art/a.jpg");
        let b = track("sha256:b", "cover-art/b.jpg");
        let forward = AlbumCovers::build(&[a.clone(), b.clone()]);
        let backward = AlbumCovers::build(&[b, a]);
        assert_eq!(forward.get("Album"), Some("cover-art/a.jpg"));
        assert_eq!(backward.get("Album"), Some("cover-art/a.jpg"));
    }

    #[test]
    fn album_cover_picks_the_smallest_hash_that_has_a_cover() {
        let mut bare = indexed("sha256:0");
        bare.album = Some("Album".to_owned());
        let mut b = indexed("sha256:b");
        b.cover_art_path = Some("cover-art/b.jpg".into());
        let mut a = indexed("sha256:a");
        a.cover_art_path = Some("cover-art/a.jpg".into());
        assert_eq!(
            album_cover([&bare, &b, &a]).as_deref(),
            Some("cover-art/a.jpg")
        );
        assert_eq!(album_cover([&bare]), None);
    }

    #[test]
    fn a_hidden_track_that_sorts_first_does_not_use_up_the_limit() {
        let mut hidden = indexed("sha256:a");
        hidden.artist = Some("A".to_owned());
        hidden.deleted_at = Some(chrono::Utc::now());
        let mut shown = indexed("sha256:b");
        shown.artist = Some("B".to_owned());
        let tracks = vec![hidden, shown];
        let found = visible(&tracks, &AlbumCovers::default(), Some(1));
        assert_eq!(hashes(&found), vec!["sha256:b"]);
    }

    #[test]
    fn marked_returns_only_marked_visible_tracks_in_order() {
        let source = FactSource::new("test", "1.0.0", "Unknown");
        let mark = Mark::Default;
        let mut tracks = Vec::new();
        for hash in ["sha256:b", "sha256:a", "sha256:plain"] {
            tracks.push(indexed(hash));
        }
        for track in tracks.iter_mut().take(2) {
            track
                .provenance
                .push((mark.fact_value(chrono::Utc::now()), source.clone()));
        }
        let found = marked(&tracks, &AlbumCovers::default(), &MarkFilter::Only(mark));
        assert_eq!(hashes(&found), vec!["sha256:a", "sha256:b"]);
    }

    #[test]
    fn an_empty_artist_is_present_and_sorts_before_a() {
        let out = sorted(vec![
            catalog_entry("h1", Some("a"), None, None, None),
            catalog_entry("h2", None, None, None, None),
            catalog_entry("h3", Some(""), None, None, None),
        ]);
        assert_eq!(hashes(&out), vec!["h3", "h1", "h2"]);
    }

    // ---- fact values and entity facts -----------------------------------------

    #[test]
    fn fact_values_are_sorted() {
        let fact_type = FactType::new("Title");
        let index = HashMap::from([(
            fact_type.clone(),
            HashSet::from(["b".to_owned(), "c".to_owned(), "a".to_owned()]),
        )]);
        assert_eq!(fact_values(&index, &fact_type), vec!["a", "b", "c"]);
        assert!(fact_values(&index, &FactType::new("Artist")).is_empty());
    }

    #[test]
    fn entity_facts_keeps_only_the_exact_entity() {
        let source = FactSource::new("test", "1.0.0", "Unknown");
        let line = |entity: &str, value: MusicValue| {
            let fact: MusicFact = Fact::new(
                ContentHash::new(entity),
                value,
                chrono::Utc::now(),
                source.clone(),
                Operation::Assert,
            );
            serde_json::to_string(&fact).unwrap()
        };
        let mine_title = MusicValue::Title(Title::new("Mine"));
        let mine_artist = MusicValue::Artist(Artist::new("Me"));
        let lines = [
            line("sha256:abc", mine_title.clone()),
            line(
                "sha256:abcdef",
                MusicValue::Title(Title::new("Longer hash")),
            ),
            "not a fact".to_owned(),
            line("sha256:abc", mine_artist.clone()),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        // The fixture lines must be facts, or the test proves nothing.
        assert_eq!(project_lines(lines.iter().copied()).tracks.len(), 2);
        let facts = entity_facts(lines.iter().copied(), &ContentHash::new("sha256:abc"));
        assert_eq!(
            facts,
            vec![
                format_fact_for_display(&mine_title),
                format_fact_for_display(&mine_artist),
            ]
        );
    }
}
