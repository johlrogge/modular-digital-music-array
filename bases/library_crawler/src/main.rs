use clap::Parser;
use color_eyre::Result;
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the music library root directory
    library_path: PathBuf,
    
    /// Show detailed output for each file
    #[arg(short, long)]
    verbose: bool,
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
    
    println!("Crawling music library: {}", args.library_path.display());
    println!();
    
    let mut total_files = 0;
    let mut successful_extractions = 0;
    
    for entry in WalkDir::new(&args.library_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        
        // Only process FLAC files
        if path.extension().and_then(|s| s.to_str()) == Some("flac") {
            total_files += 1;
            
            match flac_metadata::extract_metadata(path) {
                Ok(metadata) => {
                    successful_extractions += 1;
                    print_metadata(&metadata, args.verbose);
                }
                Err(e) => {
                    eprintln!("Error processing {}: {}", path.display(), e);
                }
            }
        }
    }
    
    println!();
    println!("Summary:");
    println!("  Total FLAC files found: {}", total_files);
    println!("  Successfully processed: {}", successful_extractions);
    println!("  Failed: {}", total_files - successful_extractions);
    
    Ok(())
}

fn print_metadata(metadata: &flac_metadata::TrackMetadata, verbose: bool) {
    println!("Found: {}", metadata.file_path.display());
    
    if let Some(artist) = &metadata.artist {
        println!("  Artist: {}", artist);
    }
    
    if let Some(album) = &metadata.album {
        println!("  Album: {}", album);
    }
    
    if let Some(title) = &metadata.title {
        println!("  Title: {}", title);
    }
    
    println!("  Duration: {}", metadata.format_duration());
    
    if verbose {
        if let Some(sample_rate) = metadata.sample_rate {
            println!("  Sample Rate: {} Hz", sample_rate);
        }
        
        if let Some(channels) = metadata.channels {
            println!("  Channels: {}", channels);
        }
    }
    
    println!();
}