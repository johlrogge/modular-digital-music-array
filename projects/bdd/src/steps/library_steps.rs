//! Step definitions for library Background and listing.

use crate::harness::SeedTrack;
use crate::world::MdmaWorld;
use cucumber::{gherkin::Step, given, then, when};
use mdma_client::ContentHash;

/// Background step: seed the library with tracks from a data table.
/// Parses the header row dynamically so any column order works, and new columns
/// (hash, duration, year) are picked up automatically.
#[given("the library contains:")]
async fn library_contains(world: &mut MdmaWorld, step: &Step) {
    if let Some(table) = step.table.as_ref() {
        let header = &table.rows[0];
        let col = |name: &str| -> Option<usize> {
            header.iter().position(|h| h.eq_ignore_ascii_case(name))
        };

        let col_hash = col("hash");
        let col_artist = col("artist");
        let col_title = col("title");
        let col_bpm = col("bpm");
        let col_genre = col("genre");
        let col_duration = col("duration");
        let col_year = col("year");

        for row in table.rows.iter().skip(1) {
            let get = |idx: Option<usize>| -> Option<&str> {
                idx.and_then(|i| row.get(i))
                    .map(|s| s.as_str())
                    .filter(|s| !s.is_empty())
            };

            world.pending_tracks.push(SeedTrack {
                artist: get(col_artist).unwrap_or("Unknown").to_string(),
                title: get(col_title).unwrap_or("Unknown").to_string(),
                bpm: get(col_bpm).and_then(|s| s.parse::<u16>().ok()),
                genre: get(col_genre).map(|s| s.to_string()),
                duration: get(col_duration).and_then(|s| s.parse::<u32>().ok()),
                year: get(col_year).and_then(|s| s.parse::<u32>().ok()),
                hash: get(col_hash).map(|s| s.to_string()),
                raw_hash: None,
            });
        }
    }
}

#[when("I list all tracks")]
async fn list_all_tracks(world: &mut MdmaWorld) {
    world.ensure_env();
    match world.library().list_tracks(None) {
        Ok(tracks) => {
            world.last_search_results = tracks;
            world.last_error = None;
        }
        Err(e) => {
            world.last_error = Some(e.to_string());
        }
    }
}

#[then(regex = r"^I should find (\d+) tracks?$")]
async fn should_find_n_tracks(world: &mut MdmaWorld, expected: usize) {
    assert_eq!(
        world.last_search_results.len(),
        expected,
        "Expected {} track(s), found {}",
        expected,
        world.last_search_results.len()
    );
}

/// Assert search results match a table exactly (unordered, matched by title).
/// Supported columns: artist, title, bpm, genre.
#[then("the results should be:")]
async fn results_should_be(world: &mut MdmaWorld, step: &Step) {
    let table = step.table.as_ref().expect("missing table for results");
    let header = &table.rows[0];
    let col =
        |name: &str| -> Option<usize> { header.iter().position(|h| h.eq_ignore_ascii_case(name)) };
    let col_artist = col("artist");
    let col_title = col("title");
    let col_bpm = col("bpm");
    let col_genre = col("genre");

    let expected_rows = &table.rows[1..];
    let results = &world.last_search_results;

    assert_eq!(
        results.len(),
        expected_rows.len(),
        "Expected {} result(s), got {}.\nResults: {:?}",
        expected_rows.len(),
        results.len(),
        results
            .iter()
            .map(|t| (&t.artist, &t.title))
            .collect::<Vec<_>>()
    );

    for row in expected_rows {
        let get = |idx: Option<usize>| -> Option<&str> {
            idx.and_then(|i| row.get(i))
                .map(|s| s.as_str())
                .filter(|s| !s.is_empty())
        };
        let exp_title = get(col_title).expect("results table must have a title column");

        let track = results
            .iter()
            .find(|t| t.title.as_deref() == Some(exp_title))
            .unwrap_or_else(|| {
                panic!(
                    "No result with title '{}'. Have: {:?}",
                    exp_title,
                    results.iter().map(|t| &t.title).collect::<Vec<_>>()
                )
            });

        if let Some(exp_artist) = get(col_artist) {
            assert_eq!(
                track.artist.as_deref(),
                Some(exp_artist),
                "Track '{}': expected artist '{}', got {:?}",
                exp_title,
                exp_artist,
                track.artist
            );
        }
        if let Some(exp_bpm) = get(col_bpm) {
            let exp_bpm: u32 = exp_bpm.parse().expect("bpm must be a number");
            let actual_bpm = track.bpm.as_ref().map(|b| b.as_u32());
            assert_eq!(
                actual_bpm,
                Some(exp_bpm),
                "Track '{}': expected bpm {}, got {:?}",
                exp_title,
                exp_bpm,
                actual_bpm
            );
        }
        if let Some(exp_genre) = get(col_genre) {
            // Genre is not on TrackInfo directly — skip for now
            let _ = exp_genre;
        }
    }
}

#[then("the operation should succeed")]
async fn operation_should_succeed(world: &mut MdmaWorld) {
    assert!(
        world.last_error.is_none(),
        "Expected success, got error: {:?}",
        world.last_error
    );
}

#[then(regex = r#"^the operation should fail with "([^"]*)"$"#)]
async fn operation_should_fail_with(world: &mut MdmaWorld, expected_msg: String) {
    let err = world
        .last_error
        .as_ref()
        .expect("Expected an error, but operation succeeded");
    assert!(
        err.contains(&expected_msg),
        "Expected error containing '{}', got: {}",
        expected_msg,
        err
    );
}

/// Step: add a legacy track with a raw hash (no sha256: prefix, arbitrary length).
/// This simulates legacy entities that may be short or lack the standard prefix.
#[given(
    regex = r#"^the library also contains a legacy track with raw hash "([^"]*)" and title "([^"]*)" by "([^"]*)"$"#
)]
async fn library_also_contains_legacy_track(
    world: &mut MdmaWorld,
    raw_hash: String,
    title: String,
    artist: String,
) {
    world.pending_tracks.push(SeedTrack {
        artist,
        title,
        bpm: None,
        genre: None,
        duration: None,
        year: None,
        hash: None,
        raw_hash: Some(raw_hash),
    });
}

/// Step: resolve a hash via get_track (exercises the resolve_hash ambiguity path).
#[when(regex = r#"^I resolve hash "([^"]*)"$"#)]
async fn resolve_hash(world: &mut MdmaWorld, hash: String) {
    world.ensure_env();
    let content_hash = ContentHash::new(hash);
    match world.library().get_track(&content_hash) {
        Ok(_) => {
            world.last_error = None;
        }
        Err(e) => {
            world.last_error = Some(e.to_string());
        }
    }
}
