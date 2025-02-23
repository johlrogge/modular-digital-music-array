// components/playback_engine/benches/track_loading.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use playback_engine::Track;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn test_file_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches/test_data")
        .join(name)
}

fn bench_track_loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("track_loading");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        let path = test_file_path(name);

        // Get initial metrics once before benchmarking
        let mut track = Track::new(&path).expect("Failed to load track");
        track.play();

        // Verify we can actually get samples
        let mut buffer = vec![0.0f32; 1024];
        track
            .get_next_samples(&mut buffer)
            .expect("Failed to get first samples");

        drop(track);

        // Run the actual benchmark without printing
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter_with_large_drop(|| {
                Track::new(black_box(&path)).expect("Failed to load track");
            });
        });
    }

    group.finish();
}

fn bench_time_to_playable(c: &mut Criterion) {
    let mut group = c.benchmark_group("time_to_playable");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        let path = test_file_path(name);

        // Print metrics once before benchmarking
        let start = Instant::now();
        let mut track = Track::new(&path).expect("Failed to load track");
        track.play();
        let ready_time = start.elapsed();
        let mut buffer = vec![0.0f32; 1024];
        let first_read = track
            .get_next_samples(&mut buffer)
            .expect("Failed to get samples");
        println!("\nInitial playability check for {}:", name);
        println!("  Time to playable: {:?}", ready_time);
        println!("  First buffer read: {} samples", first_read);
        drop(track);

        // Run the actual benchmark without printing
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter_with_large_drop(|| {
                let start = Instant::now();
                let mut track = Track::new(black_box(&path)).expect("Failed to load track");
                track.play();
                let ready_time = start.elapsed();
                black_box(ready_time);
                track
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_track_loading, bench_time_to_playable);
criterion_main!(benches);
