use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use parking_lot::Mutex;
use playback_engine::FlacSource;
use playback_engine::Track;
use playback_primitives::PlaybackError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

fn test_file_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches/test_data")
        .join(name)
}

fn bench_track_loading(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("track_loading");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        // Run the actual benchmark with explicit cleanup
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter(|| {
                // Create Track in a block to ensure it's dropped right after use
                let track = rt.block_on(async {
                    // Create a FlacSource
                    let source = FlacSource::new(&path).expect("Could not create source");

                    // Create a Track with the source
                    let track = Track::new(source).await.expect("Could not create track");
                    track
                });

                // Explicitly drop the track
                drop(track);

                // Give runtime a chance to clean up
                rt.block_on(async {
                    tokio::task::yield_now().await;
                });

                // Force GC-like cleanup
                std::thread::sleep(std::time::Duration::from_millis(1));
            });
        });
    }

    group.finish();
}

fn bench_time_to_playable(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("time_to_playable");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        let path = test_file_path(name);

        // Print metrics once before benchmarking
        let start = Instant::now();
        let mut track = rt.block_on(async {
            let source = FlacSource::new(&path).unwrap();
            Track::new(source).await.expect("Failed to create track")
        });
        let load_time = start.elapsed();

        let start = Instant::now();
        track.play();
        let play_time = start.elapsed();

        let ready_time = load_time + play_time;

        let mut buffer = vec![0.0f32; 1024];
        let first_read = track
            .get_next_samples(&mut buffer)
            .expect("Failed to get samples");

        println!("\nInitial playability check for {}:", name);
        println!("  Time to load: {:?}", load_time);
        println!("  Time to play: {:?}", play_time);
        println!("  Total time to playable: {:?}", ready_time);
        println!("  First buffer read: {} samples", first_read);

        drop(track);

        // Run the actual benchmark without printing
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter_with_large_drop(|| {
                let start = Instant::now();

                // Load track
                let mut track = rt.block_on(async {
                    let source = FlacSource::new(&path).unwrap();
                    Track::new(source).await.expect("Failed to create track")
                });

                // Start playback
                track.play();

                // Measure time to playable
                let ready_time = start.elapsed();
                black_box(ready_time);

                track
            });
        });
    }

    group.finish();
}

fn bench_seeking(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("seeking");

    for name in ["short.flac", "medium.flac", "long.flac"] {
        let path = test_file_path(name);

        // Create a track for testing
        let track = rt.block_on(async {
            let source = FlacSource::new(&path).unwrap();
            Track::new(source).await.expect("Failed to create track")
        });

        // Create a shared reference that can be cloned for each benchmark
        let track = Arc::new(Mutex::new(track));
        let length = rt.block_on(async { track.lock().length() });

        let positions = [
            ("start", 0),
            ("quarter", length / 4),
            ("middle", length / 2),
            ("three_quarters", length * 3 / 4),
            ("end", length.saturating_sub(1024)),
        ];

        for (label, pos) in positions {
            // Seek benchmark
            let track_clone = track.clone();
            group.bench_with_input(
                BenchmarkId::new(format!("seek_to_{}", label), name),
                &pos,
                |b, &pos| {
                    let pos = black_box(pos);
                    b.iter(|| {
                        rt.block_on(async {
                            let mut track = track_clone.lock();
                            track.seek(pos).unwrap();
                        });
                    });
                },
            );

            // Seek and read benchmark
            let track_clone = track.clone();
            group.bench_with_input(
                BenchmarkId::new(format!("seek_and_read_{}", label), name),
                &pos,
                |b, &pos| {
                    let mut buffer = vec![0.0f32; 1024];
                    let pos = black_box(pos);
                    b.iter(|| {
                        rt.block_on(async {
                            let mut track = track_clone.lock();
                            track.seek(pos).unwrap();
                            track.get_next_samples(&mut buffer).unwrap();
                        });
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_track_loading,
    bench_time_to_playable,
    bench_seeking
);
criterion_main!(benches);
