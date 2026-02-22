//! Step definitions for library Background and listing.

use crate::harness::SeedTrack;
use crate::world::MdmaWorld;
use cucumber::{gherkin::Step, given, then, when};

/// Background step: seed the library with tracks from a data table.
#[given("the library contains:")]
async fn library_contains(world: &mut MdmaWorld, step: &Step) {
    if let Some(table) = step.table.as_ref() {
        for row in table.rows.iter().skip(1) {
            // columns: artist | title | bpm | genre
            let artist = row
                .get(0)
                .map(|s| s.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let title = row
                .get(1)
                .map(|s| s.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let bpm = row.get(2).and_then(|s| s.parse::<u16>().ok());
            let genre = row.get(3).map(|s| s.to_string()).filter(|s| !s.is_empty());

            world.pending_tracks.push(SeedTrack {
                artist,
                title,
                bpm,
                genre,
                duration: None,
                year: None,
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
