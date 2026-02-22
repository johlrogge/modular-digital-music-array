use cucumber::World;
use mdma_bdd::world::MdmaWorld;

// Import step modules so the proc-macro registrations get linked in.
#[allow(unused_imports)]
use mdma_bdd::steps::cli_steps;
#[allow(unused_imports)]
use mdma_bdd::steps::library_steps;
#[allow(unused_imports)]
use mdma_bdd::steps::playback_steps;
#[allow(unused_imports)]
use mdma_bdd::steps::queue_steps;
#[allow(unused_imports)]
use mdma_bdd::steps::search_steps;

#[tokio::main]
async fn main() {
    MdmaWorld::run("features/").await;
}
