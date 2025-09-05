use clap::Parser;
use color_eyre::Result;
use flac_metadata::{discover_all_fields, extract_metadata, infer_from_path, TrackMetadata};
use std::collections::HashMap;
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the music library root directory
    library_path: PathBuf,

    /// Show detailed audio properties for each file
    #[arg(short, long)]
    verbose: bool,

    /// Discover and show all available metadata fields
    #[arg(short, long)]
    discover: bool,

    /// Show only files with missing essential metadata (artist or title)
    #[arg(short = 'm', long)]
    missing_only: bool,

    /// Try to infer metadata from file paths when tags are missing
    #[arg(short = 'i', long)]
    infer: bool,

    /// Show statistics about metadata fields across all files
    #[arg(short = 's', long)]
    stats: bool,
}

struct Statistics {
    total_files: usize,
    files_with_artist: usize,
    files_with_album: usize,
    files_with_title: usize,
    files_with_bpm: usize,
    files_with_key: usize,
    files_with_pictures: usize,
    files_incomplete: usize,
    total_duration_seconds: f64,
    total_size_bytes: u64,
    field_frequency: HashMap<String, usize>,
}

impl Statistics {
    fn new() -> Self {
        Self {
            total_files: 0,
            files_with_artist: 0,
            files_with_album: 0,
            files_with_title: 0,
            files_with_bpm: 0,
            files_with_key: 0,
            files_with_pictures: 0,
            files_incomplete: 0,
            total_duration_seconds: 0.0,
            total_size_bytes: 0,
            field_frequency: HashMap::new(),
        }
    }

    fn update(&mut self, metadata: &TrackMetadata, all_fields: Option<&HashMap<String, String>>) {
        self.total_files += 1;

        if metadata.artist.is_some() {
            self.files_with_artist += 1;
        }
        if metadata.album.is_some() {
            self.files_with_album += 1;
        }
        if metadata.title.is_some() {
            self.files_with_title += 1;
        }
        if metadata.bpm.is_some() {
            self.files_with_bpm += 1;
        }
        if metadata.key.is_some() {
            self.files_with_key += 1;
        }
        if metadata.has_picture {
            self.files_with_pictures += 1;
        }
        if metadata.is_incomplete() {
            self.files_incomplete += 1;
        }

        if let Some(duration) = metadata.duration {
            self.total_duration_seconds += duration.as_secs_f64();
        }

        if let Some(size) = metadata.file_size_bytes {
            self.total_size_bytes += size;
        }

        // Track field frequency
        if let Some(fields) = all_fields {
            for key in fields.keys() {
                *self.field_frequency.entry(key.clone()).or_insert(0) += 1;
            }
        }
    }

    fn print_summary(&self) {
        println!("\n{}", "=".repeat(60));
        println!("LIBRARY STATISTICS");
        println!("{}", "=".repeat(60));

        println!("\nFile Counts:");
        println!("  Total FLAC files:        {}", self.total_files);
        println!(
            "  Files with artist tag:   {} ({:.1}%)",
            self.files_with_artist,
            self.files_with_artist as f64 / self.total_files as f64 * 100.0
        );
        println!(
            "  Files with album tag:    {} ({:.1}%)",
            self.files_with_album,
            self.files_with_album as f64 / self.total_files as f64 * 100.0
        );
        println!(
            "  Files with title tag:    {} ({:.1}%)",
            self.files_with_title,
            self.files_with_title as f64 / self.total_files as f64 * 100.0
        );
        println!(
            "  Files with album art:    {} ({:.1}%)",
            self.files_with_pictures,
            self.files_with_pictures as f64 / self.total_files as f64 * 100.0
        );
        println!(
            "  Files missing metadata:  {} ({:.1}%)",
            self.files_incomplete,
            self.files_incomplete as f64 / self.total_files as f64 * 100.0
        );

        println!("\nDJ-Specific Metadata:");
        println!(
            "  Files with BPM:          {} ({:.1}%)",
            self.files_with_bpm,
            self.files_with_bpm as f64 / self.total_files as f64 * 100.0
        );
        println!(
            "  Files with KEY:          {} ({:.1}%)",
            self.files_with_key,
            self.files_with_key as f64 / self.total_files as f64 * 100.0
        );

        println!("\nLibrary Size:");
        let total_gb = self.total_size_bytes as f64 / 1_073_741_824.0;
        println!("  Total size:              {:.2} GB", total_gb);

        let total_hours = self.total_duration_seconds / 3600.0;
        let days = total_hours / 24.0;
        println!(
            "  Total duration:          {:.1} hours ({:.1} days)",
            total_hours, days
        );

        if self.total_files > 0 {
            let avg_size_mb =
                (self.total_size_bytes / self.total_files as u64) as f64 / 1_048_576.0;
            let avg_duration_min = (self.total_duration_seconds / self.total_files as f64) / 60.0;
            println!("  Average file size:       {:.1} MB", avg_size_mb);
            println!("  Average track length:    {:.1} minutes", avg_duration_min);
        }

        // Show most common metadata fields
        if !self.field_frequency.is_empty() {
            println!("\nMost Common Metadata Fields:");
            let mut sorted_fields: Vec<_> = self.field_frequency.iter().collect();
            sorted_fields.sort_by(|a, b| b.1.cmp(a.1));

            for (field, count) in sorted_fields.iter().take(10) {
                println!(
                    "  {:30} {} files ({:.1}%)",
                    field,
                    count,
                    **count as f64 / self.total_files as f64 * 100.0
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();

    if !args.library_path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "Library path does not exist: {}",
            args.library_path.display()
        ));
    }

    println!("🎵 Scanning music library: {}", args.library_path.display());
    println!("{}", "-".repeat(60));

    let mut stats = Statistics::new();
    let mut successful_extractions = 0;
    let mut failed_files = Vec::new();

    for entry in WalkDir::new(&args.library_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Only process FLAC files
        if path.extension().and_then(|s| s.to_str()) != Some("flac") {
            continue;
        }

        match extract_metadata(path) {
            Ok(mut metadata) => {
                successful_extractions += 1;

                // Try to infer from path if requested and metadata is incomplete
                if args.infer && metadata.is_incomplete() {
                    let (inferred_artist, inferred_album, inferred_title) = infer_from_path(path);

                    if metadata.artist.is_none() {
                        metadata.artist = inferred_artist;
                    }
                    if metadata.album.is_none() {
                        metadata.album = inferred_album;
                    }
                    if metadata.title.is_none() {
                        metadata.title = inferred_title;
                    }
                }

                // Get all fields if we need them for stats or discovery
                let all_fields = if args.discover || args.stats {
                    discover_all_fields(path).ok()
                } else {
                    None
                };

                // Update statistics
                stats.update(&metadata, all_fields.as_ref());

                // Skip if we're only showing missing and this one is complete
                if args.missing_only && !metadata.is_incomplete() {
                    continue;
                }

                // Print metadata
                print_metadata(&metadata, &args);

                // Print all discovered fields if requested
                if args.discover {
                    if let Some(ref fields) = all_fields {
                        print_all_fields(fields);
                    }
                }
            }
            Err(e) => {
                failed_files.push((path.to_path_buf(), e.to_string()));
            }
        }
    }

    // Print summary
    println!("\n{}", "=".repeat(60));
    println!("SCAN COMPLETE");
    println!("{}", "=".repeat(60));
    println!("  Successfully processed: {}", successful_extractions);
    println!("  Failed to process:      {}", failed_files.len());

    // Print failed files if any
    if !failed_files.is_empty() {
        println!("\n❌ Failed files:");
        for (path, error) in &failed_files {
            println!("  {} - {}", path.display(), error);
        }
    }

    // Print statistics if requested
    if args.stats {
        stats.print_summary();
    }

    Ok(())
}

fn print_metadata(metadata: &TrackMetadata, args: &Args) {
    let indicator = if metadata.is_incomplete() {
        "⚠️ "
    } else if metadata.has_picture {
        "🖼️ "
    } else {
        "✅"
    };

    println!("\n{} {}", indicator, metadata.file_path.display());
    println!("  {}", metadata.display_name());

    // Always show basic metadata if present
    if let Some(artist) = &metadata.artist {
        println!("  Artist:     {}", artist);
    }
    if let Some(album) = &metadata.album {
        println!("  Album:      {}", album);
    }
    if let Some(title) = &metadata.title {
        println!("  Title:      {}", title);
    }

    // Show duration and basic properties
    println!("  Duration:   {}", metadata.format_duration());

    // Show DJ metadata if present
    if metadata.bpm.is_some() || metadata.key.is_some() {
        println!("  DJ Metadata:");
        if let Some(bpm) = metadata.bpm {
            println!("    BPM:      {:.1}", bpm);
        }
        if let Some(key) = &metadata.key {
            println!("    Key:      {}", key);
        }
    }

    // Show detailed properties if verbose
    if args.verbose {
        println!("  Audio Properties:");
        if let Some(sample_rate) = metadata.sample_rate {
            println!("    Sample Rate:  {} Hz", sample_rate);
        }
        if let Some(channels) = metadata.channels {
            let channel_str = match channels {
                1 => "Mono",
                2 => "Stereo",
                _ => "Multi-channel",
            };
            println!("    Channels:     {} ({})", channels, channel_str);
        }
        if let Some(bit_depth) = metadata.bit_depth {
            println!("    Bit Depth:    {} bits", bit_depth);
        }
        if let Some(bitrate) = metadata.bitrate {
            println!("    Bitrate:      {} kbps", bitrate / 1000);
        }
        if let Some(size) = metadata.file_size_bytes {
            let size_mb = size as f64 / 1_048_576.0;
            println!("    File Size:    {:.1} MB", size_mb);
        }

        // Show additional metadata
        if let Some(genre) = &metadata.genre {
            println!("    Genre:        {}", genre);
        }
        if let Some(year) = metadata.year {
            println!("    Year:         {}", year);
        }
        if let Some(comment) = &metadata.comment {
            let truncated = if comment.len() > 50 {
                format!("{}...", &comment[..47])
            } else {
                comment.clone()
            };
            println!("    Comment:      {}", truncated);
        }
    }
}

fn print_all_fields(fields: &HashMap<String, String>) {
    if fields.is_empty() {
        println!("  📭 No metadata fields found");
        return;
    }

    println!("  📋 All metadata fields:");
    let mut sorted_fields: Vec<_> = fields.iter().collect();
    sorted_fields.sort_by_key(|&(key, _)| key);

    for (key, value) in sorted_fields {
        // Truncate very long values for display
        let display_value = if value.len() > 60 {
            format!("{}...", &value[..57])
        } else {
            value.clone()
        };
        println!("    {:<30} = {}", key, display_value);
    }
}
