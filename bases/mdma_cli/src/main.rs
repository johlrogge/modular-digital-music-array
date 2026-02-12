//! MDMA CLI - Command line interface for mdma-library
//!
//! Connects to the library service via nng IPC

use clap::{Parser, Subcommand};
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// =============================================================================
// IPC Message Types (mirror of mdma-library/src/ipc.rs)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LibraryRequest {
    GetStatus,
    ListTracks { limit: Option<usize> },
    GetTrack { hash: String },
    Search { query: String },
    GetInboxQueue,
    IngestFile { path: PathBuf },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub content_hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_seconds: Option<u32>,
    pub bpm: Option<f32>,
    pub key: Option<String>,
    pub blob_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub version: String,
    pub tracks_indexed: usize,
    pub facts_count: usize,
    pub inbox_queue_size: usize,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum LibraryResponse {
    Status(ServiceStatus),
    Tracks(Vec<TrackInfo>),
    Track(Option<TrackInfo>),
    SearchResults(Vec<TrackInfo>),
    InboxQueue(Vec<PathBuf>),
    IngestResult {
        hash: Option<String>,
        success: bool,
        message: String,
    },
    Pong,
    Error {
        message: String,
    },
}

// =============================================================================
// CLI Definition
// =============================================================================

#[derive(Parser, Debug)]
#[command(name = "mdma")]
#[command(author, version, about = "MDMA CLI - Control the music library")]
struct Cli {
    /// IPC socket address
    #[arg(long, default_value = "ipc:///run/mdma/library.sock", global = true)]
    socket: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Check if the service is running
    Ping,

    /// Get service status
    Status,

    /// List tracks in library
    List {
        /// Maximum number of tracks to show
        #[arg(short, long)]
        limit: Option<usize>,
    },

    /// Get a specific track by hash
    Get {
        /// Content hash (with or without sha256: prefix)
        hash: String,
    },

    /// Search for tracks
    Search {
        /// Search query
        query: String,
    },

    /// Show inbox queue
    Inbox,

    /// Ingest a file
    Ingest {
        /// Path to file to ingest
        path: PathBuf,
    },
}

// =============================================================================
// IPC Client
// =============================================================================

struct Client {
    socket: nng::Socket,
}

impl Client {
    fn connect(address: &str) -> Result<Self> {
        let socket = nng::Socket::new(nng::Protocol::Req0)?;
        socket.dial(address)?;
        Ok(Self { socket })
    }

    fn request(&self, request: &LibraryRequest) -> Result<LibraryResponse> {
        let data = serde_json::to_vec(request)?;
        let msg = nng::Message::from(&data[..]);
        self.socket.send(msg).map_err(|(_, e)| e)?;

        let response_msg = self.socket.recv()?;
        let response: LibraryResponse = serde_json::from_slice(&response_msg)?;
        Ok(response)
    }
}

// =============================================================================
// Command Handlers
// =============================================================================

fn handle_ping(client: &Client) -> Result<()> {
    match client.request(&LibraryRequest::Ping)? {
        LibraryResponse::Pong => {
            println!("pong - service is alive");
            Ok(())
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

fn handle_status(client: &Client) -> Result<()> {
    match client.request(&LibraryRequest::GetStatus)? {
        LibraryResponse::Status(status) => {
            println!("MDMA Library Service v{}", status.version);
            println!("─────────────────────────────");
            println!("Tracks indexed:  {}", status.tracks_indexed);
            println!("Facts count:     {}", status.facts_count);
            println!("Inbox queue:     {} files", status.inbox_queue_size);
            println!("Uptime:          {} seconds", status.uptime_seconds);
            Ok(())
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

fn handle_list(client: &Client, limit: Option<usize>) -> Result<()> {
    match client.request(&LibraryRequest::ListTracks { limit })? {
        LibraryResponse::Tracks(tracks) => {
            if tracks.is_empty() {
                println!("No tracks in library");
                return Ok(());
            }

            println!("Tracks in library ({}):", tracks.len());
            println!("─────────────────────────────────────────────────────────────────");

            for track in tracks {
                let title = track.title.as_deref().unwrap_or("Unknown");
                let artist = track.artist.as_deref().unwrap_or("Unknown");
                let hash_short = &track.content_hash[7..15]; // Skip "sha256:" and take 8 chars

                println!("{} │ {} - {}", hash_short, artist, title);
            }
            Ok(())
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

fn handle_get(client: &Client, hash: String) -> Result<()> {
    // Normalize hash (add prefix if missing)
    let hash = if hash.starts_with("sha256:") {
        hash
    } else {
        format!("sha256:{}", hash)
    };

    match client.request(&LibraryRequest::GetTrack { hash })? {
        LibraryResponse::Track(Some(track)) => {
            println!("Track: {}", track.content_hash);
            println!("─────────────────────────────────────────────────────────────────");
            if let Some(title) = track.title {
                println!("Title:    {}", title);
            }
            if let Some(artist) = track.artist {
                println!("Artist:   {}", artist);
            }
            if let Some(album) = track.album {
                println!("Album:    {}", album);
            }
            if let Some(duration) = track.duration_seconds {
                println!("Duration: {}:{:02}", duration / 60, duration % 60);
            }
            if let Some(bpm) = track.bpm {
                println!("BPM:      {:.1}", bpm);
            }
            if let Some(key) = track.key {
                println!("Key:      {}", key);
            }
            if let Some(path) = track.blob_path {
                println!("Path:     {}", path.display());
            }
            Ok(())
        }
        LibraryResponse::Track(None) => {
            println!("Track not found");
            std::process::exit(1);
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

fn handle_search(client: &Client, query: String) -> Result<()> {
    match client.request(&LibraryRequest::Search {
        query: query.clone(),
    })? {
        LibraryResponse::SearchResults(tracks) => {
            if tracks.is_empty() {
                println!("No tracks found matching '{}'", query);
                return Ok(());
            }

            println!("Search results for '{}' ({} matches):", query, tracks.len());
            println!("─────────────────────────────────────────────────────────────────");

            for track in tracks {
                let title = track.title.as_deref().unwrap_or("Unknown");
                let artist = track.artist.as_deref().unwrap_or("Unknown");
                let hash_short = &track.content_hash[7..15];

                println!("{} │ {} - {}", hash_short, artist, title);
            }
            Ok(())
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

fn handle_inbox(client: &Client) -> Result<()> {
    match client.request(&LibraryRequest::GetInboxQueue)? {
        LibraryResponse::InboxQueue(files) => {
            if files.is_empty() {
                println!("Inbox is empty");
                return Ok(());
            }

            println!("Inbox queue ({} files):", files.len());
            println!("─────────────────────────────────────────────────────────────────");

            for file in files {
                println!("  {}", file.display());
            }
            Ok(())
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

fn handle_ingest(client: &Client, path: PathBuf) -> Result<()> {
    // Convert to absolute path
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    println!("Ingesting: {}", path.display());

    match client.request(&LibraryRequest::IngestFile { path })? {
        LibraryResponse::IngestResult {
            hash,
            success,
            message,
        } => {
            if success {
                if let Some(hash) = hash {
                    println!("Success: {}", hash);
                } else {
                    println!("Success: {}", message);
                }
            } else {
                eprintln!("Failed: {}", message);
                std::process::exit(1);
            }
            Ok(())
        }
        LibraryResponse::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        other => {
            eprintln!("Unexpected response: {:?}", other);
            std::process::exit(1);
        }
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    // Connect to service
    let client = match Client::connect(&cli.socket) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to service at {}: {}", cli.socket, e);
            eprintln!("Is mdma-library running?");
            std::process::exit(1);
        }
    };

    // Dispatch command
    match cli.command {
        Commands::Ping => handle_ping(&client),
        Commands::Status => handle_status(&client),
        Commands::List { limit } => handle_list(&client, limit),
        Commands::Get { hash } => handle_get(&client, hash),
        Commands::Search { query } => handle_search(&client, query),
        Commands::Inbox => handle_inbox(&client),
        Commands::Ingest { path } => handle_ingest(&client, path),
    }
}
