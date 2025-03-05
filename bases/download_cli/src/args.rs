// bases/download_cli/src/args.rs
use clap::Parser;
use std::path::PathBuf;

/// Download audio tracks from various sources
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Directory to store downloaded files
    #[arg(short, long)]
    pub output_dir: PathBuf,

    /// URL to download from
    pub url: String,

    /// Download entire playlist if URL is a playlist
    #[arg(short, long)]
    pub playlist: bool,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,
}
