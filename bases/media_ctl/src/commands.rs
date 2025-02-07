use clap::Subcommand;
use color_eyre::Result;
use std::path::PathBuf;
use media_client::Channel;

#[derive(Subcommand)]
pub enum Commands {
    /// Load a track from the library
    Load {
        /// Path to music library
        #[arg(long)]
        library: PathBuf,
        
        /// Artist name
        #[arg(long)]
        artist: String,
        
        /// Album name (optional)
        #[arg(long)]
        album: Option<String>,
        
        /// Song name
        #[arg(long)]
        song: String,
        
        /// Channel (A or B)
        #[arg(long)]
        channel: char,
    },
    
    /// Play a loaded track
    Play {
        /// Channel (A or B)
        #[arg(long)]
        channel: char,
    },
    
    /// Stop a playing track
    Stop {
        /// Channel (A or B)
        #[arg(long)]
        channel: char,
    },
    
    /// Set volume for a channel
    Volume {
        /// Channel (A or B)
        #[arg(long)]
        channel: char,
        
        /// Volume in dB (-inf to 0)
        #[arg(long)]
        db: f32,
    },
}

pub fn parse_channel(c: char) -> Result<Channel> {
    match c.to_uppercase().next() {
        Some('A') => Ok(Channel::A),
        Some('B') => Ok(Channel::B),
        _ => Err(color_eyre::eyre::eyre!("Invalid channel. Use 'A' or 'B'"))
    }
}

pub fn channel_to_string(channel: Channel) -> &'static str {
    match channel {
        Channel::A => "A",
        Channel::B => "B",
    }
}

pub fn construct_path(library: PathBuf, artist: String, album: Option<String>, song: String) -> Result<PathBuf> {
    let mut path = library;
    path.push(artist);
    if let Some(album) = album {
        path.push(album);
    }
    path.push(format!("{}.flac", song));
    
    if !path.exists() {
        return Err(color_eyre::eyre::eyre!("Track not found: {}", path.display()));
    }
    
    Ok(path)
}