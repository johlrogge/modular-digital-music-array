//! Step definitions for playback control scenarios.

use crate::world::MdmaWorld;
use cucumber::{then, when};
use media_protocol::Deck;

#[when("I play from queue")]
async fn play_from_queue(world: &mut MdmaWorld) {
    world.ensure_env();
    match world.playback().play_queue() {
        Ok(()) => world.last_error = None,
        Err(e) => world.last_error = Some(e.to_string()),
    }
}

#[when("I stop playback")]
async fn stop_playback(world: &mut MdmaWorld) {
    world.ensure_env();
    match world.playback().stop(Deck::A) {
        Ok(()) => world.last_error = None,
        Err(e) => world.last_error = Some(e.to_string()),
    }
}

#[then(regex = r#"^the now playing track should be "([^"]*)"$"#)]
async fn now_playing_should_be(world: &mut MdmaWorld, expected_hash: String) {
    world.ensure_env();
    let now = world
        .playback()
        .now_playing()
        .expect("failed to get now playing");
    let hash = now.expect("nothing is playing");
    assert_eq!(
        hash.0, expected_hash,
        "Expected now playing '{}', got '{}'",
        expected_hash, hash.0
    );
}

#[then("nothing should be playing")]
async fn nothing_should_be_playing(world: &mut MdmaWorld) {
    world.ensure_env();
    let now = world
        .playback()
        .now_playing()
        .expect("failed to get now playing");
    assert!(now.is_none(), "Expected nothing playing, got: {:?}", now);
}
