use audio_decoder::AudioSource;
use audio_decoder::Source;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::path::PathBuf;
use std::time::Duration;

fn test_file_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches/test_data")
        .join(name)
}

fn bench_source_loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("source_loading");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac", "medium.flac", "long.flac"] {
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter(|| {
                let source = AudioSource::new(&path).expect("Could not create source");
                black_box(source.sample_rate());
            });
        });
    }

    group.finish();
}

fn bench_decode_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode_frame");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for name in ["short.flac"] {
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            let path = test_file_path(name);
            b.iter(|| {
                let source = AudioSource::new(&path).expect("Could not create source");
                let segments = source.decode_next_frame().expect("Failed to decode");
                black_box(segments.len());
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_source_loading, bench_decode_frame);
criterion_main!(benches);
