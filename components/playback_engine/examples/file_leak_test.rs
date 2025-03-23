// Enhanced examples/file_leak_test.rs
use playback_engine::{FlacSource, Track};
use ringbuf::HeapRb;
use std::path::PathBuf;
use std::time::Duration;

async fn force_cleanup() {
    // Yield to allow task cancellation to be processed
    tokio::task::yield_now().await;
    // Wait a short time to allow shutdown logic to complete
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Yield again to ensure runtime processes all pending tasks
    tokio::task::yield_now().await;
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Path to a test file
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches/test_data/short.flac");

    println!("Process ID: {}", std::process::id());
    println!("Press Enter to create and drop 10 tracks...");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();

    for i in 0..10 {
        println!("Creating track {}...", i);
        let buffer = HeapRb::new(8 * 1024);
        let (prod, mut cons) = buffer.split();
        let source = FlacSource::new(&path).expect("Failed to create source");
        let mut track = Track::new(source, prod)
            .await
            .expect("Failed to create track");

        // Play the track to ensure background task is active
        track.play();

        println!("Dropping track {}...", i);
        drop(track);

        // Wait and force cleanup
        force_cleanup().await;

        println!("Track {} should be fully cleaned up", i);
    }

    println!("All tracks created and dropped");
    println!("Press Enter to exit...");
    input.clear();
    std::io::stdin().read_line(&mut input).unwrap();
}
