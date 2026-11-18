//! Cost of folding a replica's `facts.jsonl` on the client (ADR-030 §6, P6).
//!
//! Usage: `fold_cost <facts.jsonl> [iterations=20] [artist]`
//!
//! Runs the real functions the client replica runs, stage by stage, and prints p50
//! and p95 per stage. `artist` is the filter of the second search; by default the
//! most common artist of the log.

use library_ipc_protocol::{StringQuery, TrackQuery};
use library_projection::{project_lines, search, sort_catalog, track_info, AlbumCovers};
use std::collections::HashMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

const STAGES: [&str; 8] = [
    "read file",
    "project_lines",
    "plays apply",
    "AlbumCovers::build",
    "search (default query)",
    "search (artist filter)",
    "sort_catalog",
    "total",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: fold_cost <facts.jsonl> [iterations=20] [artist]");
    let iterations: usize = args
        .next()
        .map(|n| n.parse().expect("iterations must be a number"))
        .unwrap_or(20);
    let artist_arg = args.next();

    print_machine();
    let text = std::fs::read_to_string(&path).expect("read facts file");
    println!(
        "replica: {path}\n  {} lines, {} bytes",
        text.lines().count(),
        text.len()
    );

    let artist = artist_arg.unwrap_or_else(|| most_common_artist(&text));
    println!("artist filter: {artist}\niterations: {iterations}\n");

    let mut samples: Vec<Vec<Duration>> = vec![Vec::new(); STAGES.len()];
    for _ in 0..iterations {
        let run_start = Instant::now();
        let mut lap = Instant::now();
        let mut record = |stage: usize, lap: &mut Instant| {
            samples[stage].push(lap.elapsed());
            *lap = Instant::now();
        };

        let text = std::fs::read_to_string(&path).expect("read facts file");
        record(0, &mut lap);

        let mut projection = project_lines(text.lines());
        record(1, &mut lap);

        projection.lineage.apply_to(&mut projection.tracks);
        record(2, &mut lap);

        let covers = AlbumCovers::build(&projection.tracks);
        record(3, &mut lap);

        let all = search(&projection.tracks, &covers, &TrackQuery::default());
        record(4, &mut lap);

        let by_artist = TrackQuery {
            artist: Some(StringQuery::Contains(artist.clone())),
            ..TrackQuery::default()
        };
        let found = search(&projection.tracks, &covers, &by_artist);
        record(5, &mut lap);

        // `search` sorts already; time the sort alone on an unsorted list.
        let mut unsorted: Vec<_> = projection
            .tracks
            .iter()
            .filter(|t| !t.is_hidden())
            .map(|t| track_info(t, &covers))
            .collect();
        lap = Instant::now();
        sort_catalog(&mut unsorted);
        record(6, &mut lap);

        black_box((&all, &found, &unsorted));
        samples[7].push(run_start.elapsed());
    }

    println!("{:<26}{:>12}{:>12}", "stage", "p50 ms", "p95 ms");
    for (name, mut durations) in STAGES.iter().zip(samples) {
        durations.sort();
        println!(
            "{name:<26}{:>12.1}{:>12.1}",
            ms(percentile(&durations, 50)),
            ms(percentile(&durations, 95))
        );
    }
}

/// Nearest-rank percentile of sorted `durations`.
fn percentile(durations: &[Duration], p: usize) -> Duration {
    let rank = (durations.len() * p).div_ceil(100).max(1);
    durations[rank - 1]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The artist with most tracks, found with the real fold (outside the timings).
fn most_common_artist(text: &str) -> String {
    let projection = project_lines(text.lines());
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for artist in projection.tracks.iter().filter_map(|t| t.artist.as_deref()) {
        *counts.entry(artist).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(artist, n)| (*n, *artist))
        .map(|(artist, _)| artist.to_owned())
        .unwrap_or_default()
}

fn print_machine() {
    let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, model)| model.trim().to_owned())
        .unwrap_or_else(|| "unknown cpu".to_owned());
    println!("machine: {} / {cpu}", host.trim());
}
