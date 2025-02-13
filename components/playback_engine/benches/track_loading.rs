// components/playback_engine/benches/track_loading.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use playback_engine::{LoadMetrics, Track};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn test_file_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches/test_data")
        .join(name)
}

// Print the detailed metrics once for a file
fn print_metrics(name: &str, metrics: &LoadMetrics, time_to_playable: Duration) {
    println!("\nMetrics for {}:", name);
    println!("\nTiming:");
    println!("  Time to playable:   {:?}", time_to_playable);
    println!("  File open:          {:?}", metrics.file_open_time);
    println!("  Decoder creation:   {:?}", metrics.decoder_creation_time);
    println!("  Buffer allocation:  {:?}", metrics.buffer_allocation_time);

    println!("\nDecoding breakdown:");
    let decode_total = metrics.decoding_time.as_secs_f64();
    println!(
        "  Packet reading:     {:?} ({:.1}%)",
        metrics.decoding_stats.packet_read_time,
        metrics.decoding_stats.packet_read_time.as_secs_f64() * 100.0 / decode_total
    );
    println!(
        "  Packet decoding:    {:?} ({:.1}%)",
        metrics.decoding_stats.packet_decode_time,
        metrics.decoding_stats.packet_decode_time.as_secs_f64() * 100.0 / decode_total
    );
    println!(
        "  Sample copying:     {:?} ({:.1}%)",
        metrics.decoding_stats.sample_copy_time,
        metrics.decoding_stats.sample_copy_time.as_secs_f64() * 100.0 / decode_total
    );
    println!("  Total decoding:     {:?}", metrics.decoding_time);

    println!("\nPacket statistics:");
    println!(
        "  Packets processed:  {}",
        metrics.decoding_stats.packets_processed
    );
    println!(
        "  Avg packet size:    {} bytes",
        metrics.decoding_stats.total_packet_bytes / metrics.decoding_stats.packets_processed
    );
    println!(
        "  Largest packet:     {} bytes",
        metrics.decoding_stats.largest_packet
    );
    println!(
        "  Smallest packet:    {} bytes",
        metrics.decoding_stats.smallest_packet
    );

    println!("\nOverall statistics:");
    println!("  Total time:         {:?}", metrics.total_time);
    println!("  Decoded frames:     {}", metrics.decoded_frames);
    println!("  Buffer size:        {} samples", metrics.buffer_size);
    println!("  Read calls:         {}", metrics.read_calls);
    println!(
        "  Bytes read:         {} ({:.2} MB)",
        metrics.bytes_read,
        metrics.bytes_read as f64 / 1_048_576.0
    );
}

fn bench_track_loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("track_loading");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        let path = test_file_path(name);

        // Get initial metrics once before benchmarking
        let start = Instant::now();
        let (mut track, metrics) = Track::new(&path).expect("Failed to load track");
        let load_time = start.elapsed();
        track.play();
        let time_to_playable = start.elapsed();

        // Verify we can actually get samples
        let mut buffer = vec![0.0f32; 1024];
        track
            .get_next_samples(&mut buffer)
            .expect("Failed to get first samples");

        print_metrics(name, &metrics, time_to_playable);
        drop(track);

        // Run the actual benchmark without printing
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter_with_large_drop(|| {
                let (track, _) = Track::new(black_box(&path)).expect("Failed to load track");
                track
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
        let (mut track, _) = Track::new(&path).expect("Failed to load track");
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
                let (mut track, _) = Track::new(black_box(&path)).expect("Failed to load track");
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
