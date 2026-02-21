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
use library_search::{parse_numeric_query, parse_string_query, TrackQuery};
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
        /// Free-text query applied to all text fields (title, artist, album, label, genre).
        /// Supports CamelCase initialism (CarbBased) and /regex/ syntax.
        query: Option<String>,

        /// Filter by artist name
        #[arg(long)]
        artist: Option<String>,

        /// Filter by track title
        #[arg(long)]
        title: Option<String>,

        /// Filter by album name
        #[arg(long)]
        album: Option<String>,

        /// Filter by label name
        #[arg(long)]
        label: Option<String>,

        /// Filter by main genre (e.g. "Electronic", "Techno")
        #[arg(long)]
        genre: Option<String>,

        /// Filter by style descriptor — matches if any descriptor matches
        #[arg(long)]
        style: Option<String>,

        /// Filter by BPM. Formats: 128  128+-4  124..132  128+2  128-2
        #[arg(long)]
        bpm: Option<String>,

        /// Filter by musical key. Formats: Am  "A minor"  8B  8B+-1  8B+-1~
        #[arg(long)]
        key: Option<String>,

        /// Filter by duration. Formats: 7m  7m15s  >5m  <8m  6m..8m
        #[arg(long)]
        duration: Option<String>,

        /// Filter by release year. Formats: 2022  2019..2022
        #[arg(long)]
        year: Option<String>,

        /// Filter by source (bandcamp, beatport, upload)
        #[arg(long)]
        source: Option<String>,

        #[command(subcommand)]
        subcommand: Option<SearchSubcommands>,
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

    /// Sort hashes from stdin by a track metadata field.
    /// Reads one hash per line from stdin, outputs sorted hashes.
    /// Stable sort: chain multiple invocations for multi-key sort (right-to-left priority).
    ///
    /// Examples:
    ///   mdma search --artist=CBL | mdma sort title -a
    ///   mdma queue list | mdma sort bpm -d | mdma queue append
    ///   cat friday.plist | mdma sort title -a | mdma sort artist -a > sorted.plist
    Sort {
        /// Field to sort by
        #[arg(value_enum)]
        field: SortField,

        /// Sort ascending (A→Z, low→high)
        #[arg(short = 'a')]
        ascending: bool,

        /// Sort descending (Z→A, high→low)
        #[arg(short = 'd')]
        descending: bool,
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

    /// Show what is currently playing
    Now,
}

#[derive(Subcommand, Debug)]
enum SearchSubcommands {
    /// List all distinct values stored for a fact type.
    ///
    /// Examples:
    ///   mdma search fact-values-for genre
    ///   mdma search fact-values-for label | grep -i "ost"
    ///   mdma search fact-values-for genre | dmenu | xargs -I{} mdma search --genre {}
    FactValuesFor {
        /// Fact type to inspect (e.g. MainGenre, Label, Key, Source, BPM, StyleDescriptor)
        fact_type: String,
    },
}

#[derive(clap::ValueEnum, Debug, Clone)]
enum SortField {
    Bpm,
    Title,
    Artist,
    Album,
    Duration,
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

    /// Remove track(s) from the queue by hash.
    /// Hash may be omitted; if so, reads one hash per line from stdin.
    Remove {
        /// Content hash (full or partial). Omit to read from stdin.
        hash: Option<String>,
    },

    /// Atomically replace the entire queue from stdin.
    /// Reads playlist-format lines (8–12 char hex hash as first token).
    ///
    /// Examples:
    ///   mdma search --genre=Techno | shuf | mdma queue replace
    ///   cat friday.plist | mdma queue replace
    ///   mdma queue list | mdma sort bpm -a | mdma queue replace
    Replace,

    /// Edit the queue in $EDITOR (falls back to vi).
    /// Opens the current queue as a playlist file; save to apply changes.
    Edit,
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

/// Format a track as the canonical playlist line: `{short_hash}  {Artist} - {Title}  [{duration}]`
fn format_track_line(track: &TrackInfo) -> String {
    let title = track.title.as_deref().unwrap_or("Unknown");
    let artist = track.artist.as_deref().unwrap_or("Unknown");
    let duration = track
        .duration
        .map(|d| format!("[{}]", d))
        .unwrap_or_default();
    let hash = short_hash(&track.content_hash);
    if duration.is_empty() {
        format!("{}  {} - {}", hash, artist, title)
    } else {
        format!("{}  {} - {}  {}", hash, artist, title, duration)
    }
}

/// Print tracks with a terminal header. Pipe mode: track lines only.
fn print_tracks(tracks: &[TrackInfo], header: &str) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        println!("{}:", header);
        println!("{}", "=".repeat(65));
    }
    for track in tracks {
        println!("{}", format_track_line(track));
    }
}

/// Print queue tracks (terminal adds position prefix, pipe mode: track lines only).
fn print_queue_tracks(indexed: &[(usize, &TrackInfo)]) {
    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();
    for (pos, track) in indexed {
        if is_tty {
            println!("{}. {}", pos, format_track_line(track));
        } else {
            println!("{}", format_track_line(track));
        }
    }
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

/// Build a TrackQuery from individual CLI arguments.
fn build_track_query(
    any_text: Option<String>,
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    label: Option<String>,
    genre: Option<String>,
    style: Option<String>,
    bpm_str: Option<String>,
    _key_str: Option<String>,
    _duration_str: Option<String>,
    year_str: Option<String>,
    source: Option<String>,
) -> TrackQuery {
    TrackQuery {
        any_text: any_text.map(|s| parse_string_query(&s)),
        artist: artist.map(|s| parse_string_query(&s)),
        title: title.map(|s| parse_string_query(&s)),
        album: album.map(|s| parse_string_query(&s)),
        label: label.map(|s| parse_string_query(&s)),
        genre: genre.map(|s| parse_string_query(&s)),
        style: style.map(|s| parse_string_query(&s)),
        bpm: bpm_str.and_then(|s| parse_numeric_query(&s).ok()),
        key: None,      // key parsing deferred — key filter not yet wired up
        duration: None, // duration parsing deferred
        year: year_str.and_then(|s| parse_numeric_query(&s).ok()),
        source,
    }
}

fn handle_search(client: &LibraryClient, query: &TrackQuery) -> Result<()> {
    use std::collections::HashSet;
    use std::io::{BufRead, IsTerminal};

    // When stdin is piped, read all hashes as an intersection filter.
    // Each line may be a full hash (sha256:abc...) or the short-hash+display format
    // emitted by `mdma search` in pipe mode — we take the first whitespace token.
    let stdin_filter: Option<HashSet<String>> = if !std::io::stdin().is_terminal() {
        let tokens = std::io::stdin()
            .lock()
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| line.split_whitespace().next().map(|t| t.to_string()))
            .collect();
        Some(tokens)
    } else {
        None
    };

    match client.search(query) {
        Ok(tracks) => {
            // Apply stdin intersection filter if hashes were piped in.
            let tracks: Vec<_> = if let Some(ref filter) = stdin_filter {
                tracks
                    .into_iter()
                    .filter(|t| {
                        let clean = t
                            .content_hash
                            .0
                            .strip_prefix("sha256:")
                            .unwrap_or(&t.content_hash.0);
                        filter.iter().any(|token| {
                            let token_clean =
                                token.strip_prefix("sha256:").unwrap_or(token.as_str());
                            clean.starts_with(token_clean)
                        })
                    })
                    .collect()
            } else {
                tracks
            };

            if tracks.is_empty() && std::io::stdout().is_terminal() {
                println!("No tracks found");
                return Ok(());
            }
            print_tracks(
                &tracks,
                &format!("Search results ({} matches)", tracks.len()),
            );
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_fact_values_for(client: &LibraryClient, fact_type: String) -> Result<()> {
    use std::io::IsTerminal;
    match client.get_fact_values(&fact_type) {
        Ok(values) => {
            if std::io::stdout().is_terminal() {
                if values.is_empty() {
                    println!("No values found for fact type '{}'", fact_type);
                    return Ok(());
                }
                println!("Values for '{}' ({} distinct):", fact_type, values.len());
                println!("{}", "=".repeat(65));
                for v in values {
                    println!("  {}", v);
                }
            } else {
                // Pipe mode: one value per line — composable with dmenu, grep, etc.
                for v in values {
                    println!("{}", v);
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

fn handle_playback_now(
    media_client: &MediaClient,
    library_client: Option<&LibraryClient>,
) -> Result<()> {
    let hash = match media_client.now_playing() {
        Ok(h) => h,
        Err(e) => handle_playback_error(e),
    };
    match hash {
        None => {
            eprintln!("Nothing playing");
            std::process::exit(1);
        }
        Some(h) => match library_client {
            None => println!("{}", h.0),
            Some(lib) => {
                let track = lib
                    .get_track(&h)
                    .expect("playing hash not found in library — invariant violated");
                let artist = track.artist.as_deref().unwrap_or("-");
                let title = track.title.as_deref().unwrap_or("-");
                let duration = track
                    .duration
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!("{}  {} - {}  [{}]", short_hash(&h), artist, title, duration);
            }
        },
    }
    Ok(())
}

fn handle_queue_next(
    library_client: &LibraryClient,
    media_client: &MediaClient,
    hashes: Vec<String>,
) -> Result<()> {
    let count = hashes.len();
    // Prepend in reverse so the first hash ends up at the front of the queue.
    for hash in hashes.into_iter().rev() {
        let (content_hash, path) = resolve_track(library_client, hash);
        if let Err(e) = media_client.queue_next(content_hash, path) {
            handle_playback_error(e);
        }
    }
    println!("Queued {} track(s) next", count);
    Ok(())
}

fn handle_queue_append(
    library_client: &LibraryClient,
    media_client: &MediaClient,
    hashes: Vec<String>,
) -> Result<()> {
    let count = hashes.len();
    for hash in hashes {
        let (content_hash, path) = resolve_track(library_client, hash);
        if let Err(e) = media_client.queue_append(content_hash, path) {
            handle_playback_error(e);
        }
    }
    println!("Appended {} track(s) to queue", count);
    Ok(())
}

fn handle_queue_list(media_client: &MediaClient, library_client: &LibraryClient) -> Result<()> {
    let hashes = match media_client.queue_list() {
        Ok(h) => h,
        Err(e) => handle_playback_error(e),
    };

    if hashes.is_empty() {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!("Queue is empty");
        }
        return Ok(());
    }

    let tracks: Vec<TrackInfo> = hashes
        .iter()
        .map(|hash| {
            library_client
                .get_track(hash)
                .expect("queued hash not found in library — invariant violated")
        })
        .collect();

    let indexed: Vec<(usize, &TrackInfo)> =
        tracks.iter().enumerate().map(|(i, t)| (i + 1, t)).collect();

    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        println!("Queue ({} tracks):", tracks.len());
        println!("{}", "=".repeat(65));
    }
    print_queue_tracks(&indexed);
    Ok(())
}

fn handle_queue_remove(
    library_client: &LibraryClient,
    media_client: &MediaClient,
    hashes: Vec<String>,
) -> Result<()> {
    // Resolve each hash through the library to get the canonical full sha256: hash.
    // This handles short hashes (8-char prefixes) and full hashes equally.
    // Hashes that don't resolve are skipped with a warning — they can't be in the queue.
    let content_hashes: Vec<ContentHash> = hashes
        .into_iter()
        .filter_map(|hash| {
            let content_hash = ContentHash(hash);
            match library_client.get_track(&content_hash) {
                Ok(t) => Some(t.content_hash),
                Err(e) => {
                    eprintln!("Warning: could not resolve hash: {}", e);
                    None
                }
            }
        })
        .collect();

    if content_hashes.is_empty() {
        return Ok(());
    }

    match media_client.queue_remove(content_hashes) {
        Ok(removed) if removed > 0 => {
            println!("Removed {} track(s) from queue", removed);
        }
        Ok(_) => {}
        Err(e) => handle_playback_error(e),
    }
    Ok(())
}

fn handle_queue_clear(media_client: &MediaClient) -> Result<()> {
    if let Err(e) = media_client.queue_clear() {
        handle_playback_error(e);
    }
    println!("Queue cleared");
    Ok(())
}

fn handle_queue_replace(
    library_client: &LibraryClient,
    media_client: &MediaClient,
    hashes: Vec<String>,
) -> Result<()> {
    let entries: Vec<(ContentHash, std::path::PathBuf)> = hashes
        .into_iter()
        .map(|hash| resolve_track(library_client, hash))
        .collect();
    let count = entries.len();
    if let Err(e) = media_client.queue_replace(entries) {
        handle_playback_error(e);
    }
    println!("Queue replaced: {} tracks", count);
    Ok(())
}

fn handle_queue_edit(library_client: &LibraryClient, media_client: &MediaClient) -> Result<()> {
    // 1. Get current queue hashes.
    let hashes = match media_client.queue_list() {
        Ok(h) => h,
        Err(e) => handle_playback_error(e),
    };

    // 2. Look up each track for display info.
    let tracks: Vec<TrackInfo> = hashes
        .iter()
        .map(|hash| {
            library_client
                .get_track(hash)
                .expect("queued hash not found in library — invariant violated")
        })
        .collect();

    // 3. Write to temp file in playlist format.
    let tmp_path = std::env::temp_dir().join("mdma_queue_edit.plist");
    let mut content = String::from(
        "# MDMA queue — reorder, delete, or add lines. Save to apply.\n\
         # Lines not starting with an 8-12 character lowercase hash followed by a space are ignored.\n\
         \n",
    );
    for track in &tracks {
        content.push_str(&format_track_line(track));
        content.push('\n');
    }
    std::fs::write(&tmp_path, &content)
        .map_err(|e| {
            eprintln!("Failed to write temp file: {}", e);
            std::process::exit(1);
        })
        .unwrap();

    // 4. Launch $EDITOR (fallback to vi).
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .map_err(|e| {
            eprintln!("Failed to launch editor '{}': {}", editor, e);
            std::process::exit(1);
        })
        .unwrap();

    if !status.success() {
        eprintln!("Editor exited with non-zero status");
        std::process::exit(1);
    }

    // 5. Read back, parse hashes using the standard filter.
    use std::io::BufRead;
    let file = std::fs::File::open(&tmp_path)
        .map_err(|e| {
            eprintln!("Failed to read temp file: {}", e);
            std::process::exit(1);
        })
        .unwrap();
    let edited_hashes: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let first = line.split_whitespace().next()?;
            let len = first.len();
            if len >= 8
                && len <= 12
                && first
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
            {
                Some(first.to_string())
            } else {
                None
            }
        })
        .collect();

    let _ = std::fs::remove_file(&tmp_path);

    // 6. Resolve and replace.
    let entries: Vec<(ContentHash, std::path::PathBuf)> = edited_hashes
        .into_iter()
        .map(|hash| resolve_track(library_client, hash))
        .collect();
    let count = entries.len();
    if let Err(e) = media_client.queue_replace(entries) {
        handle_playback_error(e);
    }
    println!("Queue updated: {} tracks", count);
    Ok(())
}

/// Return the provided hash as a single-element vec, or read ALL lines from stdin and
/// extract valid short hashes (8–12 lowercase hex chars as first token).
///
/// Supports both:
///   Single:  mdma queue append ec9ce8d0
///   Multi:   mdma search "van morph" | mdma queue append
///   Dmenu:   mdma search "van morph" | dmenu | mdma queue append  (dmenu outputs one line)
///   Playlist: cat set.plist | mdma queue replace   (comments/blank lines silently ignored)
fn hashes_arg_or_stdin(hash: Option<String>) -> Vec<String> {
    match hash {
        Some(h) => vec![h],
        None => {
            use std::io::BufRead;
            let hashes: Vec<String> = std::io::stdin()
                .lock()
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| {
                    let first = line.split_whitespace().next()?;
                    let len = first.len();
                    if len >= 8
                        && len <= 12
                        && first
                            .chars()
                            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
                    {
                        Some(first.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            if hashes.is_empty() {
                eprintln!("No hash provided and stdin was empty");
                std::process::exit(1);
            }
            hashes
        }
    }
}

/// Resolve a hash to a (ContentHash, PathBuf) pair via the library service.
fn resolve_track(
    library_client: &LibraryClient,
    hash: String,
) -> (ContentHash, std::path::PathBuf) {
    let content_hash = ContentHash(hash);
    let track = match library_client.get_track(&content_hash) {
        Ok(t) => t,
        Err(e) => handle_error(e),
    };
    match track.blob_path {
        Some(p) => (track.content_hash, std::path::PathBuf::from(p)),
        None => {
            eprintln!("Track {} has no blob path", short_hash(&track.content_hash));
            std::process::exit(1);
        }
    }
}

fn handle_sort(
    client: &LibraryClient,
    field: SortField,
    ascending: bool,
    descending: bool,
) -> Result<()> {
    use std::cmp::Ordering;

    let direction_asc = match (ascending, descending) {
        (true, false) => true,
        (false, true) => false,
        _ => {
            eprintln!("Specify exactly one of -a (ascending) or -d (descending)");
            std::process::exit(1);
        }
    };

    let hashes = hashes_arg_or_stdin(None);

    let mut tracks: Vec<TrackInfo> = hashes
        .into_iter()
        .filter_map(|hash| {
            let content_hash = ContentHash(hash);
            match client.get_track(&content_hash) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("Warning: could not resolve hash: {}", e);
                    None
                }
            }
        })
        .collect();

    tracks.sort_by(|a, b| match &field {
        SortField::Bpm => match (&a.bpm, &b.bpm) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(av), Some(bv)) => {
                if direction_asc {
                    av.cmp(bv)
                } else {
                    bv.cmp(av)
                }
            }
        },
        SortField::Title => match (&a.title, &b.title) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(av), Some(bv)) => {
                let cmp = av.to_lowercase().cmp(&bv.to_lowercase());
                if direction_asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }
        },
        SortField::Artist => match (&a.artist, &b.artist) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(av), Some(bv)) => {
                let cmp = av.to_lowercase().cmp(&bv.to_lowercase());
                if direction_asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }
        },
        SortField::Album => match (&a.album, &b.album) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(av), Some(bv)) => {
                let cmp = av.to_lowercase().cmp(&bv.to_lowercase());
                if direction_asc {
                    cmp
                } else {
                    cmp.reverse()
                }
            }
        },
        SortField::Duration => match (&a.duration, &b.duration) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(av), Some(bv)) => {
                if direction_asc {
                    av.0.cmp(&bv.0)
                } else {
                    bv.0.cmp(&av.0)
                }
            }
        },
    });

    print_tracks(&tracks, &format!("Sorted ({} tracks)", tracks.len()));
    Ok(())
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

            let connect_library = || match LibraryClient::connect(&cli.socket) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to connect to library at {}: {}", cli.socket, e);
                    eprintln!("Is mdma-library running?");
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
                PlaybackCommands::Now => {
                    use std::io::IsTerminal;
                    let media_client = connect_media();
                    if std::io::stdout().is_terminal() {
                        let lib = connect_library();
                        handle_playback_now(&media_client, Some(&lib))
                    } else {
                        handle_playback_now(&media_client, None)
                    }
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
                QueueCommands::Next { hash } => handle_queue_next(
                    &connect_library(),
                    &connect_media(),
                    hashes_arg_or_stdin(hash),
                ),
                QueueCommands::Append { hash } => handle_queue_append(
                    &connect_library(),
                    &connect_media(),
                    hashes_arg_or_stdin(hash),
                ),
                QueueCommands::List => handle_queue_list(&connect_media(), &connect_library()),
                QueueCommands::Clear => handle_queue_clear(&connect_media()),
                QueueCommands::Remove { hash } => handle_queue_remove(
                    &connect_library(),
                    &connect_media(),
                    hashes_arg_or_stdin(hash),
                ),
                QueueCommands::Replace => handle_queue_replace(
                    &connect_library(),
                    &connect_media(),
                    hashes_arg_or_stdin(None),
                ),
                QueueCommands::Edit => handle_queue_edit(&connect_library(), &connect_media()),
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
                Commands::Search {
                    query,
                    artist,
                    title,
                    album,
                    label,
                    genre,
                    style,
                    bpm,
                    key,
                    duration,
                    year,
                    source,
                    subcommand,
                } => {
                    if let Some(sub) = subcommand {
                        match sub {
                            SearchSubcommands::FactValuesFor { fact_type } => {
                                handle_fact_values_for(&client, fact_type)
                            }
                        }
                    } else {
                        let track_query = build_track_query(
                            query, artist, title, album, label, genre, style, bpm, key, duration,
                            year, source,
                        );
                        handle_search(&client, &track_query)
                    }
                }
                Commands::Inbox { command } => match command {
                    InboxCommands::List => handle_inbox_list(&client),
                    InboxCommands::Delete { filename } => handle_inbox_delete(&client, filename),
                    InboxCommands::Ingest { filename } => handle_inbox_ingest(&client, filename),
                    InboxCommands::IngestAll => handle_inbox_ingest_all(&client),
                },
                Commands::Sort {
                    field,
                    ascending,
                    descending,
                } => handle_sort(&client, field, ascending, descending),
                Commands::Bandcamp { .. } | Commands::Playback { .. } | Commands::Queue { .. } => {
                    unreachable!()
                }
            }
        }
    }
}
