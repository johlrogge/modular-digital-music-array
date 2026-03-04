use cucumber::World;

#[path = "../src/harness.rs"]
mod harness;
#[path = "../src/playback_simulator.rs"]
mod playback_simulator;
#[path = "../src/steps/mod.rs"]
mod steps;
#[path = "../src/world.rs"]
mod world;

use world::MdmaWorld;

// Import step modules so the proc-macro registrations get linked in.
#[allow(unused_imports)]
use steps::cli_steps;
#[allow(unused_imports)]
use steps::library_steps;
#[allow(unused_imports)]
use steps::playback_steps;
#[allow(unused_imports)]
use steps::queue_steps;
#[allow(unused_imports)]
use steps::search_steps;

#[tokio::main]
async fn main() {
    MdmaWorld::run("features/").await;
}
