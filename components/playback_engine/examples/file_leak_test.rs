// Enhanced examples/file_leak_test.rs
use playback_engine::{AudioSource, Source, Track};
use ringbuf::HeapRb;
use std::path::PathBuf;
use std::time::Duration;

fn main() {
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
        let (prod, _cons) = buffer.split();
        let source = AudioSource::new(&path).expect("Failed to create source");
        let source_rate = source.sample_rate();
        let mut track =
            Track::new(source, prod, source_rate, source_rate).expect("Failed to create track");

        // Play the track to ensure background thread is active
        track.play();

        println!("Dropping track {}...", i);
        drop(track);

        // Wait for decoder thread to join (handled by Drop)
        std::thread::sleep(Duration::from_millis(50));

        println!("Track {} should be fully cleaned up", i);
    }

    println!("All tracks created and dropped");
    println!("Press Enter to exit...");
    input.clear();
    std::io::stdin().read_line(&mut input).unwrap();
}
