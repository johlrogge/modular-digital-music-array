mod fact_generator;
mod fact_reader;
mod fact_writer;
mod hash;

use chrono::Utc;
use clap::Parser;
use color_eyre::Result;
use fact_generator::generate_facts;
use flac_metadata::{discover_all_fields, extract_metadata};
use hash::compute_content_hash;
use stainless_facts::{Fact, FactStreamWriter, Operation};
use std::path::PathBuf;
use walkdir::WalkDir;

use crate::fact_reader::{read_and_aggregate, AggregatedTrack};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the music library root directory (for scanning)
    library_path: Option<PathBuf>,

    /// Write facts to this file (JSONL format)
    #[arg(long)]
    write_facts: Option<PathBuf>,

    /// Read facts from this file and aggregate
    #[arg(long)]
    read_facts: Option<PathBuf>,

    /// Show aggregated tracks (use with --read-facts)
    #[arg(long)]
    aggregate: bool,

    /// Show progress while scanning
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    // Mode 1: Read and aggregate facts
    if let Some(ref facts_path) = args.read_facts {
        return read_and_display_facts(facts_path, args.aggregate);
    }

    // Mode 2: Scan library and write facts
    if let Some(ref library_path) = args.library_path {
        if let Some(ref output_path) = args.write_facts {
            return scan_and_write_facts(library_path, output_path, args.verbose);
        } else {
            eprintln!("Error: --write-facts is required when scanning a library");
            eprintln!("Usage: library-crawler <LIBRARY_PATH> --write-facts <OUTPUT>");
            std::process::exit(1);
        }
    }

    eprintln!("Error: Must provide either:");
    eprintln!("  1. <LIBRARY_PATH> --write-facts <OUTPUT>  (to scan and generate facts)");
    eprintln!("  2. --read-facts <INPUT> --aggregate       (to read and display facts)");
    std::process::exit(1);
}

/// Scan library and write facts to file
fn scan_and_write_facts(
    library_path: &PathBuf,
    output_path: &PathBuf,
    verbose: bool,
) -> Result<()> {
    if !library_path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "Library path does not exist: {}",
            library_path.display()
        ));
    }

    println!("🎵 Scanning music library: {}", library_path.display());
    println!("📝 Writing facts to: {}", output_path.display());
    println!("{}", "-".repeat(60));

    let mut writer = FactStreamWriter::open(output_path)?;
    let mut track_count = 0;
    let mut total_facts = 0;
    let mut failed_files = Vec::new();

    for entry in WalkDir::new(library_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Only process FLAC files
        if path.extension().and_then(|s| s.to_str()) != Some("flac") {
            continue;
        }

        if verbose {
            println!("Processing: {}", path.display());
        }

        match process_file(path, &mut writer) {
            Ok(fact_count) => {
                track_count += 1;
                total_facts += fact_count;

                if verbose {
                    println!("  ✓ Generated {} facts", fact_count);
                }
            }
            Err(e) => {
                failed_files.push((path.to_path_buf(), e.to_string()));
                if verbose {
                    eprintln!("  ✗ Error: {}", e);
                }
            }
        }
    }

    println!("\n{}", "=".repeat(60));
    println!("SCAN COMPLETE");
    println!("{}", "=".repeat(60));
    println!("  Tracks processed:  {}", track_count);
    println!("  Total facts:       {}", total_facts);
    println!(
        "  Facts per track:   {:.1}",
        total_facts as f64 / track_count as f64
    );
    println!("  Failed files:      {}", failed_files.len());

    if !failed_files.is_empty() {
        println!("\n❌ Failed files:");
        for (path, error) in &failed_files {
            println!("  {} - {}", path.display(), error);
        }
    }

    Ok(())
}

/// Process a single FLAC file and write its facts
fn process_file(path: &std::path::Path, writer: &mut FactStreamWriter) -> Result<usize> {
    // Compute content hash (entity ID)
    let content_hash = compute_content_hash(path)?;

    // Extract metadata
    let metadata = extract_metadata(path)?;

    // Discover all fields
    let all_fields = discover_all_fields(path)?;

    // Generate facts
    let facts_and_sources = generate_facts(content_hash.clone(), &metadata, &all_fields)?;

    let now = Utc::now();
    let fact_count = facts_and_sources.len();

    // Write facts
    for (value, source) in facts_and_sources {
        let fact = Fact::new(content_hash.clone(), value, now, source, Operation::Assert);
        writer.write_batch(&[fact])?;
    }
    Ok(fact_count)
}

/// Read facts from file and display them
fn read_and_display_facts(facts_path: &PathBuf, show_aggregate: bool) -> Result<()> {
    if !facts_path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "Facts file does not exist: {}",
            facts_path.display()
        ));
    }

    println!("📖 Reading facts from: {}", facts_path.display());
    println!("{}", "-".repeat(60));

    let tracks = read_and_aggregate(facts_path)?;

    println!("\n{}", "=".repeat(60));
    println!("AGGREGATION COMPLETE");
    println!("{}", "=".repeat(60));
    println!("  Total tracks: {}", tracks.len());

    let total_facts: usize = tracks.values().map(|t| t.fact_count).sum();
    println!("  Total facts:  {}", total_facts);
    println!(
        "  Facts per track (avg): {:.1}",
        total_facts as f64 / tracks.len() as f64
    );

    if show_aggregate {
        println!("\n{}", "=".repeat(60));
        println!("TRACKS");
        println!("{}", "=".repeat(60));

        let mut sorted_tracks: Vec<_> = tracks.values().collect();
        sorted_tracks.sort_by(|a, b| a.display_name().cmp(&b.display_name()));

        for track in sorted_tracks {
            print_aggregated_track(track);
        }
    }

    Ok(())
}

/// Print an aggregated track with all its information
fn print_aggregated_track(track: &AggregatedTrack) {
    println!("\n{}", track.display_name());
    println!("  Entity: {}", track.entity.as_ref().unwrap().0);
    println!("  Facts:  {}", track.fact_count);

    if let Some(ref path) = track.file_path {
        println!("  Path:   {}", path.display());
    }

    // Basic metadata
    if let Some(ref artist) = track.artist {
        println!("  Artist: {}", artist);
    }
    if let Some(ref album) = track.album {
        println!("  Album:  {}", album);
    }
    if let Some(ref title) = track.title {
        println!("  Title:  {}", title);
    }

    // DJ metadata
    if let Some(ref bpm) = track.bpm {
        println!("  BPM:    {}", bpm);
    }
    if let Some(ref key) = track.key {
        println!("  Key:    {}", key);
    }

    // Genre
    if let Some(ref genre) = track.main_genre {
        print!("  Genre:  {}", genre);
        if !track.style_descriptors.is_empty() {
            print!(" ({})", track.style_descriptors.join(", "));
        }
        println!();
    }

    // Catalog
    if let Some(ref label) = track.label {
        println!("  Label:  {}", label);
    }
    if let Some(ref isrc) = track.isrc {
        println!("  ISRC:   {}", isrc);
    }

    // Audio properties
    println!("  Duration: {}", track.format_duration());
    if let Some(sr) = track.sample_rate {
        print!("  Audio:    {}Hz", sr);
        if let Some(bd) = track.bit_depth {
            print!(", {}bit", bd);
        }
        if let Some(ch) = track.channels {
            let ch_str = match ch {
                1 => "Mono",
                2 => "Stereo",
                _ => "Multi",
            };
            print!(", {}", ch_str);
        }
        println!();
    }

    println!("  Size:     {}", track.format_file_size());

    if track.has_album_art {
        println!("  🖼️  Has album art");
    }
}
