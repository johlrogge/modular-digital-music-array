//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to the library and bandcamp services via nng IPC

use bandcamp_ipc_client::{
    BandcampClient, BandcampUsername, ClientError as BandcampClientError,
    ProtocolError as BandcampProtocolError,
};
use clap::{Parser, Subcommand};
use color_eyre::Result;
use library_ipc_client::{
    ClientError, ContentHash, InboxPath, LibraryClient, ProtocolError, TrackInfo,
};
use media_client::{Deck, MediaClient};

// =============================================================================
// CLI Definition
// =============================================================================

#[derive(Parser, Debug)]
#[command(name = "mdma")]
#[command(author, version, about = "MDMA CLI - Control the music services")]
struct Cli {
    /// Library IPC socket address
    #[arg(
        long,
        default_value = "ipc:///run/mdma/library.sock",
        global = true,
        env = "MDMA_LIBRARY_SOCKET"
    )]
    socket: String,

    /// Bandcamp IPC socket address
    #[arg(
        long,
        default_value = "ipc:///run/mdma/bandcamp.sock",
        global = true,
        env = "MDMA_BANDCAMP_SOCKET"
    )]
    bandcamp_socket: String,

    /// Playback server socket address
    #[arg(
        long,
        default_value = "ipc:///run/mdma/playback.sock",
        global = true,
        env = "MDMA_PLAYBACK_SOCKET"
    )]
    playback_socket: String,

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

    /// Get a specific track by hash (supports partial hashes like git)
    Get {
        /// Content hash (full or partial, with or without sha256: prefix)
        hash: String,
    },

    /// Show all facts for a track
    Facts {
        /// Content hash (full or partial, with or without sha256: prefix)
        hash: String,
    },

    /// Search for tracks
    Search {
        /// Search query
        query: String,
    },

    /// Inbox management commands
    Inbox {
        #[command(subcommand)]
        command: InboxCommands,
    },

    /// Bandcamp download commands
    Bandcamp {
        #[command(subcommand)]
        command: BandcampCommands,
    },

    /// Playback control commands
    Playback {
        #[command(subcommand)]
        command: PlaybackCommands,
    },

    /// Queue management (feeds deck A)
    Queue {
        #[command(subcommand)]
        command: QueueCommands,
    },
}

#[derive(Subcommand, Debug)]
enum InboxCommands {
    /// List files in inbox
    List,

    /// Delete a file from inbox without ingesting
    Delete {
        /// File name in inbox
        filename: String,
    },

    /// Ingest a specific file from inbox
    Ingest {
        /// File name in inbox
        filename: String,
    },

    /// Ingest all files in inbox
    IngestAll,
}

#[derive(Subcommand, Debug)]
enum BandcampCommands {
    /// Check if the bandcamp service is running
    Ping,

    /// Get bandcamp service status
    Status,

    /// Reload cookies from disk
    ReloadCookies,

    /// Sync a user's Bandcamp collection
    Sync {
        /// Bandcamp username
        username: String,
    },

    /// List current downloads
    Downloads,

    /// Cancel a download
    Cancel {
        /// Item ID to cancel
        id: String,
    },

    /// Pause all downloads
    Pause,

    /// Resume downloads
    Resume,
}

#[derive(Subcommand, Debug)]
enum PlaybackCommands {
    /// Play from the queue (use `mdma queue append <hash>` to enqueue tracks)
    Play,

    /// Stop playback on deck A
    Stop,
}

#[derive(Subcommand, Debug)]
enum QueueCommands {
    /// Prepend a track to the front of the queue (plays next).
    /// Hash may be omitted; if so, reads "{hash}  {display}" from stdin (dmenu output).
    Next {
        /// Content hash (full or partial). Omit to read from stdin.
        hash: Option<String>,
    },

    /// Append a track to the end of the queue.
    /// Hash may be omitted; if so, reads "{hash}  {display}" from stdin (dmenu output).
    Append {
        /// Content hash (full or partial). Omit to read from stdin.
        hash: Option<String>,
    },

    /// Show the current queue
    List,

    /// Clear the queue
    Clear,
}

// =============================================================================
// Display Helpers
// =============================================================================

/// Get a short hash for display (8 chars after sha256: prefix)
fn short_hash(hash: &ContentHash) -> &str {
    let clean = hash.0.strip_prefix("sha256:").unwrap_or(&hash.0);
    if clean.len() >= 8 {
        &clean[..8]
    } else {
        clean
    }
}

/// Format track for single-line display
fn format_track_line(track: &TrackInfo) -> String {
    let title = track.title.as_deref().unwrap_or("Unknown");
    let artist = track.artist.as_deref().unwrap_or("Unknown");
    format!("{} - {}", artist, title)
}

/// Handle library client errors uniformly
fn handle_error(err: ClientError) -> ! {
    match err {
        ClientError::Protocol(ProtocolError::TrackNotFound { hash }) => {
            eprintln!("Track not found: {}", hash);
        }
        ClientError::Protocol(ProtocolError::InboxFileNotFound { path }) => {
            eprintln!("Inbox file not found: {}", path);
        }
        ClientError::Protocol(e) => {
            eprintln!("Error: {}", e);
        }
        ClientError::Connection(e) => {
            eprintln!("Connection failed: {}", e);
            eprintln!("Is mdma-library running?");
        }
        e => {
            eprintln!("Error: {}", e);
        }
    }
    std::process::exit(1);
}

/// Handle bandcamp client errors uniformly
fn handle_bandcamp_error(err: BandcampClientError) -> ! {
    match err {
        BandcampClientError::Protocol(BandcampProtocolError::NotAuthenticated { message }) => {
            eprintln!("Not authenticated: {}", message);
            eprintln!("Upload cookies to /etc/mdma/bandcamp-cookies.json");
        }
        BandcampClientError::Protocol(e) => {
            eprintln!("Error: {}", e);
        }
        BandcampClientError::Connection(e) => {
            eprintln!("Connection failed: {}", e);
            eprintln!("Is mdma-bandcamp running?");
        }
        e => {
            eprintln!("Error: {}", e);
        }
    }
    std::process::exit(1);
}

// =============================================================================
// Command Handlers
// =============================================================================

fn handle_ping(client: &LibraryClient) -> Result<()> {
    match client.ping() {
        Ok(()) => {
            println!("pong - service is alive");
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_status(client: &LibraryClient) -> Result<()> {
    match client.status() {
        Ok(status) => {
            println!("MDMA Library Service v{}", status.version);
            println!("{}", "=".repeat(35));
            println!("Tracks indexed:  {}", status.tracks_indexed);
            println!("Facts count:     {}", status.facts_count);
            println!("Inbox queue:     {} files", status.inbox_queue_size);
            println!("Uptime:          {} seconds", status.uptime_seconds);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_list(client: &LibraryClient, limit: Option<usize>) -> Result<()> {
    match client.list_tracks(limit) {
        Ok(tracks) => {
            if tracks.is_empty() {
                println!("No tracks in library");
                return Ok(());
            }

            println!("Tracks in library ({}):", tracks.len());
            println!("{}", "=".repeat(65));

            for track in tracks {
                println!(
                    "{} | {}",
                    short_hash(&track.content_hash),
                    format_track_line(&track)
                );
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_get(client: &LibraryClient, hash: String) -> Result<()> {
    let content_hash = ContentHash(hash);
    match client.get_track(&content_hash) {
        Ok(track) => {
            println!("Track: {}", track.content_hash.0);
            println!("{}", "=".repeat(65));
            if let Some(title) = track.title {
                println!("Title:    {}", title);
            }
            if let Some(artist) = track.artist {
                println!("Artist:   {}", artist);
            }
            if let Some(album) = track.album {
                println!("Album:    {}", album);
            }
            if let Some(duration) = track.duration {
                println!("Duration: {}", duration);
            }
            if let Some(bpm) = track.bpm {
                println!("BPM:      {}", bpm);
            }
            if let Some(key) = track.key {
                println!("Key:      {}", key);
            }
            if let Some(path) = track.blob_path {
                println!("Path:     {}", path);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_facts(client: &LibraryClient, hash: String) -> Result<()> {
    let content_hash = ContentHash(hash);
    match client.get_facts(&content_hash) {
        Ok((full_hash, facts)) => {
            println!("Facts for: {}", full_hash.0);
            println!("{}", "=".repeat(65));
            for (fact_type, fact_value) in facts {
                println!("{:20} | {}", fact_type, fact_value);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_search(client: &LibraryClient, query: String) -> Result<()> {
    use std::io::IsTerminal;
    match client.search(&query) {
        Ok(tracks) => {
            if std::io::stdout().is_terminal() {
                if tracks.is_empty() {
                    println!("No tracks found matching '{}'", query);
                    return Ok(());
                }
                println!("Search results for '{}' ({} matches):", query, tracks.len());
                println!("{}", "=".repeat(65));
                for track in tracks {
                    println!(
                        "{} | {}",
                        short_hash(&track.content_hash),
                        format_track_line(&track)
                    );
                }
            } else {
                // Pipe / dmenu mode: "{short_hash}  {display}" — one per line, no header.
                // The receiving command reads the first whitespace-delimited token as the hash.
                for track in tracks {
                    println!(
                        "{}  {}",
                        short_hash(&track.content_hash),
                        format_track_line(&track)
                    );
                }
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_inbox_list(client: &LibraryClient) -> Result<()> {
    match client.inbox_queue() {
        Ok(files) => {
            if files.is_empty() {
                println!("Inbox is empty");
                return Ok(());
            }

            println!("Inbox queue ({} files):", files.len());
            println!("{}", "=".repeat(65));

            for file in files {
                println!("  {}", file.as_str());
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_inbox_delete(client: &LibraryClient, filename: String) -> Result<()> {
    let path = match InboxPath::new(&filename) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Invalid filename: {}", e);
            std::process::exit(1);
        }
    };

    match client.delete_inbox_file(&path) {
        Ok(result) => {
            if result.success {
                println!("{}", result.message);
            } else {
                eprintln!("Failed: {}", result.message);
                std::process::exit(1);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_inbox_ingest(client: &LibraryClient, filename: String) -> Result<()> {
    let path = match InboxPath::new(&filename) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Invalid filename: {}", e);
            std::process::exit(1);
        }
    };

    println!("Ingesting: {}", filename);

    match client.ingest_file(&path) {
        Ok(result) => {
            if result.success {
                if let Some(hash) = result.hash {
                    println!("Success: {}", hash.0);
                } else {
                    println!("Success: {}", result.message);
                }
            } else {
                eprintln!("Failed: {}", result.message);
                std::process::exit(1);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_inbox_ingest_all(client: &LibraryClient) -> Result<()> {
    println!("Ingesting all files in inbox...");

    match client.ingest_all() {
        Ok(results) => {
            if results.is_empty() {
                println!("Inbox is empty - nothing to ingest");
                return Ok(());
            }

            let mut success_count = 0;
            let mut fail_count = 0;

            for item in results {
                if item.result.success {
                    success_count += 1;
                    if let Some(hash) = item.result.hash {
                        println!("  OK: {} -> {}", item.path.as_str(), short_hash(&hash));
                    } else {
                        println!("  OK: {}", item.path.as_str());
                    }
                } else {
                    fail_count += 1;
                    println!("  FAIL: {} - {}", item.path.as_str(), item.result.message);
                }
            }

            println!();
            println!("Done: {} succeeded, {} failed", success_count, fail_count);

            if fail_count > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

// =============================================================================
// Bandcamp Command Handlers
// =============================================================================

fn handle_bandcamp_ping(client: &BandcampClient) -> Result<()> {
    match client.ping() {
        Ok(()) => {
            println!("pong - bandcamp service is alive");
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_status(client: &BandcampClient) -> Result<()> {
    match client.status() {
        Ok(status) => {
            println!("MDMA Bandcamp Service v{}", status.version);
            println!("{}", "=".repeat(40));
            println!(
                "Cookies loaded:    {}",
                if status.cookies_loaded { "yes" } else { "no" }
            );
            if let Some(user) = status.current_username {
                println!("Current user:      {}", user);
            }
            println!("Downloads active:  {}", status.downloads_active);
            println!("Downloads queued:  {}", status.downloads_queued);
            println!("Downloads done:    {}", status.downloads_completed);
            println!("Downloads failed:  {}", status.downloads_failed);
            println!("Uptime:            {} seconds", status.uptime_seconds);
            println!(
                "Paused:            {}",
                if status.paused { "yes" } else { "no" }
            );
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_reload_cookies(client: &BandcampClient) -> Result<()> {
    match client.reload_cookies() {
        Ok((valid, message)) => {
            if valid {
                println!("Cookies reloaded successfully");
            } else {
                eprintln!("Failed to reload cookies: {}", message);
                std::process::exit(1);
            }
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_sync(client: &BandcampClient, username: String) -> Result<()> {
    let username = match BandcampUsername::new(&username) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Invalid username: {}", e);
            std::process::exit(1);
        }
    };

    println!("Syncing collection for {}...", username);

    match client.sync(&username) {
        Ok((user, total, new)) => {
            println!("Sync started for {}", user);
            println!("Total items: {}, New items: {}", total, new);
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_downloads(client: &BandcampClient) -> Result<()> {
    match client.list_downloads() {
        Ok(downloads) => {
            if downloads.is_empty() {
                println!("No downloads in progress");
                return Ok(());
            }

            println!("Downloads ({}):", downloads.len());
            println!("{}", "=".repeat(65));

            for dl in downloads {
                let progress = if let Some(total) = dl.total_bytes {
                    let pct = (dl.downloaded_bytes as f64 / total as f64) * 100.0;
                    format!("{:.1}%", pct)
                } else {
                    format!("{} bytes", dl.downloaded_bytes)
                };

                let status = match dl.state {
                    bandcamp_ipc_client::DownloadState::Queued => "queued".to_string(),
                    bandcamp_ipc_client::DownloadState::Downloading => {
                        format!("downloading {}", progress)
                    }
                    bandcamp_ipc_client::DownloadState::Extracting => "extracting".to_string(),
                    bandcamp_ipc_client::DownloadState::Moving => "moving".to_string(),
                    bandcamp_ipc_client::DownloadState::Completed => "completed".to_string(),
                    bandcamp_ipc_client::DownloadState::Failed => {
                        format!("failed: {}", dl.error.unwrap_or_default())
                    }
                    bandcamp_ipc_client::DownloadState::Cancelled => "cancelled".to_string(),
                };

                println!("{} | {} - {} | {}", dl.id, dl.artist, dl.title, status);
            }
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_cancel(client: &BandcampClient, id: String) -> Result<()> {
    let item_id = bandcamp_ipc_client::ItemId::new(&id);
    match client.cancel_download(&item_id) {
        Ok(()) => {
            println!("Cancelled download: {}", id);
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_pause(client: &BandcampClient) -> Result<()> {
    match client.pause() {
        Ok(()) => {
            println!("Downloads paused");
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

fn handle_bandcamp_resume(client: &BandcampClient) -> Result<()> {
    match client.resume() {
        Ok(()) => {
            println!("Downloads resumed");
            Ok(())
        }
        Err(e) => handle_bandcamp_error(e),
    }
}

// =============================================================================
// Playback Command Handlers
// =============================================================================

fn handle_playback_error(err: media_client::ClientError) -> ! {
    match err {
        media_client::ClientError::Connection(e) => {
            eprintln!("Connection failed: {}", e);
            eprintln!("Is mdma-playback running?");
        }
        e => {
            eprintln!("Error: {}", e);
        }
    }
    std::process::exit(1);
}

fn handle_playback_play(media_client: &MediaClient) -> Result<()> {
    if let Err(e) = media_client.play_queue() {
        handle_playback_error(e);
    }
    println!("Playing from queue");
    Ok(())
}

fn handle_queue_next(
    library_client: &LibraryClient,
    media_client: &MediaClient,
    hash: String,
) -> Result<()> {
    let path = resolve_blob_path(library_client, hash);
    if let Err(e) = media_client.queue_next(path) {
        handle_playback_error(e);
    }
    println!("Queued next");
    Ok(())
}

fn handle_queue_append(
    library_client: &LibraryClient,
    media_client: &MediaClient,
    hash: String,
) -> Result<()> {
    let path = resolve_blob_path(library_client, hash);
    if let Err(e) = media_client.queue_append(path) {
        handle_playback_error(e);
    }
    println!("Appended to queue");
    Ok(())
}

fn handle_queue_list(media_client: &MediaClient) -> Result<()> {
    match media_client.queue_list() {
        Ok(paths) => {
            if paths.is_empty() {
                println!("Queue is empty");
            } else {
                println!("Queue ({} tracks):", paths.len());
                for (i, p) in paths.iter().enumerate() {
                    println!("  {}  {}", i + 1, p.display());
                }
            }
            Ok(())
        }
        Err(e) => handle_playback_error(e),
    }
}

fn handle_queue_clear(media_client: &MediaClient) -> Result<()> {
    if let Err(e) = media_client.queue_clear() {
        handle_playback_error(e);
    }
    println!("Queue cleared");
    Ok(())
}

/// Return the provided hash, or read the first whitespace-delimited token from stdin.
/// Supports `mdma search ... | dmenu | mdma queue append` where dmenu outputs
/// "{short_hash}  {display}".
fn hash_or_stdin(hash: Option<String>) -> String {
    match hash {
        Some(h) => h,
        None => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .expect("failed to read from stdin");
            match line.split_whitespace().next() {
                Some(token) => token.to_string(),
                None => {
                    eprintln!("No hash provided and stdin was empty");
                    std::process::exit(1);
                }
            }
        }
    }
}

/// Resolve a hash to a blob path via the library service.
fn resolve_blob_path(library_client: &LibraryClient, hash: String) -> std::path::PathBuf {
    let content_hash = ContentHash(hash);
    let track = match library_client.get_track(&content_hash) {
        Ok(t) => t,
        Err(e) => handle_error(e),
    };
    match track.blob_path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            eprintln!("Track {} has no blob path", short_hash(&track.content_hash));
            std::process::exit(1);
        }
    }
}

fn handle_playback_stop(media_client: &MediaClient) -> Result<()> {
    if let Err(e) = media_client.stop(Deck::A) {
        handle_playback_error(e);
    }

    println!("Stopped");
    Ok(())
}

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    // Dispatch command - connect to appropriate service based on command
    match cli.command {
        Commands::Playback { command } => {
            let connect_media = || match MediaClient::connect(&cli.playback_socket) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "Failed to connect to playback server at {}: {}",
                        cli.playback_socket, e
                    );
                    eprintln!("Is mdma-playback running?");
                    std::process::exit(1);
                }
            };

            match command {
                PlaybackCommands::Play => {
                    let media_client = connect_media();
                    handle_playback_play(&media_client)
                }
                PlaybackCommands::Stop => {
                    let media_client = connect_media();
                    handle_playback_stop(&media_client)
                }
            }
        }

        Commands::Queue { command } => {
            let connect_media = || match MediaClient::connect(&cli.playback_socket) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "Failed to connect to playback server at {}: {}",
                        cli.playback_socket, e
                    );
                    eprintln!("Is mdma-playback running?");
                    std::process::exit(1);
                }
            };
            let connect_library = || match LibraryClient::connect(&cli.socket) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to connect to library at {}: {}", cli.socket, e);
                    eprintln!("Is mdma-library running?");
                    std::process::exit(1);
                }
            };

            match command {
                QueueCommands::Next { hash } => {
                    handle_queue_next(&connect_library(), &connect_media(), hash_or_stdin(hash))
                }
                QueueCommands::Append { hash } => {
                    handle_queue_append(&connect_library(), &connect_media(), hash_or_stdin(hash))
                }
                QueueCommands::List => handle_queue_list(&connect_media()),
                QueueCommands::Clear => handle_queue_clear(&connect_media()),
            }
        }

        Commands::Bandcamp { command } => {
            // Connect to bandcamp service
            let client = match BandcampClient::connect(&cli.bandcamp_socket) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "Failed to connect to bandcamp service at {}: {}",
                        cli.bandcamp_socket, e
                    );
                    eprintln!("Is mdma-bandcamp running?");
                    std::process::exit(1);
                }
            };

            match command {
                BandcampCommands::Ping => handle_bandcamp_ping(&client),
                BandcampCommands::Status => handle_bandcamp_status(&client),
                BandcampCommands::ReloadCookies => handle_bandcamp_reload_cookies(&client),
                BandcampCommands::Sync { username } => handle_bandcamp_sync(&client, username),
                BandcampCommands::Downloads => handle_bandcamp_downloads(&client),
                BandcampCommands::Cancel { id } => handle_bandcamp_cancel(&client, id),
                BandcampCommands::Pause => handle_bandcamp_pause(&client),
                BandcampCommands::Resume => handle_bandcamp_resume(&client),
            }
        }

        // Library commands - connect to library service
        cmd => {
            let client = match LibraryClient::connect(&cli.socket) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to connect to service at {}: {}", cli.socket, e);
                    eprintln!("Is mdma-library running?");
                    std::process::exit(1);
                }
            };

            match cmd {
                Commands::Ping => handle_ping(&client),
                Commands::Status => handle_status(&client),
                Commands::List { limit } => handle_list(&client, limit),
                Commands::Get { hash } => handle_get(&client, hash),
                Commands::Facts { hash } => handle_facts(&client, hash),
                Commands::Search { query } => handle_search(&client, query),
                Commands::Inbox { command } => match command {
                    InboxCommands::List => handle_inbox_list(&client),
                    InboxCommands::Delete { filename } => handle_inbox_delete(&client, filename),
                    InboxCommands::Ingest { filename } => handle_inbox_ingest(&client, filename),
                    InboxCommands::IngestAll => handle_inbox_ingest_all(&client),
                },
                Commands::Bandcamp { .. } | Commands::Playback { .. } | Commands::Queue { .. } => {
                    unreachable!()
                }
            }
        }
    }
}
