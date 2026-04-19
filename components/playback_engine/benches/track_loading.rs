use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use playback_engine::AudioSource;
use playback_engine::Source;
use playback_engine::Track;
use ringbuf::HeapRb;
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
        // Run the actual benchmark with explicit cleanup
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter(|| {
                let buffer = HeapRb::new(1024 * 8);
                let (prod, _cons) = buffer.split();
                // Create a AudioSource
                let source = AudioSource::new(&path).expect("Could not create source");
                let source_rate = source.sample_rate();

                // Create a Track with the source (no resampling in benchmarks)
                let track = Track::new(source, prod, source_rate, source_rate)
                    .expect("Could not create track");

                // Explicitly drop the track — Drop joins the decoder thread
                drop(track);
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
        let buffer = HeapRb::new(1024 * 8);
        let (prod, mut cons) = buffer.split();
        // Print metrics once before benchmarking
        let start = Instant::now();
        let source = AudioSource::new(&path).unwrap();
        let rate = source.sample_rate();
        let mut track = Track::new(source, prod, rate, rate).expect("Failed to create track");
        let load_time = start.elapsed();

        let start = Instant::now();
        track.play();
        let play_time = start.elapsed();

        let ready_time = load_time + play_time;

        let mut buffer = [0.0f32; 1024];
        let first_read = cons.pop_slice(&mut buffer);
        assert_eq!(first_read, 1024);

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
                let buffer = HeapRb::new(8 * 1024);
                let (prod, _cons) = buffer.split();
                // Load track
                let source = AudioSource::new(&path).unwrap();
                let rate = source.sample_rate();
                let mut track =
                    Track::new(source, prod, rate, rate).expect("Failed to create track");

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

criterion_group!(benches, bench_track_loading, bench_time_to_playable);
criterion_main!(benches);
