// bases/download_cli/src/output.rs
use std::path::Path;
use media_downloader::TrackMetadata;

pub struct OutputHandler {
    verbose: bool,
}

impl OutputHandler {
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    pub fn print_download_start(&self, url: &str) {
        println!("Starting download from: {}", url);
    }

    pub fn print_download_complete(&self, path: &Path, metadata: &TrackMetadata) {
        println!("Downloaded: {} to {}", metadata.location.title, path.display());
        println!("Artist: {}", metadata.location.artist);
        
        if let Some(album) = &metadata.location.album {
            println!("Album: {}", album);
        }
        println!("Duration: {:.1} seconds", metadata.duration);
        
        if self.verbose {
            println!("Source: {}", metadata.source_url);
            println!("Download time: {}", metadata.download_time);
        }
    }

    pub fn print_error(&self, error: &color_eyre::Report) {
        eprintln!("Error: {}", error);
        
        if self.verbose {
            eprintln!("\nError details:");
            error.chain().skip(1).for_each(|cause| {
                eprintln!("  caused by: {}", cause);
            });
        }
    }
}
