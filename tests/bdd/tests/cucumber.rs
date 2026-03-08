use cucumber::World;

use mdma_bdd::steps;
use mdma_bdd::world;

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
