//! DMT Master Node
//!
//! Main entry point for the DMT master node that coordinates
//! timing across the network.

use clock::MusicalClock;
use color_eyre::Result;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // Install color-eyre
    color_eyre::install()?;

    let clock = MusicalClock::new(clock::SystemTimeSource);
    println!("DMT Master node starting...");

    // Temporary test loop
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        clock.tick()?;
        let (ticks, tempo) = clock.get_position();
        println!("Tick: {}, Tempo: {}", ticks, tempo);
    }
}
