//! Step definitions for search scenarios.

use crate::world::MdmaWorld;
use cucumber::when;
use library_ipc_protocol::TrackQuery;
use library_search::{parse_numeric_query, parse_string_query};

#[when(regex = r#"^I search with artist "([^"]*)"$"#)]
async fn search_by_artist(world: &mut MdmaWorld, artist: String) {
    world.ensure_env();
    let query = TrackQuery {
        artist: Some(parse_string_query(&artist)),
        ..Default::default()
    };
    match world.library().search(&query) {
        Ok(results) => {
            world.last_search_results = results;
            world.last_error = None;
        }
        Err(e) => {
            world.last_search_results = vec![];
            world.last_error = Some(e.to_string());
        }
    }
}

#[when(regex = r"^I search with bpm (\d+) tolerance (\d+)$")]
async fn search_by_bpm_tolerance(world: &mut MdmaWorld, bpm: String, tolerance: String) {
    world.ensure_env();
    let bpm_str = format!("{}+-{}", bpm, tolerance);
    let query = TrackQuery {
        bpm: parse_numeric_query(&bpm_str).ok(),
        ..Default::default()
    };
    match world.library().search(&query) {
        Ok(results) => {
            world.last_search_results = results;
            world.last_error = None;
        }
        Err(e) => {
            world.last_search_results = vec![];
            world.last_error = Some(e.to_string());
        }
    }
}

#[when(regex = r#"^I search with artist "([^"]*)" and bpm (\d+)$"#)]
async fn search_by_artist_and_bpm(world: &mut MdmaWorld, artist: String, bpm: String) {
    world.ensure_env();
    let query = TrackQuery {
        artist: Some(parse_string_query(&artist)),
        bpm: parse_numeric_query(&bpm).ok(),
        ..Default::default()
    };
    match world.library().search(&query) {
        Ok(results) => {
            world.last_search_results = results;
            world.last_error = None;
        }
        Err(e) => {
            world.last_search_results = vec![];
            world.last_error = Some(e.to_string());
        }
    }
}

#[when(regex = r#"^I search with genre "([^"]*)"$"#)]
async fn search_by_genre(world: &mut MdmaWorld, genre: String) {
    world.ensure_env();
    let query = TrackQuery {
        genre: Some(parse_string_query(&genre)),
        ..Default::default()
    };
    match world.library().search(&query) {
        Ok(results) => {
            world.last_search_results = results;
            world.last_error = None;
        }
        Err(e) => {
            world.last_search_results = vec![];
            world.last_error = Some(e.to_string());
        }
    }
}
