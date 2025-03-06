use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use playback_engine::FlacSource;
use playback_engine::Track;
use std::path::PathBuf;
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
        let path = test_file_path(name);

        // Get initial metrics once before benchmarking
        let track = rt.block_on(async {
            Track::<FlacSource>::new(&path)
                .await
                .expect("Failed to load track")
        });

        // Drop the track immediately after use to clean up resources
        drop(track);

        // Run the actual benchmark with explicit cleanup
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter_with_large_drop(|| {
                let track = rt.block_on(async {
                    Track::<FlacSource>::new(black_box(&path))
                        .await
                        .expect("Failed to load track")
                });

                // The iter_with_large_drop will drop the track after each iteration
                track
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
            Track::<FlacSource>::new(&path)
                .await
                .expect("Failed to load track")
        });
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
                let mut track = rt.block_on(async {
                    Track::<FlacSource>::new(black_box(&path))
                        .await
                        .expect("Failed to load track")
                });
                track.play();
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
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        let path = test_file_path(name);

        // Create a track for testing
        let mut track = rt.block_on(async {
            Track::<FlacSource>::new(&path)
                .await
                .expect("Failed to load track")
        });
        let length = track.length();

        // Benchmark seeking to different positions
        let positions = [
            ("start", 0),
            ("quarter", length / 4),
            ("middle", length / 2),
            ("three_quarters", length * 3 / 4),
            ("end", length.saturating_sub(1024)),
        ];

        for (label, pos) in positions {
            group.bench_with_input(
                BenchmarkId::new(format!("seek_to_{}", label), name),
                &pos,
                |b, &pos| {
                    b.iter(|| {
                        track.seek(pos);
                    });
                },
            );
        }

        // Benchmark seek and read (more realistic)
        for (label, pos) in positions {
            group.bench_with_input(
                BenchmarkId::new(format!("seek_and_read_{}", label), name),
                &pos,
                |b, &pos| {
                    let mut buffer = vec![0.0f32; 1024];
                    b.iter(|| {
                        track.seek(pos);
                        track.get_next_samples(&mut buffer).unwrap();
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
