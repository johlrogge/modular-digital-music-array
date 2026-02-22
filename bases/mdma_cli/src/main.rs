//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to services via gateway (single address) or direct IPC.

use clap::{Parser, Subcommand};
use color_eyre::Result;
use colored::Colorize;
use corsett::{
    shortener::{FreeText, RightEllipsis},
    ColumnSizingConfigBuilder, RemovalPolicy, Row, Score, Shorten, ShortenAny,
};
use library_ipc_client::{ClientError, ContentHash, InboxPath, ProtocolError, TrackInfo};
use library_search::{parse_numeric_query, parse_string_query, TrackQuery};
use mdma_client::{LibraryBackend, PlaybackBackend, SourceClient};
use media_client::Deck;
use source_protocol::{SourceRequest, SourceResponse};

// =============================================================================
// CLI Definition
// =============================================================================

#[derive(Parser, Debug)]
#[command(name = "mdma")]
#[command(author, version, about = "MDMA CLI - Control the music services")]
struct Cli {
    /// Gateway address (routes all requests through a single endpoint)
    #[arg(long, global = true, env = "MDMA_GATEWAY")]
    gateway: Option<String>,

    /// Library IPC socket address (direct mode, ignored when --gateway is set)
    #[arg(
        long,
        default_value = "ipc:///run/mdma/library.sock",
        global = true,
        env = "MDMA_LIBRARY_SOCKET"
    )]
    socket: String,

    /// Playback server socket address (direct mode, ignored when --gateway is set)
    #[arg(
        long,
        default_value = "ipc:///run/mdma/playback.sock",
        global = true,
        env = "MDMA_PLAYBACK_SOCKET"
    )]
    playback_socket: String,

    /// Sources directory for direct mode (contains *.sock files)
    #[arg(
        long,
        default_value = "/run/mdma/sources",
        global = true,
        env = "MDMA_SOURCES_DIR"
    )]
    sources_dir: String,

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

    /// Music source management
    Source {
        #[command(subcommand)]
        command: SourceCommands,
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
enum SourceCommands {
    /// List available music sources
    List,

    /// Sync a music source (download new items)
    Sync {
        /// Source name (e.g. bandcamp)
        name: String,
    },

    /// Show source status
    Status {
        /// Source name (e.g. bandcamp)
        name: String,
    },

    /// List downloads for a source
    Downloads {
        /// Source name (e.g. bandcamp)
        name: String,
    },

    /// Cancel a download
    Cancel {
        /// Source name
        name: String,
        /// Download ID to cancel
        id: String,
    },

    /// Pause all downloads for a source
    Pause {
        /// Source name
        name: String,
    },

    /// Resume downloads for a source
    Resume {
        /// Source name
        name: String,
    },
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
// Connection Helpers
// =============================================================================

/// Connect to the library backend (gateway or direct).
fn connect_library(cli: &Cli) -> LibraryBackend {
    match LibraryBackend::connect(cli.gateway.as_deref(), &cli.socket) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to connect to library: {}", e);
            if cli.gateway.is_none() {
                eprintln!("Is mdma-library running?");
            }
            std::process::exit(1);
        }
    }
}

/// Connect to the playback backend (gateway or direct).
fn connect_playback(cli: &Cli) -> PlaybackBackend {
    match PlaybackBackend::connect(cli.gateway.as_deref(), &cli.playback_socket) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to connect to playback: {}", e);
            if cli.gateway.is_none() {
                eprintln!("Is mdma-playback running?");
            }
            std::process::exit(1);
        }
    }
}

/// Connect to a source, via gateway or direct IPC.
fn connect_source(cli: &Cli, name: &str) -> SourceClient {
    match SourceClient::connect(cli.gateway.as_deref(), &cli.sources_dir, name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", e);
            if cli.gateway.is_none() {
                eprintln!("Is mdma-{} running?", name);
            }
            std::process::exit(1);
        }
    }
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

/// Canonical playlist line format: `{short_hash}  {Artist} - {Title}  [{duration}]`
/// Used for pipe mode output and temp files (no colors, no alignment).
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

// =============================================================================
// Track Table Rendering (corsett + colored)
// =============================================================================

// Column types — each maps to a corsett shortening algorithm.
struct ColHash(String);
struct ColArtist(String);
struct ColTitle(String);
struct ColDuration(String);

impl AsRef<str> for ColHash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColArtist {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColTitle {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColDuration {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Shorten for ColHash {
    type Algorithm = FreeText; // always 8 chars, never needs shortening
}
impl Shorten for ColArtist {
    type Algorithm = RightEllipsis<'…', FreeText>;
}
impl Shorten for ColTitle {
    type Algorithm = RightEllipsis<'…', FreeText>;
}
impl Shorten for ColDuration {
    type Algorithm = FreeText; // naturally short, never needs shortening
}

struct TrackRow {
    hash: ColHash,
    artist: ColArtist,
    title: ColTitle,
    duration: ColDuration,
}

impl Row<4> for TrackRow {
    fn get_cell(&self, index: usize) -> &dyn ShortenAny {
        match index {
            0 => &self.hash,
            1 => &self.artist,
            2 => &self.title,
            3 => &self.duration,
            _ => panic!("TrackRow only has 4 columns"),
        }
    }
}

impl TrackRow {
    fn from_track(track: &TrackInfo) -> Self {
        Self {
            hash: ColHash(short_hash(&track.content_hash).to_string()),
            artist: ColArtist(track.artist.as_deref().unwrap_or("Unknown").to_string()),
            title: ColTitle(track.title.as_deref().unwrap_or("Unknown").to_string()),
            duration: ColDuration(track.duration.map(|d| d.to_string()).unwrap_or_default()),
        }
    }
}

/// Detect terminal width from $COLUMNS env var, fallback to 100.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100)
}

/// Pad a string to `width` chars (visual, not bytes).
fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len < width {
        format!("{}{}", s, " ".repeat(width - len))
    } else {
        s.to_string()
    }
}

/// Render tracks as a colored, aligned table for terminal display.
/// Returns one formatted string per track row.
fn render_track_table(tracks: &[TrackInfo]) -> Vec<String> {
    if tracks.is_empty() {
        return vec![];
    }

    let rows: Vec<TrackRow> = tracks.iter().map(TrackRow::from_track).collect();
    let config = ColumnSizingConfigBuilder::<4>::new()
        .terminal_width(terminal_width())
        .gap_size(2)
        .removal_policies([
            RemovalPolicy::Never,                      // hash — always visible
            RemovalPolicy::BelowScore(Score::MINIMAL), // artist — removed only when very cramped
            RemovalPolicy::Never,                      // title — always visible
            RemovalPolicy::BelowScore(Score::BASIC), // duration — removed first on narrow terminals
        ])
        .build();

    let resized = corsett::resize_columns(config, &rows);

    // Max visual width per column across all rows — for alignment.
    let mut col_widths = [0usize; 4];
    for row in &resized {
        for (i, cell) in row.iter().enumerate() {
            col_widths[i] = col_widths[i].max(cell.chars().count());
        }
    }

    resized
        .into_iter()
        .map(|[hash, artist, title, duration]| {
            let h = pad(&hash, col_widths[0]).bright_black().to_string();
            let a = pad(&artist, col_widths[1]).green().to_string();
            let t = pad(&title, col_widths[2]).bold().to_string();
            if duration.is_empty() {
                format!("{}  {}  {}", h, a, t)
            } else {
                let d = duration.bright_black().to_string();
                format!("{}  {}  {}  {}", h, a, t, d)
            }
        })
        .collect()
}

/// Print tracks with a bold header in terminal mode; canonical lines in pipe mode.
fn print_tracks(tracks: &[TrackInfo], header: &str) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        println!("{}", header.bold());
        println!();
        for line in render_track_table(tracks) {
            println!("{}", line);
        }
    } else {
        for track in tracks {
            println!("{}", format_track_line(track));
        }
    }
}

/// Print queue tracks. Terminal: numbered, colored table. Pipe: canonical lines.
fn print_queue_tracks(indexed: &[(usize, &TrackInfo)]) {
    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();
    if is_tty {
        let tracks: Vec<TrackInfo> = indexed.iter().map(|(_, t)| (*t).clone()).collect();
        let lines = render_track_table(&tracks);
        let pos_width = indexed
            .last()
            .map(|(p, _)| p.to_string().len())
            .unwrap_or(1);
        for ((pos, _), line) in indexed.iter().zip(lines.iter()) {
            let pos_str = format!("{:>width$}.", pos, width = pos_width)
                .bright_black()
                .to_string();
            println!("{}  {}", pos_str, line);
        }
    } else {
        for (_, track) in indexed {
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

/// Handle source errors uniformly
fn handle_source_error(err: &str) -> ! {
    eprintln!("Error: {}", err);
    std::process::exit(1);
}

// =============================================================================
// Command Handlers
// =============================================================================

fn handle_ping(client: &LibraryBackend) -> Result<()> {
    match client.ping() {
        Ok(()) => {
            println!("pong - service is alive");
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_status(client: &LibraryBackend) -> Result<()> {
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

fn handle_list(client: &LibraryBackend, limit: Option<usize>) -> Result<()> {
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

fn handle_get(client: &LibraryBackend, hash: String) -> Result<()> {
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

fn handle_facts(client: &LibraryBackend, hash: String) -> Result<()> {
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

fn handle_search(client: &LibraryBackend, query: &TrackQuery) -> Result<()> {
    use std::collections::HashSet;
    use std::io::{BufRead, IsTerminal};

    // When stdin is piped, read all hashes as an intersection filter.
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

fn handle_fact_values_for(client: &LibraryBackend, fact_type: String) -> Result<()> {
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

fn handle_inbox_list(client: &LibraryBackend) -> Result<()> {
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

fn handle_inbox_delete(client: &LibraryBackend, filename: String) -> Result<()> {
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

fn handle_inbox_ingest(client: &LibraryBackend, filename: String) -> Result<()> {
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

fn handle_inbox_ingest_all(client: &LibraryBackend) -> Result<()> {
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
// Source Command Handlers
// =============================================================================

fn handle_source_list(cli: &Cli) -> Result<()> {
    let sources =
        match mdma_client::list_available_sources(cli.gateway.as_deref(), &cli.sources_dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };
    if sources.is_empty() {
        println!("No sources available");
    } else {
        for name in sources {
            println!("{}", name);
        }
    }
    Ok(())
}

fn handle_source_sync(client: &SourceClient, name: &str) -> Result<()> {
    println!("Syncing {}...", name);
    match client.request(name, &SourceRequest::Sync) {
        Ok(SourceResponse::SyncStarted {
            total_items,
            new_items,
        }) => {
            println!("Sync started for {}", name);
            println!("Total items: {}, New items: {}", total_items, new_items);
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error("Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_status(client: &SourceClient, name: &str) -> Result<()> {
    match client.request(name, &SourceRequest::GetStatus) {
        Ok(SourceResponse::Status(status)) => {
            println!("Source: {} v{}", status.name, status.version);
            println!("{}", "=".repeat(40));
            println!(
                "Authenticated:     {}",
                if status.authenticated { "yes" } else { "no" }
            );
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
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error("Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_downloads(client: &SourceClient, name: &str) -> Result<()> {
    match client.request(name, &SourceRequest::ListDownloads) {
        Ok(SourceResponse::Downloads(downloads)) => {
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

                let status = match &dl.state {
                    source_protocol::DownloadState::Queued => "queued".to_string(),
                    source_protocol::DownloadState::Downloading => {
                        format!("downloading {}", progress)
                    }
                    source_protocol::DownloadState::Processing => "processing".to_string(),
                    source_protocol::DownloadState::Completed => "completed".to_string(),
                    source_protocol::DownloadState::Failed { message } => {
                        format!("failed: {}", message)
                    }
                    source_protocol::DownloadState::Cancelled => "cancelled".to_string(),
                };

                println!("{} | {} - {} | {}", dl.id, dl.artist, dl.title, status);
            }
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error("Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_cancel(client: &SourceClient, name: &str, id: String) -> Result<()> {
    match client.request(name, &SourceRequest::CancelDownload { id: id.clone() }) {
        Ok(SourceResponse::Cancelled { .. }) => {
            println!("Cancelled download: {}", id);
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error("Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_pause(client: &SourceClient, name: &str) -> Result<()> {
    match client.request(name, &SourceRequest::PauseAll) {
        Ok(SourceResponse::Paused) => {
            println!("Downloads paused");
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error("Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_resume(client: &SourceClient, name: &str) -> Result<()> {
    match client.request(name, &SourceRequest::ResumeAll) {
        Ok(SourceResponse::Resumed) => {
            println!("Downloads resumed");
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error("Unexpected response"),
        Err(e) => handle_source_error(&e),
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

fn handle_playback_play(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.play_queue() {
        handle_playback_error(e);
    }
    println!("Playing from queue");
    Ok(())
}

fn handle_playback_now(
    media_client: &PlaybackBackend,
    library_client: Option<&LibraryBackend>,
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
    library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
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
    library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
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

fn handle_queue_list(
    media_client: &PlaybackBackend,
    library_client: &LibraryBackend,
) -> Result<()> {
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
        println!("{}", format!("Queue ({} tracks)", tracks.len()).bold());
        println!();
    }
    print_queue_tracks(&indexed);
    Ok(())
}

fn handle_queue_remove(
    library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
    hashes: Vec<String>,
) -> Result<()> {
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

fn handle_queue_clear(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.queue_clear() {
        handle_playback_error(e);
    }
    println!("Queue cleared");
    Ok(())
}

fn handle_queue_replace(
    library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
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

fn handle_queue_edit(
    library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
) -> Result<()> {
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
    library_client: &LibraryBackend,
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
    client: &LibraryBackend,
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

fn handle_playback_stop(media_client: &PlaybackBackend) -> Result<()> {
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
    match &cli.command {
        Commands::Playback { command } => {
            let pb = connect_playback(&cli);
            match command {
                PlaybackCommands::Play => handle_playback_play(&pb),
                PlaybackCommands::Stop => handle_playback_stop(&pb),
                PlaybackCommands::Now => {
                    use std::io::IsTerminal;
                    if std::io::stdout().is_terminal() {
                        let lib = connect_library(&cli);
                        handle_playback_now(&pb, Some(&lib))
                    } else {
                        handle_playback_now(&pb, None)
                    }
                }
            }
        }

        Commands::Queue { command } => {
            let lib = connect_library(&cli);
            let pb = connect_playback(&cli);
            match command {
                QueueCommands::Next { hash } => {
                    handle_queue_next(&lib, &pb, hashes_arg_or_stdin(hash.clone()))
                }
                QueueCommands::Append { hash } => {
                    handle_queue_append(&lib, &pb, hashes_arg_or_stdin(hash.clone()))
                }
                QueueCommands::List => handle_queue_list(&pb, &lib),
                QueueCommands::Clear => handle_queue_clear(&pb),
                QueueCommands::Remove { hash } => {
                    handle_queue_remove(&lib, &pb, hashes_arg_or_stdin(hash.clone()))
                }
                QueueCommands::Replace => {
                    handle_queue_replace(&lib, &pb, hashes_arg_or_stdin(None))
                }
                QueueCommands::Edit => handle_queue_edit(&lib, &pb),
            }
        }

        Commands::Source { command } => match command {
            SourceCommands::List => handle_source_list(&cli),
            SourceCommands::Sync { name } => {
                let client = connect_source(&cli, name);
                handle_source_sync(&client, name)
            }
            SourceCommands::Status { name } => {
                let client = connect_source(&cli, name);
                handle_source_status(&client, name)
            }
            SourceCommands::Downloads { name } => {
                let client = connect_source(&cli, name);
                handle_source_downloads(&client, name)
            }
            SourceCommands::Cancel { name, id } => {
                let client = connect_source(&cli, name);
                handle_source_cancel(&client, name, id.clone())
            }
            SourceCommands::Pause { name } => {
                let client = connect_source(&cli, name);
                handle_source_pause(&client, name)
            }
            SourceCommands::Resume { name } => {
                let client = connect_source(&cli, name);
                handle_source_resume(&client, name)
            }
        },

        // Library commands - connect to library service
        Commands::Ping => {
            let client = connect_library(&cli);
            handle_ping(&client)
        }
        Commands::Status => {
            let client = connect_library(&cli);
            handle_status(&client)
        }
        Commands::List { limit } => {
            let client = connect_library(&cli);
            handle_list(&client, *limit)
        }
        Commands::Get { hash } => {
            let client = connect_library(&cli);
            handle_get(&client, hash.clone())
        }
        Commands::Facts { hash } => {
            let client = connect_library(&cli);
            handle_facts(&client, hash.clone())
        }
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
            let client = connect_library(&cli);
            if let Some(sub) = subcommand {
                match sub {
                    SearchSubcommands::FactValuesFor { fact_type } => {
                        handle_fact_values_for(&client, fact_type.clone())
                    }
                }
            } else {
                let track_query = build_track_query(
                    query.clone(),
                    artist.clone(),
                    title.clone(),
                    album.clone(),
                    label.clone(),
                    genre.clone(),
                    style.clone(),
                    bpm.clone(),
                    key.clone(),
                    duration.clone(),
                    year.clone(),
                    source.clone(),
                );
                handle_search(&client, &track_query)
            }
        }
        Commands::Inbox { command } => {
            let client = connect_library(&cli);
            match command {
                InboxCommands::List => handle_inbox_list(&client),
                InboxCommands::Delete { filename } => {
                    handle_inbox_delete(&client, filename.clone())
                }
                InboxCommands::Ingest { filename } => {
                    handle_inbox_ingest(&client, filename.clone())
                }
                InboxCommands::IngestAll => handle_inbox_ingest_all(&client),
            }
        }
        Commands::Sort {
            field,
            ascending,
            descending,
        } => {
            let client = connect_library(&cli);
            handle_sort(&client, field.clone(), *ascending, *descending)
        }
    }
}
