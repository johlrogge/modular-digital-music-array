//! Step definitions for queue management scenarios.

use crate::world::MdmaWorld;
use cucumber::{given, then, when};
use mdma_client::ContentHash;

#[given(regex = r#"^the queue contains "([^"]*)"$"#)]
async fn queue_contains(world: &mut MdmaWorld, hash: String) {
    world.ensure_env();
    let content_hash = ContentHash::new(hash);
    if let Err(e) = world
        .playback()
        .queue_append(content_hash, "audio".to_string())
    {
        world.last_error = Some(e.to_string());
    }
}

#[when(regex = r#"^I append "([^"]*)" to the queue$"#)]
async fn append_to_queue(world: &mut MdmaWorld, hash: String) {
    world.ensure_env();
    let content_hash = ContentHash::new(hash);
    match world
        .playback()
        .queue_append(content_hash, "audio".to_string())
    {
        Ok(()) => world.last_error = None,
        Err(e) => world.last_error = Some(e.to_string()),
    }
}

#[when(regex = r#"^I prepend "([^"]*)" to the queue$"#)]
async fn prepend_to_queue(world: &mut MdmaWorld, hash: String) {
    world.ensure_env();
    let content_hash = ContentHash::new(hash);
    match world
        .playback()
        .queue_next(content_hash, "audio".to_string())
    {
        Ok(()) => world.last_error = None,
        Err(e) => world.last_error = Some(e.to_string()),
    }
}

#[when("I clear the queue")]
async fn clear_queue(world: &mut MdmaWorld) {
    world.ensure_env();
    match world.playback().queue_clear() {
        Ok(()) => world.last_error = None,
        Err(e) => world.last_error = Some(e.to_string()),
    }
}

#[when("I list the queue")]
async fn list_queue(world: &mut MdmaWorld) {
    world.ensure_env();
    match world.playback().queue_list() {
        Ok(hashes) => {
            world.last_queue = hashes;
            world.last_error = None;
        }
        Err(e) => world.last_error = Some(e.to_string()),
    }
}

#[then(regex = r"^the queue should contain (\d+) tracks?$")]
async fn queue_should_contain_n(world: &mut MdmaWorld, expected: usize) {
    world.ensure_env();
    let hashes = world.playback().queue_list().expect("failed to list queue");
    assert_eq!(
        hashes.len(),
        expected,
        "Expected {} track(s) in queue, found {}",
        expected,
        hashes.len()
    );
}

#[then("the queue should be empty")]
async fn queue_should_be_empty(world: &mut MdmaWorld) {
    world.ensure_env();
    let hashes = world.playback().queue_list().expect("failed to list queue");
    assert!(
        hashes.is_empty(),
        "Expected empty queue, found {} tracks",
        hashes.len()
    );
}
