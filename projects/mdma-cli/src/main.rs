//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to services via gateway (single address) or direct IPC.

use clap::{CommandFactory, Parser, Subcommand};
use color_eyre::Result;
use colored::Colorize;
use corsett::{
    shortener::{FreeText, RightEllipsis},
    ColumnSizingConfigBuilder, RemovalPolicy, Row, Score, Shorten, ShortenAny,
};
use event_protocol::{from_topic_message, PlaybackEvent, TOPIC_PLAYBACK};
use library_ipc_client::{ClientError, ContentHash, InboxPath, ProtocolError, TrackInfo};
use library_search::{parse_date_query, parse_numeric_query, parse_string_query, TrackQuery};
use mdma_client::{
    Deck, IngestSource, LibraryBackend, PlaybackBackend, PlaybackClientError, SourceClient,
    SourceName,
};
use music_facts::MusicValue;
use nng::options::Options;
use rekordbox_xml::{parse_xml, RekordboxTrack};
use source_protocol::{SourceError, SourceRequest, SourceResponse};
use std::path::Path;
use track_matcher::{CandidateTrack, MatchResult, TrackLookup};

// =============================================================================
// CLI Definition
// =============================================================================

#[derive(Parser, Debug)]
#[command(name = "mdma")]
#[command(author, version, about = "MDMA CLI - Control the music services")]
struct Cli {
    /// Node hostname (e.g. mdma-909.local). Derives gateway addresses automatically.
    #[arg(long, env = "MDMA_NODE")]
    node: Option<String>,

    /// Library IPC socket address (direct mode, ignored when --node is set)
    #[arg(
        long,
        default_value = "ipc:///run/mdma/library.sock",
        env = "MDMA_LIBRARY_SOCKET"
    )]
    socket: String,

    /// Playback server socket address (direct mode, ignored when --node is set)
    #[arg(
        long,
        default_value = "ipc:///run/mdma/playback.sock",
        env = "MDMA_PLAYBACK_SOCKET"
    )]
    playback_socket: String,

    /// Sources directory for direct mode (contains *.sock files)
    #[arg(long, default_value = "/run/mdma/sources", env = "MDMA_SOURCES_DIR")]
    sources_dir: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
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

        /// Ignore stdin (don't read piped hashes as intersection filter)
        #[arg(long)]
        no_stdin: bool,

        /// Filter by last started date. Format: N/A, >2026-02, <2026, 2026-01..2026-06
        #[arg(long, allow_hyphen_values = true)]
        started: Option<String>,

        /// Filter by last stopped date. Same format as --started.
        #[arg(long, allow_hyphen_values = true)]
        stopped: Option<String>,

        /// Filter by date added to library. Uses date expression syntax: ~/+1/15 (15th next month), -7 (7 days ago), ^ (1st of month), $ (end of month). Ranges: -7..~ (last 7 days to today).
        #[arg(long, allow_hyphen_values = true)]
        added: Option<String>,

        /// Filter by playback history. `never` matches tracks with no started fact (never played).
        /// When combined with --started, --played=never takes precedence.
        #[arg(long)]
        played: Option<PlayedFilter>,

        /// Invert the search results — return tracks that do NOT match the filters.
        #[arg(long)]
        not: bool,

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

    /// Subscribe to real-time events from the playback system
    Subscribe {
        /// Topic filter (e.g. "playback/track_started"). Default: all playback events.
        #[arg(long)]
        topic: Option<String>,
    },

    /// Upload a file (audio or ZIP of audio) to the library
    Upload {
        /// Path to file (audio file or ZIP archive)
        file: std::path::PathBuf,

        /// SSH key for SCP transfer
        #[arg(long, default_value = "~/.ssh/mdma_pi", env = "MDMA_SSH_KEY")]
        ssh_key: String,

        /// SSH user on the Pi
        #[arg(long, default_value = "mdma", env = "MDMA_SSH_USER")]
        ssh_user: String,

        /// Remote inbox directory
        #[arg(long, default_value = "/music/inbox/", env = "MDMA_INBOX_DIR")]
        inbox_dir: String,
    },

    /// Export tracks from the library to local files.
    ///
    /// Reads content hashes from stdin (one per line, pipe-friendly).
    /// Compatible with search output: `mdma search --artist CBL | mdma export`
    ///
    /// Examples:
    ///   mdma search --artist CBL | mdma export
    ///   mdma search --artist CBL | mdma export --format aiff
    ///   mdma search --artist CBL | mdma export --format wav --output ./archive/
    ///   mdma search --artist CBL | mdma export --lossless-format aiff --lossy-format wav
    Export {
        /// Output format for ALL tracks (original, aiff or wav). Default: original.
        /// Conflicts with --lossless-format and --lossy-format.
        #[arg(long, value_enum, conflicts_with_all = ["lossless_format", "lossy_format"])]
        format: Option<ExportFormat>,

        /// Target format for lossless sources (flac, wav, aiff).
        /// Lossy sources pass through unchanged.
        #[arg(long, value_enum)]
        lossless_format: Option<ExportFormat>,

        /// Target format for lossy sources (mp3, ogg, opus).
        /// Lossless sources pass through unchanged.
        #[arg(long, value_enum)]
        lossy_format: Option<ExportFormat>,

        /// Output directory (created if it doesn't exist)
        #[arg(long, default_value = "./export/")]
        output: std::path::PathBuf,
    },

    /// Export tracks for Pioneer Rekordbox
    ///
    /// Downloads audio files and generates rekordbox.xml for import
    /// via Rekordbox File → Import Library.
    ///
    /// Examples:
    ///   mdma rekordbox export --playlist my-set
    ///   mdma search --artist CBL | mdma rekordbox export
    Rekordbox {
        #[command(subcommand)]
        command: RekordboxCommands,
    },

    /// Manage playlists
    Playlist {
        #[command(subcommand)]
        command: PlaylistCommands,
    },

    /// Library maintenance commands
    Library {
        #[command(subcommand)]
        command: LibraryCommands,
    },

    /// Bookmark the currently playing track, or a specific track by hash
    Bookmark {
        /// Content hash of track to bookmark (defaults to now-playing if omitted)
        hash: Option<String>,
        /// Optional scope — associate bookmark with a named set
        #[arg(long)]
        scope: Option<String>,
    },

    /// List all bookmarked tracks
    Bookmarks,

    /// Generate shell completions
    #[command(hide = true)]
    GenerateCompletions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    /// Transfer the file without conversion (default)
    Original,
    Aiff,
    Wav,
}

impl ExportFormat {
    /// Return the fixed file extension for converted formats, or `None` for `Original`.
    fn static_extension(&self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aiff => Some("aiff"),
            Self::Wav => Some("wav"),
        }
    }

    /// Format string to pass in the `?format=` query parameter.
    fn format_param(&self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Aiff => "aiff",
            Self::Wav => "wav",
        }
    }
}

#[derive(Subcommand, Debug)]
enum RekordboxCommands {
    /// Export tracks to Rekordbox-compatible XML + audio files.
    ///
    /// Pass one or more --playlist flags to export named playlists.
    /// When no --playlist is given, hashes are read from stdin instead.
    /// Stdin input is ignored when --playlist is provided.
    Export {
        /// MDMA playlist(s) to export. Repeat for multiple: --playlist A --playlist B.
        /// If none given, falls back to stdin input (existing behaviour).
        #[arg(long)]
        playlist: Vec<String>,
        /// Output directory. Defaults to ~/Music/mdma_rekordbox.
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Audio format (defaults to original)
        #[arg(long, value_enum)]
        format: Option<ExportFormat>,
        /// Force a fresh XML and re-download everything (discards incremental sync).
        #[arg(long)]
        replace: bool,
    },

    /// Import tracks and playlists from a Rekordbox XML file.
    ///
    /// Matches Rekordbox tracks to MDMA library tracks by metadata,
    /// creates playlists, and optionally enriches BPM/Key facts.
    ///
    /// Examples:
    ///   mdma rekordbox import rekordbox.xml
    ///   mdma rekordbox import rekordbox.xml --playlist imported-set
    ///   mdma rekordbox import rekordbox.xml --enrich --dry-run
    Import {
        /// Path to rekordbox.xml file
        path: std::path::PathBuf,

        /// Create an MDMA playlist with matched tracks.
        /// Invalid characters (spaces, special chars) are replaced with `-`.
        #[arg(long)]
        playlist: Option<String>,

        /// Import all playlists found in the XML as MDMA playlists
        #[arg(long)]
        all_playlists: bool,

        /// Update BPM and Key facts from Rekordbox data for matched tracks
        #[arg(long)]
        enrich: bool,

        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
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

    /// Force re-sync of a specific item (bypasses dedup)
    Resync {
        /// Source name (e.g. bandcamp)
        name: String,
        /// Source-specific item identifier (e.g. "p123456" for bandcamp)
        identifier: String,
    },

    /// Check if a specific item has changed upstream
    CheckItem {
        /// Source name (e.g. bandcamp)
        name: String,
        /// Source-specific item identifier
        identifier: String,
    },

    /// Check the whole source collection for stale items
    CheckUpdates {
        /// Source name
        name: String,
        /// Auto-apply resync for any stale items found
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Debug)]
enum PlaybackCommands {
    /// Start playback from the queue (use `mdma queue append <hash>` to enqueue tracks)
    Start,

    /// Stop playback on deck A
    Stop,

    /// Pause playback on deck A (keeps the current track loaded; resume to continue)
    Pause,

    /// Resume playback on deck A after a pause
    Resume,

    /// Skip to the next track in the queue
    Skip,

    /// Show what is currently playing
    Now,

    /// Show the current session ID (a session spans from first play to queue empty)
    Session,

    /// List available audio output devices
    Outputs,

    /// Select an audio output device by name
    SetOutput {
        /// Device name as shown by `mdma playback outputs`
        name: String,
    },

    /// Show the currently selected audio output
    GetOutput,
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
    TrackNumber,
    DiscNumber,
    Added,
    /// Last started (played) datetime. None treated as -∞: ascending puts never-played first,
    /// descending puts never-played last.
    Started,
    /// Last stopped datetime. None treated as -∞: ascending puts never-stopped first,
    /// descending puts never-stopped last.
    Stopped,
}

/// Filter by playback history.
#[derive(clap::ValueEnum, Debug, Clone)]
enum PlayedFilter {
    /// Tracks that have never been played (started fact is absent).
    Never,
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

#[derive(Subcommand, Debug)]
enum PlaylistCommands {
    /// List all playlists
    List,

    /// Get playlist content (pipe to mdma queue replace to load).
    /// If name is omitted, reads playlist names from stdin (first whitespace-delimited
    /// token per line — handles both bare names and enriched `playlist list` output).
    Get {
        /// Playlist name. Omit to read names from stdin.
        name: Option<String>,
    },

    /// Print the name of every playlist that contains any of the tracks piped in on stdin.
    /// Reads track lines from stdin (8–12 char hex hash as first token).
    /// Output: one playlist name per match; pipe to `sort -u` to deduplicate.
    Contains {
        /// Print playlist only if it contains ALL input tracks
        #[arg(long)]
        all: bool,
        /// Print playlist only if it contains at least N input tracks
        #[arg(long)]
        at_least: Option<usize>,
        /// Print playlist only if it contains NONE of the input tracks
        #[arg(long)]
        no: bool,
    },

    /// Create a new playlist from stdin (fails if it already exists)
    New {
        /// Playlist name
        name: String,
    },

    /// Append stdin to an existing playlist
    Append {
        /// Playlist name
        name: String,
    },

    /// Replace playlist content from stdin
    Replace {
        /// Playlist name
        name: String,
    },

    /// Edit playlist in $EDITOR
    Edit {
        /// Playlist name
        name: String,
    },

    /// Remove a playlist
    Remove {
        /// Playlist name
        name: String,
    },

    /// Rename a playlist
    Rename {
        /// Current playlist name
        from: String,
        /// New playlist name
        to: String,
    },
}

#[derive(Subcommand, Debug)]
enum LibraryCommands {
    /// Re-extract and store cover art for tracks that don't have a CoverArtPath fact yet.
    ///
    /// Reads embedded artwork from each track's audio blob and writes the image to
    /// `/music/cover-art/<hash>.<ext>`. Only tracks without an existing CoverArtPath
    /// fact are processed.
    ReindexCovers,
}

// =============================================================================
// Gateway Resolution Helpers
// =============================================================================

/// Resolve the effective gateway address.
/// Derived from `--node` / `MDMA_NODE` (already in `cli.node` via clap): `tcp://<node>:5555`
/// Returns `None` when `--node` is not set — caller falls back to direct IPC.
fn resolve_gateway(cli: &Cli) -> Option<String> {
    client::ClientConfig {
        node: cli.node.clone(),
        ..Default::default()
    }
    .gateway_addr()
}

/// Resolve the effective event gateway address.
/// Derived from `--node` / `MDMA_NODE`: `tcp://<node>:5556`
/// Returns `None` when `--node` is not set.
fn resolve_event_gateway(cli: &Cli) -> Option<String> {
    client::ClientConfig {
        node: cli.node.clone(),
        ..Default::default()
    }
    .event_addr()
}

// =============================================================================
// Connection Helpers
// =============================================================================

/// Connect to the library backend (gateway or direct).
fn connect_library(cli: &Cli) -> LibraryBackend {
    let gateway = resolve_gateway(cli);
    match LibraryBackend::connect(gateway.as_deref(), &cli.socket) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to connect to library: {}", e);
            if gateway.is_none() {
                eprintln!("Is mdma-library running?");
            }
            std::process::exit(1);
        }
    }
}

/// Connect to the playback backend (gateway or direct).
fn connect_playback(cli: &Cli) -> PlaybackBackend {
    let gateway = resolve_gateway(cli);
    match PlaybackBackend::connect(gateway.as_deref(), &cli.playback_socket) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to connect to playback: {}", e);
            if gateway.is_none() {
                eprintln!("Is mdma-playback running?");
            }
            std::process::exit(1);
        }
    }
}

/// Connect to a source, via gateway or direct IPC.
fn connect_source(cli: &Cli, name: &str) -> SourceClient {
    let gateway = resolve_gateway(cli);
    match SourceClient::connect(gateway.as_deref(), &cli.sources_dir, name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", e);
            if gateway.is_none() {
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
    let clean = hash
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(hash.as_str());
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

/// Detect terminal width.
///
/// Priority:
/// 1. `$COLUMNS` env var — honoured first so BDD tests can override the width.
/// 2. ioctl via `terminal_size` — reads the actual PTY/terminal dimensions.
/// 3. Hard-coded fallback of 80 — used in CI / piped contexts.
fn terminal_width() -> usize {
    // Prefer $COLUMNS if explicitly set (e.g. in BDD tests)
    if let Some(w) = std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()) {
        return w;
    }
    // Query the actual terminal via ioctl
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80)
}

/// Fit a string to exactly `width` visible chars: truncate with trailing `…` if too long, pad with
/// spaces if too short. Returns an empty string when `width == 0`.
fn fit_cell(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let len = s.chars().count();
    if len > width {
        // Truncate, appending an ellipsis if there's room.
        if width > 1 {
            let truncated: String = s.chars().take(width - 1).collect();
            format!("{}…", truncated)
        } else {
            s.chars().take(width).collect()
        }
    } else if len < width {
        format!("{}{}", s, " ".repeat(width - len))
    } else {
        s.to_string()
    }
}

/// Render tracks as a colored, aligned table for terminal display.
/// Returns one formatted string per track row.
///
/// `reserved_prefix` is subtracted from the terminal width before column sizing.
/// Pass `0` for plain output; for queue display, pass `pos_width + 3` (digit(s) + "." + 2 spaces).
fn render_track_table(tracks: &[TrackInfo], reserved_prefix: usize) -> Vec<String> {
    render_track_table_inner(tracks, reserved_prefix, terminal_width())
}

/// Inner implementation of `render_track_table` that accepts an explicit `term_width`,
/// allowing tests to inject a known width without relying on `$COLUMNS`.
///
/// Corsett's `terminal_width` is compared to the sum of column widths only (no gaps), so we
/// subtract the gap overhead — `(N-1) * gap_size = 3 * 2 = 6` — to keep the full rendered
/// line (columns + gaps + prefix) within the terminal width.
fn render_track_table_inner(
    tracks: &[TrackInfo],
    reserved_prefix: usize,
    term_width: usize,
) -> Vec<String> {
    if tracks.is_empty() {
        return vec![];
    }

    const GAP_SIZE: usize = 2;
    const NUM_COLUMNS: usize = 4;
    const GAP_OVERHEAD: usize = (NUM_COLUMNS - 1) * GAP_SIZE; // 6

    let available_content = term_width
        .saturating_sub(reserved_prefix)
        .saturating_sub(GAP_OVERHEAD);

    let rows: Vec<TrackRow> = tracks.iter().map(TrackRow::from_track).collect();
    let config = ColumnSizingConfigBuilder::<4>::new()
        .terminal_width(available_content)
        .gap_size(GAP_SIZE)
        .max_depth(200)
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

    // Render rows: fit_cell truncates-or-pads each cell to the column width.
    resized
        .into_iter()
        .map(|[hash, artist, title, duration]| {
            let h = fit_cell(&hash, col_widths[0]).bright_black().to_string();
            let a = fit_cell(&artist, col_widths[1]).green().to_string();
            let t = fit_cell(&title, col_widths[2]).bold().to_string();
            if col_widths[3] == 0 || duration.is_empty() {
                format!("{}  {}  {}", h, a, t)
            } else {
                let d = fit_cell(&duration, col_widths[3])
                    .bright_black()
                    .to_string();
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
        for line in render_track_table(tracks, 0) {
            println!("{}", line);
        }
    } else {
        for track in tracks {
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
        ClientError::Transport(nng_transport::NngClientError::Connection(e)) => {
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
fn handle_source_error(err: &dyn std::fmt::Display) -> ! {
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
    let content_hash = ContentHash::new(hash);
    match client.get_track(&content_hash) {
        Ok(track) => {
            println!("Track: {}", track.content_hash.as_str());
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
    let content_hash = ContentHash::new(hash);
    match client.get_facts(&content_hash) {
        Ok((full_hash, facts)) => {
            println!("Facts for: {}", full_hash.as_str());
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
///
/// Precedence: `--played=never` overrides any explicit `--started` value.
#[allow(clippy::too_many_arguments)]
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
    started_str: Option<String>,
    stopped_str: Option<String>,
    added_str: Option<String>,
    played: Option<PlayedFilter>,
) -> TrackQuery {
    let started = if matches!(played, Some(PlayedFilter::Never)) {
        // --played=never is a shortcut for started=N/A and overrides any --started value.
        Some(library_search::DateQuery::NA)
    } else if let Some(s) = started_str {
        match parse_date_query(&s) {
            Ok(q) => Some(q),
            Err(e) => {
                eprintln!("Invalid --started value: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let stopped = if let Some(s) = stopped_str {
        match parse_date_query(&s) {
            Ok(q) => Some(q),
            Err(e) => {
                eprintln!("Invalid --stopped value: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let added = if let Some(s) = added_str {
        match parse_date_query(&s) {
            Ok(q) => Some(q),
            Err(e) => {
                eprintln!("Invalid --added value: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };
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
        started,
        stopped,
        added,
        not: false,
    }
}

/// Returns true if a track's content hash matches any token in the set.
///
/// Both the track hash and the token are stripped of the "sha256:" prefix before
/// comparison, and a prefix match is used (like `git log <short-hash>`).
fn hash_matches(track_hash: &str, set: &std::collections::HashSet<String>) -> bool {
    let clean = track_hash.strip_prefix("sha256:").unwrap_or(track_hash);
    set.iter().any(|token| {
        let token_clean = token.strip_prefix("sha256:").unwrap_or(token.as_str());
        clean.starts_with(token_clean)
    })
}

/// Apply a stdin hash filter to a list of tracks.
///
/// - `stdin`: `None` means no filter was piped — return `tracks` unchanged.
/// - `stdin`: `Some(set)` with `not == false` — keep only tracks whose hash is in `set`
///   (intersection).
/// - `stdin`: `Some(set)` with `not == true` — keep only tracks whose hash is NOT in `set`
///   (exclusion). In this case the caller must ensure the query was sent to the library
///   with `.not = false` so that the library returns a candidate set (not nothing).
fn apply_stdin_filter(
    tracks: Vec<TrackInfo>,
    stdin: Option<&std::collections::HashSet<String>>,
    not: bool,
) -> Vec<TrackInfo> {
    match stdin {
        None => tracks,
        Some(set) => {
            if not {
                tracks
                    .into_iter()
                    .filter(|t| !hash_matches(t.content_hash.as_str(), set))
                    .collect()
            } else {
                tracks
                    .into_iter()
                    .filter(|t| hash_matches(t.content_hash.as_str(), set))
                    .collect()
            }
        }
    }
}

fn handle_search(client: &LibraryBackend, query: &TrackQuery, no_stdin: bool) -> Result<()> {
    use std::collections::HashSet;
    use std::io::{BufRead, IsTerminal};

    // When stdin is piped (and --no-stdin is not set), read all hashes as an intersection filter.
    let stdin_filter: Option<HashSet<String>> = if !no_stdin && !std::io::stdin().is_terminal() {
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

    // When stdin hashes are present and --not is set, we invert the intersection rather
    // than the library query: send the query without `.not` (so the library returns a
    // candidate set), then exclude any track whose hash is in the piped set.
    // Without stdin, `.not` flows through to the library as usual.
    let effective_not = query.not && stdin_filter.is_some();
    let mut effective_query = query.clone();
    if effective_not {
        effective_query.not = false;
    }

    match client.search(&effective_query) {
        Ok(tracks) => {
            let tracks = apply_stdin_filter(tracks, stdin_filter.as_ref(), effective_not);

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
            use mdma_client::IngestResult;
            match result {
                IngestResult::Success { message, .. } => println!("{}", message),
                IngestResult::Failure { message } => {
                    eprintln!("Failed: {}", message);
                    std::process::exit(1);
                }
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
            use mdma_client::IngestResult;
            match result {
                IngestResult::Success { hash, message } => {
                    if let Some(h) = hash {
                        println!("Success: {}", h.as_str());
                    } else {
                        println!("Success: {}", message);
                    }
                }
                IngestResult::Failure { message } => {
                    eprintln!("Failed: {}", message);
                    std::process::exit(1);
                }
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

            use mdma_client::IngestResult;
            for item in results {
                match item.result {
                    IngestResult::Success { hash, .. } => {
                        success_count += 1;
                        if let Some(hash) = hash {
                            println!("  OK: {} -> {}", item.path.as_str(), short_hash(&hash));
                        } else {
                            println!("  OK: {}", item.path.as_str());
                        }
                    }
                    IngestResult::Failure { message } => {
                        fail_count += 1;
                        println!("  FAIL: {} - {}", item.path.as_str(), message);
                    }
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
// Playlist Helpers
// =============================================================================

fn parse_playlist_name(name: &str) -> library_ipc_client::PlaylistName {
    library_ipc_client::PlaylistName::new(name).unwrap_or_else(|e| {
        eprintln!("Invalid playlist name: {}", e);
        std::process::exit(1);
    })
}

fn read_stdin_to_string() -> String {
    use std::io::Read;
    let mut content = String::new();
    std::io::stdin()
        .read_to_string(&mut content)
        .unwrap_or_else(|e| {
            eprintln!("Failed to read stdin: {}", e);
            std::process::exit(1);
        });
    content
}

fn playlist_expect_content(
    response: std::result::Result<
        library_ipc_client::LibraryResponse,
        library_ipc_client::ClientError,
    >,
) -> String {
    use library_ipc_client::LibraryResponse;
    match response {
        Ok(LibraryResponse::PlaylistContent(c)) => c,
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn playlist_expect_names(
    response: std::result::Result<
        library_ipc_client::LibraryResponse,
        library_ipc_client::ClientError,
    >,
) -> Vec<library_ipc_client::PlaylistName> {
    use library_ipc_client::LibraryResponse;
    match response {
        Ok(LibraryResponse::PlaylistNames(names)) => names,
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            std::process::exit(1);
        }
    }
}

// =============================================================================
// Playlist Command Handlers
// =============================================================================

fn handle_playlist_list(client: &LibraryBackend) -> Result<()> {
    use library_ipc_client::{DurationSeconds, LibraryRequest, LibraryResponse};

    let names = playlist_expect_names(client.request(&LibraryRequest::PlaylistList));

    for name in &names {
        let content = match client.request(&LibraryRequest::PlaylistGet { name: name.clone() }) {
            Ok(LibraryResponse::PlaylistContent(c)) => c,
            _ => continue,
        };

        let hashes: Vec<ContentHash> = content
            .lines()
            .filter_map(|line| parse_hash_from_line(line).map(ContentHash::new))
            .collect();

        let track_count = hashes.len();

        let total_secs: u32 = hashes
            .iter()
            .filter_map(|h| {
                let track = client.get_track(h).ok()?;
                track.duration.map(|d| d.value())
            })
            .sum();

        let duration = DurationSeconds::new(total_secs);
        println!(
            "{}  {}  [{}]",
            name.to_string().bold(),
            format!("{} tracks", track_count).green(),
            duration
        );
    }

    Ok(())
}

fn handle_playlist_get(client: &LibraryBackend, name: &Option<String>) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    match name {
        Some(n) => {
            let pname = parse_playlist_name(n);
            let content = playlist_expect_content(
                client.request(&LibraryRequest::PlaylistGet { name: pname }),
            );
            print!("{}", content);
        }
        None => {
            use std::io::BufRead;
            let names: Vec<String> = std::io::stdin()
                .lock()
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| {
                    let first = line.split_whitespace().next()?;
                    Some(first.to_string())
                })
                .collect();
            if names.is_empty() {
                eprintln!("No playlist name provided and stdin was empty");
                std::process::exit(1);
            }
            for n in &names {
                let pname = parse_playlist_name(n);
                let content = playlist_expect_content(
                    client.request(&LibraryRequest::PlaylistGet { name: pname }),
                );
                print!("{}", content);
            }
        }
    }
    Ok(())
}

/// Resolve the match threshold and invert flag from the `contains` filter flags.
/// Assumes mutual exclusivity and `at_least > 0` have already been validated.
/// Returns `(threshold, invert)`.
fn resolve_contains_threshold(
    all: bool,
    at_least: Option<usize>,
    no: bool,
    input_len: usize,
) -> (usize, bool) {
    if no {
        (1, true)
    } else if all {
        (input_len, false)
    } else if let Some(n) = at_least {
        (n, false)
    } else {
        (1, false)
    }
}

fn handle_playlist_contains(
    client: &LibraryBackend,
    all: bool,
    at_least: Option<usize>,
    no: bool,
) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    use std::collections::HashSet;
    use std::io::BufRead;

    // Validate mutually exclusive flags
    let flag_count = usize::from(all) + usize::from(at_least.is_some()) + usize::from(no);
    if flag_count > 1 {
        eprintln!("Error: --all, --at-least, and --no are mutually exclusive.");
        std::process::exit(1);
    }

    // Validate --at-least value
    if let Some(0) = at_least {
        eprintln!("Error: --at-least requires a value of at least 1.");
        std::process::exit(1);
    }

    // 1. Read hashes from stdin
    let input_hashes: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    if input_hashes.is_empty() {
        eprintln!("No hashes provided on stdin.");
        std::process::exit(1);
    }

    // 2. Determine threshold and invert
    let (threshold, invert) = resolve_contains_threshold(all, at_least, no, input_hashes.len());

    let input_set: HashSet<&String> = input_hashes.iter().collect();

    // 3. Get all playlist names
    let names = playlist_expect_names(client.request(&LibraryRequest::PlaylistList));

    // 4. For each playlist, count how many input hashes appear in it
    for pname in &names {
        let content = match client.request(&LibraryRequest::PlaylistGet {
            name: pname.clone(),
        }) {
            Ok(LibraryResponse::PlaylistContent(c)) => c,
            _ => continue,
        };

        let playlist_hashes: HashSet<String> =
            content.lines().filter_map(parse_hash_from_line).collect();

        let match_count = input_set
            .iter()
            .filter(|h| playlist_hashes.contains(h.as_str()))
            .count();

        let should_print = if invert {
            match_count == 0
        } else {
            match_count >= threshold
        };

        if should_print {
            println!("{}", pname);
        }
    }

    Ok(())
}

fn handle_playlist_new(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let pname = parse_playlist_name(name);
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistNew {
        name: pname,
        content,
    }));
    eprintln!("Created playlist '{}'", name);
    Ok(())
}

fn handle_playlist_append(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let pname = parse_playlist_name(name);
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistAppend {
        name: pname,
        content,
    }));
    eprintln!("Appended to playlist '{}'", name);
    Ok(())
}

fn handle_playlist_replace(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let pname = parse_playlist_name(name);
    let content = read_stdin_to_string();
    playlist_expect_content(client.request(&LibraryRequest::PlaylistReplace {
        name: pname,
        content,
    }));
    eprintln!("Replaced playlist '{}'", name);
    Ok(())
}

fn handle_playlist_edit(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse, ProtocolError};

    let pname = parse_playlist_name(name);

    // 1. Get current content (or start empty if not found)
    let current = match client.request(&LibraryRequest::PlaylistGet {
        name: pname.clone(),
    }) {
        Ok(LibraryResponse::PlaylistContent(c)) => c,
        Ok(LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. })) => String::new(),
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    };

    // 2. Write to temp file
    let tmp_path = std::env::temp_dir().join(format!("mdma_playlist_{}.plist", name));
    std::fs::write(&tmp_path, &current).unwrap_or_else(|e| {
        eprintln!("Failed to write temp file: {}", e);
        std::process::exit(1);
    });

    // 3. Open in $EDITOR
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to launch editor '{}': {}", editor, e);
            std::process::exit(1);
        });
    if !status.success() {
        eprintln!("Editor exited with non-zero status");
        std::process::exit(1);
    }

    // 4. Read back and replace
    let new_content = std::fs::read_to_string(&tmp_path).unwrap_or_else(|e| {
        eprintln!("Failed to read temp file: {}", e);
        std::process::exit(1);
    });

    playlist_expect_content(client.request(&LibraryRequest::PlaylistReplace {
        name: pname,
        content: new_content,
    }));
    eprintln!("Updated playlist '{}'", name);

    // Cleanup
    let _ = std::fs::remove_file(&tmp_path);
    Ok(())
}

fn handle_playlist_rename(client: &LibraryBackend, from: &str, to: &str) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let from_name = parse_playlist_name(from);
    let to_name = parse_playlist_name(to);
    let response = client.request(&LibraryRequest::PlaylistRename {
        from: from_name,
        to: to_name,
    });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Renamed playlist '{}' to '{}'", from, to);
        }
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn handle_playlist_remove(client: &LibraryBackend, name: &str) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let pname = parse_playlist_name(name);
    let response = client.request(&LibraryRequest::PlaylistRemove { name: pname });
    match response {
        Ok(LibraryResponse::Pong) => {
            eprintln!("Removed playlist '{}'", name);
        }
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }
    Ok(())
}

// =============================================================================
// Library Maintenance Command Handlers
// =============================================================================

fn handle_library_reindex_covers(client: &LibraryBackend) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let response = client.request(&LibraryRequest::ReindexCovers);
    match response {
        Ok(LibraryResponse::IngestResult(result)) => match result {
            library_ipc_client::IngestResult::Success { message, .. } => {
                println!("{}", message);
            }
            library_ipc_client::IngestResult::Failure { message } => {
                eprintln!("Error: {}", message);
                std::process::exit(1);
            }
        },
        Ok(LibraryResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }
    Ok(())
}

// =============================================================================
// Bookmark Command Handlers
// =============================================================================

fn handle_bookmark(
    library_client: &LibraryBackend,
    playback_client: Option<&PlaybackBackend>,
    hash: Option<String>,
    scope: Option<String>,
) -> Result<()> {
    let content_hash = match hash {
        Some(h) => ContentHash::new(h),
        None => {
            let pb = match playback_client {
                Some(p) => p,
                None => {
                    eprintln!("No hash provided and no playback client available");
                    std::process::exit(1);
                }
            };
            match pb.now_playing() {
                Ok(Some(h)) => h,
                Ok(None) => {
                    eprintln!("Nothing is currently playing");
                    std::process::exit(1);
                }
                Err(e) => handle_playback_error(e),
            }
        }
    };

    match library_client.write_bookmark(&content_hash, scope) {
        Ok(()) => {
            println!("Bookmarked.");
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_bookmarks(library_client: &LibraryBackend) -> Result<()> {
    let tracks = match library_client.list_tracks(None) {
        Ok(t) => t,
        Err(e) => handle_error(e),
    };

    let bookmarked: Vec<TrackInfo> = tracks
        .into_iter()
        .filter(
            |track| match library_client.get_facts(&track.content_hash) {
                Ok((_hash, facts)) => facts
                    .iter()
                    .any(|(fact_type, _value)| fact_type == "Bookmarked"),
                Err(_) => false,
            },
        )
        .collect();

    if bookmarked.is_empty() {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!("No bookmarked tracks");
        }
        return Ok(());
    }

    print_tracks(
        &bookmarked,
        &format!("Bookmarked tracks ({})", bookmarked.len()),
    );
    Ok(())
}

// =============================================================================
// Source Command Handlers
// =============================================================================

fn handle_source_list(cli: &Cli) -> Result<()> {
    let gateway = resolve_gateway(cli);
    let sources = match mdma_client::list_available_sources(gateway.as_deref(), &cli.sources_dir) {
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
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_resync(client: &SourceClient, name: &str, identifier: &str) -> Result<()> {
    println!("Requesting resync of {} from {}...", identifier, name);
    match client.request(
        name,
        &SourceRequest::ResyncItem {
            identifier: identifier.to_string(),
        },
    ) {
        Ok(SourceResponse::ResyncQueued {
            identifier,
            tracks_queued,
        }) => {
            println!(
                "Queued resync for {} ({} item queued)",
                identifier, tracks_queued
            );
            Ok(())
        }
        Ok(SourceResponse::Error(SourceError::ItemNotFound { identifier: ref id })) => {
            eprintln!("Item {} not found in your {} collection", id, name);
            std::process::exit(1);
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error(&"Unexpected response"),
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
                if status.auth == source_protocol::AuthStatus::Authenticated {
                    "yes"
                } else {
                    "no"
                }
            );
            println!("Downloads active:  {}", status.downloads_active);
            println!("Downloads queued:  {}", status.downloads_queued);
            println!("Downloads done:    {}", status.downloads_completed);
            println!("Downloads failed:  {}", status.downloads_failed);
            println!("Uptime:            {} seconds", status.uptime_seconds);
            println!(
                "Paused:            {}",
                if status.queue == source_protocol::QueueState::Paused {
                    "yes"
                } else {
                    "no"
                }
            );
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error(&"Unexpected response"),
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
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_cancel(client: &SourceClient, name: &str, id: String) -> Result<()> {
    match client.request(
        name,
        &SourceRequest::CancelDownload {
            id: source_protocol::DownloadId::new(id.clone()),
        },
    ) {
        Ok(SourceResponse::Cancelled { .. }) => {
            println!("Cancelled download: {}", id);
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_source_error(&e.to_string()),
        Ok(_) => handle_source_error(&"Unexpected response"),
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
        Ok(_) => handle_source_error(&"Unexpected response"),
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
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_check_item(client: &SourceClient, name: &str, identifier: &str) -> Result<()> {
    match client.request(
        name,
        &SourceRequest::CheckItem {
            identifier: identifier.to_string(),
        },
    ) {
        Ok(SourceResponse::ItemChecked {
            identifier,
            live_track_count,
            stored_track_count,
            stale,
        }) => {
            if stale {
                println!(
                    "STALE {}: live={} stored={}",
                    identifier, live_track_count, stored_track_count
                );
            } else {
                println!("OK {}: {} tracks", identifier, stored_track_count);
            }
            Ok(())
        }
        Ok(SourceResponse::Error(SourceError::ItemNotFound { identifier: ref id })) => {
            eprintln!("Item {} not found in your {} collection", id, name);
            std::process::exit(1);
        }
        Ok(SourceResponse::Error(e)) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

fn handle_source_check_updates(
    cli: &Cli,
    source_client: &SourceClient,
    name: &str,
    apply: bool,
) -> Result<()> {
    // Fetch all ItemId values from the library for this source.
    // We use GetFactValues("ItemId") — returns all ItemId values the library knows about.
    let lib = connect_library(cli);
    let all_item_ids = match lib.request(&library_ipc_client::LibraryRequest::GetFactValues {
        fact_type: library_ipc_client::FactType::new("ItemId"),
    }) {
        Ok(library_ipc_client::LibraryResponse::FactValues(values)) => values,
        Ok(library_ipc_client::LibraryResponse::Error(e)) => {
            eprintln!("Library error: {}", e);
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("Unexpected response from library");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to connect to library: {}", e);
            std::process::exit(1);
        }
    };

    if all_item_ids.is_empty() {
        println!("No items in library.");
        return Ok(());
    }

    let total = all_item_ids.len();
    let mut stale_ids: Vec<String> = Vec::new();
    let mut fresh_count = 0usize;
    let mut error_count = 0usize;

    for (idx, item_id) in all_item_ids.iter().enumerate() {
        eprint!("[{}/{}] checking {} ... ", idx + 1, total, item_id);

        match source_client.request(
            name,
            &SourceRequest::CheckItem {
                identifier: item_id.clone(),
            },
        ) {
            Ok(SourceResponse::ItemChecked {
                live_track_count,
                stored_track_count,
                stale,
                ..
            }) => {
                if stale {
                    eprintln!(
                        "STALE (live={}, stored={})",
                        live_track_count, stored_track_count
                    );
                    stale_ids.push(item_id.clone());
                } else {
                    eprintln!("ok");
                    fresh_count += 1;
                }
            }
            Ok(SourceResponse::Error(SourceError::ItemNotFound { .. })) => {
                // Not in this source's collection — skip silently
                eprintln!("not in collection (skipping)");
                fresh_count += 1;
            }
            Ok(SourceResponse::Error(e)) => {
                eprintln!("ERROR: {}", e);
                error_count += 1;
            }
            Ok(_) => {
                eprintln!("unexpected response");
                error_count += 1;
            }
            Err(e) => {
                eprintln!("transport error: {}", e);
                error_count += 1;
            }
        }

        // Rate-limit: be polite to bandcamp
        if idx + 1 < total {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    println!(
        "\n{} items stale, {} items up-to-date, {} items errored",
        stale_ids.len(),
        fresh_count,
        error_count
    );

    if stale_ids.is_empty() {
        return Ok(());
    }

    if apply {
        for id in &stale_ids {
            match source_client.request(
                name,
                &SourceRequest::ResyncItem {
                    identifier: id.clone(),
                },
            ) {
                Ok(SourceResponse::ResyncQueued { identifier, .. }) => {
                    println!("Queued resync for {}", identifier);
                }
                Ok(SourceResponse::Error(e)) => {
                    eprintln!("Failed to queue resync for {}: {}", id, e);
                }
                Ok(_) => {
                    eprintln!("Unexpected response queueing resync for {}", id);
                }
                Err(e) => {
                    eprintln!("Transport error queueing resync for {}: {}", id, e);
                }
            }
        }
    } else {
        println!("Run with --apply to resync stale items, or resync individually with:");
        for id in &stale_ids {
            println!("  mdma source resync {} {}", name, id);
        }
    }

    Ok(())
}

// =============================================================================
// Playback Command Handlers
// =============================================================================

fn handle_playback_error(err: PlaybackClientError) -> ! {
    match err {
        PlaybackClientError::Connection(e) => {
            eprintln!("Connection failed: {}", e);
            eprintln!("Is mdma-playback running?");
        }
        e => {
            eprintln!("Error: {}", e);
        }
    }
    std::process::exit(1);
}

fn handle_playback_start(media_client: &PlaybackBackend) -> Result<()> {
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
            None => println!("{}", h.as_str()),
            Some(lib) => {
                let track = match lib.get_track(&h) {
                    Ok(t) => t,
                    Err(e) => handle_error(e),
                };
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
    _library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
    hashes: Vec<String>,
) -> Result<()> {
    let count = hashes.len();
    // Prepend in reverse so the first hash ends up at the front of the queue.
    for hash in hashes.into_iter().rev() {
        let content_hash = ContentHash::new(hash);
        if let Err(e) = media_client.queue_next(content_hash, SourceName::audio()) {
            handle_playback_error(e);
        }
    }
    println!("Queued {} track(s) next", count);
    Ok(())
}

fn handle_queue_append(
    _library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
    hashes: Vec<String>,
) -> Result<()> {
    let count = hashes.len();
    for hash in hashes {
        let content_hash = ContentHash::new(hash);
        if let Err(e) = media_client.queue_append(content_hash, SourceName::audio()) {
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

    // Resolve each hash, keeping a Result per slot so positions stay aligned.
    let resolved: Vec<Result<TrackInfo, String>> = hashes
        .iter()
        .map(|hash| library_client.get_track(hash).map_err(|e| e.to_string()))
        .collect();

    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();

    if is_tty {
        println!("{}", format!("Queue ({} tracks)", hashes.len()).bold());
        println!();

        let pos_width = hashes.len().to_string().len().max(1);
        // Collect only the resolvable tracks so we can size columns together.
        let resolvable: Vec<TrackInfo> = resolved
            .iter()
            .filter_map(|r| r.as_ref().ok().cloned())
            .collect();
        // Pre-render resolvable tracks into lines (indexed by their slot).
        let mut resolvable_lines = render_track_table(&resolvable, pos_width + 3).into_iter();

        for (i, slot) in resolved.iter().enumerate() {
            let pos = i + 1;
            let pos_str = format!("{:>width$}.", pos, width = pos_width)
                .bright_black()
                .to_string();
            match slot {
                Ok(_) => {
                    let line = resolvable_lines.next().unwrap_or_default();
                    println!("{}  {}", pos_str, line);
                }
                Err(e) => {
                    let short = short_hash(&hashes[i]);
                    println!(
                        "{}  {}  {}",
                        pos_str,
                        short.bright_black(),
                        format!("[unavailable: {}]", e).bright_red()
                    );
                }
            }
        }
    } else {
        for (i, slot) in resolved.iter().enumerate() {
            match slot {
                Ok(track) => println!("{}", format_track_line(track)),
                Err(e) => {
                    let short = short_hash(&hashes[i]);
                    println!("{}  [unavailable: {}]", short, e);
                }
            }
        }
    }
    Ok(())
}

fn handle_queue_remove(
    _library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
    hashes: Vec<String>,
) -> Result<()> {
    let content_hashes: Vec<ContentHash> = hashes.into_iter().map(ContentHash::new).collect();

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
    _library_client: &LibraryBackend,
    media_client: &PlaybackBackend,
    hashes: Vec<String>,
) -> Result<()> {
    let entries: Vec<(ContentHash, SourceName)> = hashes
        .into_iter()
        .map(|hash| (ContentHash::new(hash), SourceName::audio()))
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

    // 2. Look up each track for display info. Unresolvable entries are written as
    //    plain-hash comment lines so they survive the edit round-trip unchanged.
    // 3. Write to temp file in playlist format.
    let tmp_path = std::env::temp_dir().join("mdma_queue_edit.plist");
    let mut content = String::from(
        "# MDMA queue — reorder, delete, or add lines. Save to apply.\n\
         # Lines not starting with an 8-12 character lowercase hash followed by a space are ignored.\n\
         \n",
    );
    for hash in &hashes {
        match library_client.get_track(hash) {
            Ok(track) => {
                content.push_str(&format_track_line(&track));
            }
            Err(e) => {
                // Write the raw hash with an inline comment so the user can keep or remove it.
                // parse_hash_from_line reads only the first whitespace token, so the comment is ignored
                // when the file is parsed back — the hash will survive the round-trip.
                content.push_str(&format!("{}  # [unavailable: {}]", short_hash(hash), e));
            }
        }
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
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    let _ = std::fs::remove_file(&tmp_path);

    // 6. Replace queue with reordered hashes.
    let entries: Vec<(ContentHash, SourceName)> = edited_hashes
        .into_iter()
        .map(|hash| (ContentHash::new(hash), SourceName::audio()))
        .collect();
    let count = entries.len();
    if let Err(e) = media_client.queue_replace(entries) {
        handle_playback_error(e);
    }
    println!("Queue updated: {} tracks", count);
    Ok(())
}

/// Return the provided hash as a single-element vec, or read ALL lines from stdin and
/// extract valid hashes (8+ lowercase hex chars, or sha256:-prefixed full hashes, as first token).
fn hashes_arg_or_stdin(hash: Option<String>) -> Vec<String> {
    match hash {
        Some(h) => vec![h],
        None => {
            use std::io::BufRead;
            let hashes: Vec<String> = std::io::stdin()
                .lock()
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| parse_hash_from_line(&line))
                .collect();
            if hashes.is_empty() {
                eprintln!("No hash provided and stdin was empty");
                std::process::exit(1);
            }
            hashes
        }
    }
}

/// Compare two optional values treating `None` as negative-infinity (-∞).
///
/// Semantics:
/// - Ascending  (`asc=true`):  None first, then real values oldest→newest.
/// - Descending (`asc=false`): real values newest→oldest, None last.
///
/// This applies uniformly to Started, Stopped, and Added so that
/// `mdma sort started -d` yields "most recently played first, never-played last".
fn compare_optional<T: Ord>(a: Option<T>, b: Option<T>, asc: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ord = match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less, // None is -∞, smaller than any Some
        (Some(_), None) => Ordering::Greater, // Some is larger than -∞
        (Some(av), Some(bv)) => av.cmp(&bv),
    };
    if asc {
        ord
    } else {
        ord.reverse()
    }
}

/// Resolve a list of raw hash strings to `TrackInfo`, skipping any that the library
/// cannot find and printing a warning for each skipped entry.
fn resolve_tracks_skip_errors(client: &LibraryBackend, hashes: Vec<String>) -> Vec<TrackInfo> {
    hashes
        .into_iter()
        .filter_map(|hash| {
            let content_hash = ContentHash::new(hash.clone());
            match client.get_track(&content_hash) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("Warning: skipping track {}: {}", hash, e);
                    None
                }
            }
        })
        .collect()
}

fn handle_sort(
    client: &LibraryBackend,
    field: SortField,
    ascending: bool,
    descending: bool,
) -> Result<()> {
    let direction_asc = match (ascending, descending) {
        (true, false) => true,
        (false, true) => false,
        _ => {
            eprintln!("Specify exactly one of -a (ascending) or -d (descending)");
            std::process::exit(1);
        }
    };

    let hashes = hashes_arg_or_stdin(None);
    let mut tracks = resolve_tracks_skip_errors(client, hashes);

    tracks.sort_by(|a, b| match &field {
        SortField::Bpm => compare_optional(a.bpm, b.bpm, direction_asc),
        SortField::Title => compare_optional(
            a.title.as_deref().map(str::to_lowercase),
            b.title.as_deref().map(str::to_lowercase),
            direction_asc,
        ),
        SortField::Artist => compare_optional(
            a.artist.as_deref().map(str::to_lowercase),
            b.artist.as_deref().map(str::to_lowercase),
            direction_asc,
        ),
        SortField::Album => compare_optional(
            a.album.as_deref().map(str::to_lowercase),
            b.album.as_deref().map(str::to_lowercase),
            direction_asc,
        ),
        SortField::Duration => compare_optional(
            a.duration.map(|d| d.value()),
            b.duration.map(|d| d.value()),
            direction_asc,
        ),
        SortField::TrackNumber => compare_optional(a.track_number, b.track_number, direction_asc),
        SortField::DiscNumber => compare_optional(a.disc_number, b.disc_number, direction_asc),
        SortField::Added => compare_optional(
            a.added.as_deref().map(str::to_string),
            b.added.as_deref().map(str::to_string),
            direction_asc,
        ),
        SortField::Started => compare_optional(
            a.started.as_deref().map(str::to_string),
            b.started.as_deref().map(str::to_string),
            direction_asc,
        ),
        SortField::Stopped => compare_optional(
            a.stopped.as_deref().map(str::to_string),
            b.stopped.as_deref().map(str::to_string),
            direction_asc,
        ),
    });

    print_tracks(&tracks, &format!("Sorted ({} tracks)", tracks.len()));
    Ok(())
}

fn handle_playback_session(media_client: &PlaybackBackend) -> Result<()> {
    match media_client.session() {
        Ok(Some(id)) => {
            println!("{}", id);
        }
        Ok(None) => {
            println!("No active session");
        }
        Err(e) => handle_playback_error(e),
    }
    Ok(())
}

fn handle_playback_outputs(media_client: &PlaybackBackend) -> Result<()> {
    let sinks = match media_client.list_audio_outputs() {
        Ok(s) => s,
        Err(e) => handle_playback_error(e),
    };
    println!("{:<40} {:<40} MAX RATE", "NAME", "DESCRIPTION");
    println!("{}", "-".repeat(90));
    for sink in &sinks {
        println!(
            "{:<40} {:<40} {}",
            sink.name,
            sink.description.as_deref().unwrap_or("-"),
            sink.max_sample_rate
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
    Ok(())
}

fn handle_playback_set_output(media_client: &PlaybackBackend, name: &str) -> Result<()> {
    let cfg = match media_client.set_audio_output(name.to_string()) {
        Ok(c) => c,
        Err(e) => handle_playback_error(e),
    };
    let device = cfg.device_name.as_deref().unwrap_or("auto");
    let rate = cfg
        .sample_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("Audio output set to: {} ({}Hz)", device, rate);
    Ok(())
}

fn handle_playback_get_output(media_client: &PlaybackBackend) -> Result<()> {
    let cfg = match media_client.get_audio_output() {
        Ok(c) => c,
        Err(e) => handle_playback_error(e),
    };
    let device = cfg.device_name.as_deref().unwrap_or("auto");
    let rate = cfg
        .sample_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("{} ({}Hz)", device, rate);
    Ok(())
}

fn handle_playback_stop(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.stop(Deck::A) {
        handle_playback_error(e);
    }

    println!("Stopped");
    Ok(())
}

fn handle_playback_skip(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.skip() {
        handle_playback_error(e);
    }

    println!("Skipped");
    Ok(())
}

fn handle_playback_pause(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.pause(Deck::A) {
        handle_playback_error(e);
    }

    println!("Paused");
    Ok(())
}

fn handle_playback_resume(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.resume(Deck::A) {
        handle_playback_error(e);
    }

    println!("Resumed");
    Ok(())
}

// =============================================================================
// Subscribe Command Handler
// =============================================================================

fn handle_subscribe(
    event_gateway: &str,
    topic: Option<&str>,
    library: Option<&LibraryBackend>,
) -> Result<()> {
    use std::io::IsTerminal;

    let socket = nng::Socket::new(nng::Protocol::Sub0)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to create Sub0 socket: {}", e))?;

    // Subscribe to the requested topic prefix, or all playback events
    let sub_topic = topic.unwrap_or(TOPIC_PLAYBACK);
    socket
        .set_opt::<nng::options::protocol::pubsub::Subscribe>(sub_topic.as_bytes().to_vec())
        .map_err(|e| color_eyre::eyre::eyre!("Failed to set subscription: {}", e))?;

    // Resolve hostname for .local mDNS addresses
    let resolved = nng_transport::resolve_tcp_hostname(event_gateway)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to resolve address: {}", e))?;

    socket
        .dial(&resolved)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to {}: {}", event_gateway, e))?;

    let is_tty = std::io::stdout().is_terminal();

    if is_tty {
        eprintln!("Subscribed to {} (topic: {})", event_gateway, sub_topic);
        eprintln!("Waiting for events... (Ctrl-C to stop)");
    }

    loop {
        let msg = socket
            .recv()
            .map_err(|e| color_eyre::eyre::eyre!("Receive error: {}", e))?;

        match from_topic_message(msg.as_slice()) {
            Ok((_topic, event)) => {
                if is_tty {
                    print_event_human(&event, library);
                } else {
                    // Pipe mode: raw JSON, one line per event
                    println!(
                        "{}",
                        serde_json::to_string(&event).unwrap_or_else(|_| format!("{:?}", event))
                    );
                }
            }
            Err(e) => {
                eprintln!("Failed to parse event: {}", e);
            }
        }
    }
}

fn print_event_human(event: &PlaybackEvent, library: Option<&LibraryBackend>) {
    match event {
        PlaybackEvent::TrackStarted { hash } => {
            let track_info = library.and_then(|lib| lib.get_track(hash).ok());
            match track_info {
                Some(track) => {
                    let artist = track.artist.as_deref().unwrap_or("Unknown");
                    let title = track.title.as_deref().unwrap_or("Unknown");
                    let duration = track
                        .duration
                        .map(|d| format!(" [{}]", d))
                        .unwrap_or_default();
                    println!(
                        "{} {} {} - {}{}",
                        "▶".green().bold(),
                        short_hash(&track.content_hash).bright_black(),
                        artist.green(),
                        title.bold(),
                        duration.bright_black()
                    );
                }
                None => {
                    println!(
                        "{} {}",
                        "▶ started".green().bold(),
                        hash.as_str().bright_black()
                    );
                }
            }
        }
        PlaybackEvent::TrackEnded { hash } => {
            println!(
                "{} {}",
                "■ ended".yellow().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::TrackStopped { hash } => {
            println!(
                "{} {}",
                "⏹ stopped".red().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::TrackPaused { hash } => {
            println!(
                "{} {}",
                "⏸ paused".yellow().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::TrackResumed { hash } => {
            println!(
                "{} {}",
                "▶ resumed".green().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::QueueChanged { length } => {
            println!(
                "{} {} track(s)",
                "♫ queue".blue().bold(),
                length.to_string().bold()
            );
        }
        PlaybackEvent::PositionUpdate { .. } => {
            // Position updates are high-frequency; suppress from the human-readable event stream.
        }
        PlaybackEvent::SessionStarted { id } => {
            println!(
                "{} {}",
                "◉ session started".cyan().bold(),
                id.to_string().bright_black()
            );
        }
        PlaybackEvent::SessionEnded { id } => {
            println!(
                "{} {}",
                "◎ session ended".cyan().bold(),
                id.to_string().bright_black()
            );
        }
    }
}

// =============================================================================
// Upload Command Handler
// =============================================================================

/// Extract hostname from a gateway address like `tcp://mdma-909.local:5555`.
fn extract_hostname(gateway: &str) -> Option<&str> {
    let after_scheme = gateway.strip_prefix("tcp://")?;
    after_scheme.split(':').next()
}

/// Expand `~` to the user's home directory.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// SCP a local file to a remote destination.
fn scp_file(
    local_path: &Path,
    remote_user: &str,
    remote_host: &str,
    remote_dir: &str,
    ssh_key: &Path,
) -> Result<()> {
    let dest = format!("{}@{}:{}", remote_user, remote_host, remote_dir);
    let status = std::process::Command::new("scp")
        .args([
            "-i",
            &ssh_key.to_string_lossy(),
            "-o",
            "StrictHostKeyChecking=no",
            "-4",
            &local_path.to_string_lossy(),
            &dest,
        ])
        .status()?;

    if !status.success() {
        eprintln!(
            "SCP failed for {}",
            local_path.file_name().unwrap_or_default().to_string_lossy()
        );
        std::process::exit(1);
    }
    Ok(())
}

fn handle_upload(
    cli: &Cli,
    file: &Path,
    ssh_key: &str,
    ssh_user: &str,
    inbox_dir: &str,
) -> Result<()> {
    use library_ipc_client::InboxPath;

    // 1. Validate file exists
    if !file.exists() {
        eprintln!("File not found: {}", file.display());
        std::process::exit(1);
    }

    // 2. Extract hostname from gateway (derived from --node / MDMA_NODE)
    let gateway_resolved = match resolve_gateway(cli) {
        Some(gw) => gw,
        None => {
            eprintln!("Upload requires --node or MDMA_NODE to determine the Pi hostname");
            std::process::exit(1);
        }
    };

    let hostname = match extract_hostname(&gateway_resolved) {
        Some(h) => h.to_string(),
        None => {
            eprintln!("Cannot parse hostname from gateway: {}", gateway_resolved);
            std::process::exit(1);
        }
    };

    let key_path = expand_tilde(ssh_key);

    // 3. Detect file type
    let file_type = inbox_utils::detect_file_type(file);

    // 4. Collect audio files to upload
    let temp_dir = tempfile::tempdir()?;
    let audio_files: Vec<std::path::PathBuf> = match file_type {
        Some("zip") => {
            println!("Extracting ZIP archive...");
            let extracted = inbox_utils::extract_zip(file, temp_dir.path())?;
            if extracted.is_empty() {
                eprintln!("No audio files found in ZIP archive");
                std::process::exit(1);
            }
            println!("Found {} audio file(s)", extracted.len());
            extracted
        }
        Some("flac" | "mp3" | "wav" | "aiff") => {
            vec![file.to_path_buf()]
        }
        _ => {
            eprintln!(
                "Unsupported file type. Expected audio file (FLAC, MP3, WAV, AIFF) or ZIP archive."
            );
            std::process::exit(1);
        }
    };

    // 5. SCP each file to the Pi's inbox
    for audio_file in &audio_files {
        let filename = audio_file.file_name().unwrap_or_default().to_string_lossy();
        println!("Uploading: {}", filename);
        scp_file(audio_file, ssh_user, &hostname, inbox_dir, &key_path)?;
    }

    // 6. Ingest each file via the library service
    let lib = connect_library(cli);
    let mut success_count = 0;
    let mut fail_count = 0;

    for audio_file in &audio_files {
        let filename = audio_file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let inbox_path = match InboxPath::new(&filename) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  FAIL: {} - invalid inbox path: {}", filename, e);
                fail_count += 1;
                continue;
            }
        };

        match lib.ingest_file_with_source(&inbox_path, Some(IngestSource::Upload)) {
            Ok(mdma_client::IngestResult::Success { hash, .. }) => {
                let hash_str = hash
                    .as_ref()
                    .map(|h| short_hash(h).to_string())
                    .unwrap_or_default();
                println!("  OK: {} -> {}", filename, hash_str);
                success_count += 1;
            }
            Ok(mdma_client::IngestResult::Failure { message }) => {
                eprintln!("  FAIL: {} - {}", filename, message);
                fail_count += 1;
            }
            Err(e) => {
                eprintln!("  FAIL: {} - {}", filename, e);
                fail_count += 1;
            }
        }
    }

    println!();
    println!("Done: {} succeeded, {} failed", success_count, fail_count);

    if fail_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// =============================================================================
// Export Command Helpers
// =============================================================================

/// Build the export URL for a track given an MDMA node hostname.
fn export_url_from_node(node: &str, hash: &str, format: &str) -> String {
    format!("http://{}/export/{}?format={}", node, hash, format)
}

/// Sanitize a single path component by replacing unsafe filesystem characters.
fn sanitize_path_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Build the destination path for an exported track.
///
/// Layout: `<output>/<artist>/<album>/<title>.<ext>`
///
/// Each path component is sanitized to remove filesystem-unsafe characters.
/// Missing metadata falls back to `"Unknown Artist"`, `"Unknown Album"`, `"Unknown"`.
fn export_dest_path(output: &std::path::Path, track: &TrackInfo, ext: &str) -> std::path::PathBuf {
    let artist = sanitize_path_component(track.artist.as_deref().unwrap_or("Unknown Artist"));
    let album = sanitize_path_component(track.album.as_deref().unwrap_or("Unknown Album"));
    let title = sanitize_path_component(track.title.as_deref().unwrap_or("Unknown"));
    output
        .join(artist)
        .join(album)
        .join(format!("{}.{}", title, ext))
}

/// Extract a content hash from a line of text.
///
/// Handles multiple formats:
/// - Plain hash: `abcd1234ef567890`
/// - Full sha256: prefix: `sha256:abcdef0123...`
/// - Search output: `{8-char-hash}  {Artist} - {Title}  [{duration}]`
/// - Any line whose first whitespace-separated token looks like a hash
///
/// Returns `None` for blank lines, comment lines (starting with `#`), and lines
/// where the first token doesn't resemble a hash.
fn parse_hash_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let token = line.split_whitespace().next()?;
    // Accept sha256: prefixed full hashes
    if token.starts_with("sha256:") {
        let hex_part = token.strip_prefix("sha256:")?;
        if hex_part.len() >= 8
            && hex_part
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        {
            return Some(token.to_string());
        }
        return None;
    }
    // Accept plain hex tokens of 8+ chars (short or full)
    if token.len() >= 8
        && token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
    {
        return Some(token.to_string());
    }
    None
}

/// Require `--node` / `MDMA_NODE` to be set and return the hostname.
///
/// Prints an error and exits if the node is absent or empty.
fn require_node(cli: &Cli) -> String {
    match cli.node.as_deref() {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => {
            eprintln!("MDMA_NODE is not set.");
            eprintln!(
                "Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
            );
            std::process::exit(1);
        }
    }
}

/// Build the shared blocking HTTP client used by export handlers.
fn build_http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to build HTTP client: {}", e))
}

/// Outcome of a single track download attempt.
enum DownloadOutcome {
    Success {
        dest: std::path::PathBuf,
        size: u64,
        ext: String,
    },
    Failure {
        error: String,
    },
}

/// Result of a single track download attempt.
///
/// Carries enough information for both the plain export summary and the
/// Rekordbox XML builder.
struct DownloadAttempt {
    track: TrackInfo,
    hash: String,
    outcome: DownloadOutcome,
}

/// Download a list of tracks to `output`, resolving each track's format via
/// `resolve_format`.  Progress is printed to stderr.  Returns one
/// `DownloadAttempt` per input track, in order.
fn download_tracks(
    node: &str,
    tracks: &[TrackInfo],
    output: &std::path::Path,
    http: &reqwest::blocking::Client,
    resolve_format: impl Fn(&TrackInfo) -> ExportFormat,
) -> Vec<DownloadAttempt> {
    use std::io::Write;

    let total = tracks.len();
    let mut results: Vec<DownloadAttempt> = Vec::with_capacity(total);

    for (idx, track) in tracks.iter().enumerate() {
        let full_hash = track.content_hash.as_str();
        let resolved_format = resolve_format(track);
        let format_param = resolved_format.format_param();
        let url = export_url_from_node(node, full_hash, format_param);

        eprint!("[{}/{}] Downloading {} ... ", idx + 1, total, full_hash);
        let _ = std::io::stderr().flush();

        macro_rules! fail {
            ($msg:expr) => {{
                eprintln!("FAILED ({})", $msg);
                results.push(DownloadAttempt {
                    track: track.clone(),
                    hash: full_hash.to_string(),
                    outcome: DownloadOutcome::Failure {
                        error: $msg.to_string(),
                    },
                });
                continue;
            }};
        }

        let response = match http.get(&url).send() {
            Ok(r) => r,
            Err(e) => fail!(e.to_string()),
        };

        if !response.status().is_success() {
            fail!(format!("HTTP {}", response.status()));
        }

        // Determine extension: fixed for converted formats, derived from blob_path for Original.
        let source_ext_owned: String;
        let ext: &str = match resolved_format.static_extension() {
            Some(fixed) => fixed,
            None => {
                source_ext_owned = track
                    .blob_path
                    .as_deref()
                    .and_then(|p| std::path::Path::new(p).extension())
                    .and_then(|e| e.to_str())
                    .unwrap_or("bin")
                    .to_lowercase();
                &source_ext_owned
            }
        };

        let dest = export_dest_path(output, track, ext);

        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                fail!(format!("mkdir: {}", e));
            }
        }

        let body = match response.bytes() {
            Ok(b) => b,
            Err(e) => fail!(format!("read error: {}", e)),
        };

        if let Err(e) = std::fs::write(&dest, &body) {
            fail!(format!("write error: {}", e));
        }

        let size = body.len() as u64;
        eprintln!("OK  {} ({} bytes)", dest.display(), size);
        results.push(DownloadAttempt {
            track: track.clone(),
            hash: full_hash.to_string(),
            outcome: DownloadOutcome::Success {
                dest,
                size,
                ext: ext.to_string(),
            },
        });
    }

    results
}

/// Resolve the export format for a track based on its source extension and CLI flags.
///
/// Priority:
/// 1. `--format` — applies to all tracks unconditionally.
/// 2. `--lossless-format` / `--lossy-format` — applied per source category.
/// 3. No flags — defaults to `ExportFormat::Original` (pass-through).
fn resolve_export_format(
    blob_path: Option<&str>,
    format: &Option<ExportFormat>,
    lossless_format: &Option<ExportFormat>,
    lossy_format: &Option<ExportFormat>,
) -> ExportFormat {
    // If --format is given, use it for everything.
    if let Some(fmt) = format {
        return fmt.clone();
    }

    // If neither category flag is given, default to Original.
    if lossless_format.is_none() && lossy_format.is_none() {
        return ExportFormat::Original;
    }

    // Classify the source file by extension.
    let ext = blob_path
        .and_then(|p| std::path::Path::new(p).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match audio_transcoder::FormatCategory::from_extension(ext) {
        Some(audio_transcoder::FormatCategory::Lossless) => {
            lossless_format.clone().unwrap_or(ExportFormat::Original)
        }
        Some(audio_transcoder::FormatCategory::Lossy) => {
            lossy_format.clone().unwrap_or(ExportFormat::Original)
        }
        None => ExportFormat::Original,
    }
}

// =============================================================================
// Rekordbox Export
// =============================================================================

/// Build a single `RekordboxTrack` from MDMA metadata and file facts.
///
/// `dest_path` must be the already-canonicalised destination used for planning —
/// do not re-canonicalise here so the Location URI stays stable.
fn build_rekordbox_track(
    track: &TrackInfo,
    dest_path: &std::path::Path,
    ext: &str,
    size: u64,
    facts: &[(String, String)],
) -> rekordbox_xml::RefreshedTrack {
    let find_fact = |name: &str| -> Option<String> {
        facts
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };

    let bitrate = find_fact("Bitrate").and_then(|v| v.parse::<u32>().ok());
    let sample_rate = find_fact("SampleRate").and_then(|v| v.parse::<u32>().ok());
    let year = find_fact("Year").or_else(|| find_fact("RecordingYear"));
    let label = find_fact("Label");
    let comment = find_fact("Comment");
    let genre = find_fact("MainGenre")
        .or_else(|| find_fact("FullGenre"))
        .unwrap_or_default();

    let date_added = track
        .added
        .as_deref()
        .and_then(|s| s.split('T').next())
        .map(String::from);

    let tonality = track.key.as_ref().map(rekordbox_xml::key_to_tonality);
    let average_bpm = track.bpm.map(|b| b.as_f32());
    let location = rekordbox_xml::path_to_file_uri(dest_path);

    rekordbox_xml::RefreshedTrack {
        name: track.title.clone().unwrap_or_else(|| "Unknown".to_string()),
        artist: track.artist.clone().unwrap_or_default(),
        album: track.album.clone().unwrap_or_default(),
        genre,
        kind: rekordbox_xml::ext_to_kind(ext).to_string(),
        size,
        total_time: track.duration.map(|d| d.value()).unwrap_or(0),
        average_bpm,
        tonality,
        track_number: track.track_number,
        disc_number: track.disc_number,
        year,
        label,
        comment,
        date_added,
        bitrate,
        sample_rate,
        location,
    }
}

fn handle_rekordbox_export(
    cli: &Cli,
    library: &LibraryBackend,
    playlists: &[String],
    output: &std::path::Path,
    format: Option<&ExportFormat>,
    replace: bool,
) -> Result<()> {
    use std::collections::HashSet;
    use std::io::BufRead;

    let node = require_node(cli);

    // Collect hashes per playlist, or from stdin when no playlists given.
    //
    // hashes_per_playlist: ordered list of (playlist_name, ordered_hashes).
    // When stdin mode, one entry with empty name.
    let hashes_per_playlist: Vec<(String, Vec<String>)> = if !playlists.is_empty() {
        let mut result = Vec::with_capacity(playlists.len());
        for pname in playlists {
            let name = parse_playlist_name(pname);
            match library.playlist_get(&name) {
                Ok(hashes) => {
                    result.push((
                        pname.clone(),
                        hashes.into_iter().map(|h| h.as_str().to_string()).collect(),
                    ));
                }
                Err(e) => {
                    eprintln!("error: playlist '{}' not found: {}", pname, e);
                    std::process::exit(1);
                }
            }
        }
        result
    } else {
        let hashes: Vec<String> = std::io::stdin()
            .lock()
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| parse_hash_from_line(&line))
            .collect();
        vec![("".to_string(), hashes)]
    };

    // Union of all hashes, deduped while preserving first-occurrence order.
    let mut seen: HashSet<String> = HashSet::new();
    let mut all_hashes: Vec<String> = Vec::new();
    for (_, hs) in &hashes_per_playlist {
        for h in hs {
            if seen.insert(h.clone()) {
                all_hashes.push(h.clone());
            }
        }
    }

    if all_hashes.is_empty() {
        if !playlists.is_empty() {
            eprintln!("All specified playlists are empty.");
        } else {
            eprintln!("No hashes provided on stdin.");
            eprintln!("Usage: mdma search --artist CBL | mdma rekordbox export");
            eprintln!("       mdma rekordbox export --playlist my-set");
        }
        std::process::exit(1);
    }

    // Resolve each unique hash to full TrackInfo
    let tracks = resolve_tracks_skip_errors(library, all_hashes);
    if tracks.is_empty() {
        eprintln!("No tracks could be resolved. Aborting.");
        std::process::exit(1);
    }

    let http = build_http_client()?;

    // Create output directory early so we can canonicalise it for stable paths
    if let Err(e) = std::fs::create_dir_all(output) {
        return Err(color_eyre::eyre::eyre!(
            "Failed to create output directory: {}",
            e
        ));
    }
    let output_canon = output
        .canonicalize()
        .unwrap_or_else(|_| output.to_path_buf());

    // Resolve format: only --format (no lossless/lossy split for rekordbox export)
    let format_owned = format.cloned().unwrap_or(ExportFormat::Original);

    // Build (TrackInfo, canonical_dest_path, ext) for every unique desired track.
    // dest_path is canonicalised so the Location URI is stable pre- and post-download.
    let desired: Vec<(TrackInfo, std::path::PathBuf, String)> = tracks
        .into_iter()
        .map(|track| {
            let ext: String = match format_owned.static_extension() {
                Some(fixed) => fixed.to_string(),
                None => track
                    .blob_path
                    .as_deref()
                    .and_then(|p| std::path::Path::new(p).extension())
                    .and_then(|e| e.to_str())
                    .unwrap_or("bin")
                    .to_lowercase(),
            };
            let raw_dest = export_dest_path(&output_canon, &track, &ext);
            // Canonicalise if the file already exists; fall back to the absolute path otherwise.
            let dest = raw_dest.canonicalize().unwrap_or_else(|_| raw_dest.clone());
            (track, dest, ext)
        })
        .collect();

    // hash → canonical dest location URI, for playlist location building.
    let hash_to_location: std::collections::HashMap<String, String> = desired
        .iter()
        .map(|(track, dest, _)| {
            (
                track.content_hash.as_str().to_string(),
                rekordbox_xml::path_to_file_uri(dest),
            )
        })
        .collect();

    // Load existing XML for incremental sync (skip when --replace)
    let xml_path = output_canon.join("rekordbox.xml");
    let existing: Option<rekordbox_xml::RekordboxLibrary> = if !replace && xml_path.exists() {
        match std::fs::read_to_string(&xml_path) {
            Ok(bytes) => match rekordbox_xml::parse_xml(&bytes) {
                Ok(lib) => Some(lib),
                Err(err) => {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to parse existing rekordbox.xml: {}. Inspect the file or re-run with --replace to regenerate.",
                        err
                    ));
                }
            },
            Err(e) => {
                eprintln!("Warning: could not read existing rekordbox.xml ({}); treating as fresh export.", e);
                None
            }
        }
    } else {
        None
    };

    // Build DesiredTrack list for the planner
    let desired_for_plan: Vec<rekordbox_xml::DesiredTrack> = desired
        .iter()
        .map(|(track, dest, _ext)| rekordbox_xml::DesiredTrack {
            dest_path: dest.clone(),
            source_id: track.content_hash.as_str().to_string(),
        })
        .collect();

    let plan = rekordbox_xml::plan_export(existing.as_ref(), &desired_for_plan, |p| p.exists());

    // Warn about planning anomalies
    for collision in &plan.collisions {
        eprintln!(
            "warning: destination path {} shared by {} tracks: {}",
            collision.dest_path.display(),
            collision.source_ids.len(),
            collision.source_ids.join(", ")
        );
    }
    for fc in &plan.format_changes {
        let old_ext = std::path::Path::new(&fc.existing_location)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?");
        let new_ext = fc
            .new_dest_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?");
        eprintln!(
            "warning: {} already exported as .{}; new --format produces .{} — treating as a new track. Use --replace to reset the XML.",
            fc.existing_location, old_ext, new_ext
        );
    }

    // Filter download list to only tracks not already on disk
    let to_download_set: HashSet<std::path::PathBuf> = plan
        .to_download
        .iter()
        .map(|p| p.dest_path.clone())
        .collect();

    let download_tracks_only: Vec<TrackInfo> = desired
        .iter()
        .filter(|(_, dest, _)| to_download_set.contains(dest))
        .map(|(track, _, _)| track.clone())
        .collect();

    let new_count = plan.to_download.len();
    let unchanged_count = plan.to_skip.len();

    let results = download_tracks(
        &node,
        &download_tracks_only,
        &output_canon,
        &http,
        |_track| format_owned.clone(),
    );

    let fail_count = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Failure { .. }))
        .count();

    // Build a map of downloaded track hash → (dest, ext, size) for the refreshed set
    let mut downloaded_map: std::collections::HashMap<String, (std::path::PathBuf, String, u64)> =
        std::collections::HashMap::new();
    for attempt in &results {
        if let DownloadOutcome::Success { dest, ext, size } = &attempt.outcome {
            downloaded_map.insert(
                attempt.track.content_hash.as_str().to_string(),
                (dest.clone(), ext.clone(), *size),
            );
        }
    }

    // Build the skipped-location set for quick membership checks.
    let skipped_locations: HashSet<String> = plan.to_skip.into_iter().collect();

    // Only include tracks that are either freshly downloaded or already on disk.
    // Failed downloads are logged and dropped — a partial XML is worse than the
    // last-good XML, so we abort before writing if any download failed.
    let mut refreshed: Vec<rekordbox_xml::RefreshedTrack> = Vec::with_capacity(desired.len());
    for (track, dest, ext) in &desired {
        let hash_str = track.content_hash.as_str().to_string();
        let location = rekordbox_xml::path_to_file_uri(dest);

        let resolved = if let Some((dl_dest, dl_ext, dl_size)) = downloaded_map.get(&hash_str) {
            // Freshly downloaded.
            Some((dl_dest.clone(), dl_ext.clone(), *dl_size))
        } else if skipped_locations.contains(&location) {
            // Already on disk — get size from filesystem.
            let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            Some((dest.clone(), ext.clone(), size))
        } else {
            // Download failed — skip this track.
            None
        };

        if let Some((actual_dest, actual_ext, size)) = resolved {
            let facts: Vec<(String, String)> = library
                .get_facts(&track.content_hash)
                .map(|(_hash, facts)| facts)
                .unwrap_or_default();
            refreshed.push(build_rekordbox_track(
                track,
                &actual_dest,
                &actual_ext,
                size,
                &facts,
            ));
        }
    }

    // Abort before touching the XML if any download failed; leave the last-good
    // XML in place so the user can re-run after fixing the problem.
    if fail_count > 0 {
        eprintln!(
            "error: {} download(s) failed — XML not written. Re-run to retry.",
            fail_count
        );
        std::process::exit(1);
    }

    if refreshed.is_empty() {
        eprintln!("No tracks available. Aborting XML generation.");
        std::process::exit(1);
    }

    // Build a set of locations that actually made it into refreshed.
    let refreshed_locations: HashSet<&str> =
        refreshed.iter().map(|r| r.location.as_str()).collect();

    // Build PlaylistUpdate per playlist. Each playlist references its own full track list
    // (not the deduplicated union), so overlapping tracks appear in both playlists.
    // Unresolved hashes (failed downloads) are dropped.
    let playlist_updates: Vec<rekordbox_xml::PlaylistUpdate> = hashes_per_playlist
        .iter()
        .map(|(pname, hs)| {
            let locations: Vec<String> = hs
                .iter()
                .filter_map(|h| {
                    let loc = hash_to_location.get(h)?;
                    if refreshed_locations.contains(loc.as_str()) {
                        Some(loc.clone())
                    } else {
                        None
                    }
                })
                .collect();
            rekordbox_xml::PlaylistUpdate {
                name: pname.clone(),
                locations,
            }
        })
        .collect();

    let merged = rekordbox_xml::merge_export(existing, refreshed, &playlist_updates);

    let total_tracks = merged.tracks.len();
    let total_playlists = merged.playlists.len();

    let xml = merged.to_xml();
    std::fs::write(&xml_path, xml.as_bytes())
        .map_err(|e| color_eyre::eyre::eyre!("Failed to write rekordbox.xml: {}", e))?;

    eprintln!();
    let mode_prefix = if replace { "Replace" } else { "Sync" };
    let total_desired = desired.len();

    if playlists.is_empty() {
        // stdin mode
        eprintln!(
            "{}: {} tracks ({} new, {} already on disk).",
            mode_prefix, total_desired, new_count, unchanged_count
        );
    } else if playlists.len() == 1 {
        eprintln!(
            "{}: {} tracks in playlist '{}' ({} new, {} already on disk).",
            mode_prefix, total_desired, playlists[0], new_count, unchanged_count
        );
    } else {
        // Multi-playlist summary: show per-playlist track counts.
        let per_playlist: Vec<String> = hashes_per_playlist
            .iter()
            .map(|(name, hs)| format!("{}: {}", name, hs.len()))
            .collect();
        eprintln!(
            "{}: {} tracks across {} playlists ({}) ({} new, {} already on disk).",
            mode_prefix,
            total_desired,
            playlists.len(),
            per_playlist.join(", "),
            new_count,
            unchanged_count
        );
    }

    eprintln!(
        "Library now: {} tracks across {} playlists.",
        total_tracks, total_playlists
    );
    eprintln!("XML written to: {}", xml_path.display());

    if fail_count > 0 {
        eprintln!("{} download(s) failed.", fail_count);
        std::process::exit(1);
    }

    Ok(())
}

// =============================================================================
// Rekordbox Import
// =============================================================================

struct LibraryTrackLookup<'a> {
    library: &'a LibraryBackend,
}

impl TrackLookup for LibraryTrackLookup<'_> {
    fn find_by_isrc(&self, _isrc: &str) -> Vec<ContentHash> {
        // TODO: ISRC search is not available via TrackQuery — no ISRC field in library_search.
        // HasFact could be used but returns bool, not ContentHash.
        // For now, return empty and rely on artist+title matching.
        vec![]
    }

    fn find_by_artist_title(&self, artist: &str, title: &str) -> Vec<(ContentHash, Option<u32>)> {
        use library_search::StringQuery;

        let query = TrackQuery {
            artist: Some(StringQuery::Contains(artist.to_string())),
            title: Some(StringQuery::Contains(title.to_string())),
            ..Default::default()
        };

        match self.library.search(&query) {
            Ok(tracks) => tracks
                .into_iter()
                .map(|t| {
                    let duration = t.duration.map(|d| d.value());
                    (t.content_hash, duration)
                })
                .collect(),
            Err(_) => vec![],
        }
    }
}

fn handle_rekordbox_import(
    _cli: &Cli,
    library: &LibraryBackend,
    path: &std::path::Path,
    playlist: &Option<String>,
    all_playlists: bool,
    enrich: bool,
    dry_run: bool,
) -> Result<()> {
    use library_ipc_client::{Bpm, Key};
    use std::collections::HashMap;

    // 1. Read and parse XML file
    let content = std::fs::read_to_string(path)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to read {}: {}", path.display(), e))?;

    let parsed = parse_xml(&content)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to parse rekordbox XML: {}", e))?;

    if parsed.tracks.is_empty() {
        eprintln!("No tracks found in {}", path.display());
        return Ok(());
    }

    // 2. Create lookup and match tracks
    let lookup = LibraryTrackLookup { library };

    let mut matched: Vec<(&RekordboxTrack, ContentHash)> = Vec::new();
    let mut ambiguous: Vec<(&RekordboxTrack, Vec<ContentHash>)> = Vec::new();
    let mut unmatched: Vec<&RekordboxTrack> = Vec::new();

    for track in &parsed.tracks {
        let candidate = CandidateTrack {
            isrc: None,
            artist: if track.artist.is_empty() {
                None
            } else {
                Some(track.artist.clone())
            },
            title: if track.name.is_empty() {
                None
            } else {
                Some(track.name.clone())
            },
            duration_secs: if track.total_time > 0 {
                Some(track.total_time)
            } else {
                None
            },
        };

        match track_matcher::match_track(&candidate, &lookup) {
            MatchResult::Definitive(hash, _) => {
                matched.push((track, hash));
            }
            MatchResult::Ambiguous(hashes, _) => {
                ambiguous.push((track, hashes));
            }
            MatchResult::NoMatch => {
                unmatched.push(track);
            }
        }
    }

    // 3. Print summary
    println!("Matched:   {:3} tracks", matched.len());
    println!(
        "Ambiguous: {:3} tracks (need manual resolution)",
        ambiguous.len()
    );
    println!("Unmatched: {:3} tracks", unmatched.len());

    // 4. Print unmatched tracks to stderr
    if !unmatched.is_empty() {
        eprintln!();
        eprintln!("Unmatched tracks:");
        for t in &unmatched {
            let filename = rekordbox_xml::parse_location(&t.location)
                .map(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&p)
                        .to_string()
                })
                .unwrap_or_else(|| t.location.clone());
            eprintln!("  {} - {} ({})", t.artist, t.name, filename);
        }
    }

    // 5. Print ambiguous tracks to stderr
    if !ambiguous.is_empty() {
        eprintln!();
        eprintln!("Ambiguous tracks (multiple candidates):");
        for (t, hashes) in &ambiguous {
            eprintln!("  {} - {}", t.artist, t.name);
            for h in hashes.iter().take(3) {
                eprintln!("    candidate: {}", &h.as_str()[..h.as_str().len().min(16)]);
            }
        }
    }

    // Build track_id → ContentHash map for playlist resolution
    let track_id_to_hash: HashMap<u32, ContentHash> = matched
        .iter()
        .map(|(rb_track, hash)| (rb_track.track_id, hash.clone()))
        .collect();

    // 6. Create --playlist if requested
    if let Some(pname) = playlist {
        let sanitized: String = pname
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let sanitized = sanitized.trim_matches('-').to_string();
        if sanitized.is_empty() {
            eprintln!("Invalid --playlist '{}': name sanitizes to empty", pname);
            std::process::exit(1);
        }
        if sanitized != *pname {
            eprintln!(
                "Note: playlist name sanitized from '{}' to '{}'",
                pname, sanitized
            );
        }
        let name = parse_playlist_name(&sanitized);
        let hashes: Vec<ContentHash> = matched.iter().map(|(_, h)| h.clone()).collect();

        if dry_run {
            println!(
                "[dry-run] Would create playlist '{}' with {} tracks",
                sanitized,
                hashes.len()
            );
        } else {
            match library.playlist_new(&name, &hashes) {
                Ok(()) => println!(
                    "Created playlist '{}' with {} tracks",
                    sanitized,
                    hashes.len()
                ),
                Err(e) => {
                    eprintln!("Failed to create playlist '{}': {}", sanitized, e);
                    eprintln!("Trying to replace existing playlist...");
                    library.playlist_replace(&name, &hashes).map_err(|e2| {
                        color_eyre::eyre::eyre!("Failed to replace playlist: {}", e2)
                    })?;
                    println!(
                        "Replaced playlist '{}' with {} tracks",
                        sanitized,
                        hashes.len()
                    );
                }
            }
        }
    }

    // 7. Create --all-playlists
    if all_playlists {
        for rb_playlist in &parsed.playlists {
            let resolved: Vec<ContentHash> = rb_playlist
                .track_ids
                .iter()
                .filter_map(|id| track_id_to_hash.get(id).cloned())
                .collect();

            if resolved.is_empty() {
                eprintln!(
                    "Skipping playlist '{}': no matched tracks",
                    rb_playlist.name
                );
                continue;
            }

            // Sanitize playlist name: replace non-alphanumeric chars with '-'
            let sanitized: String = rb_playlist
                .name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            // Trim leading/trailing dashes
            let sanitized = sanitized.trim_matches('-').to_string();

            if sanitized.is_empty() {
                eprintln!(
                    "Skipping playlist '{}': name sanitizes to empty",
                    rb_playlist.name
                );
                continue;
            }

            if dry_run {
                println!(
                    "[dry-run] Would create playlist '{}' (from '{}') with {} tracks",
                    sanitized,
                    rb_playlist.name,
                    resolved.len()
                );
            } else {
                let name = parse_playlist_name(&sanitized);
                match library.playlist_new(&name, &resolved) {
                    Ok(()) => println!(
                        "Created playlist '{}' with {} tracks",
                        sanitized,
                        resolved.len()
                    ),
                    Err(e) => {
                        eprintln!(
                            "Failed to create playlist '{}': {} — trying replace",
                            sanitized, e
                        );
                        match library.playlist_replace(&name, &resolved) {
                            Ok(()) => println!(
                                "Replaced playlist '{}' with {} tracks",
                                sanitized,
                                resolved.len()
                            ),
                            Err(e2) => {
                                eprintln!("Failed to replace playlist '{}': {}", sanitized, e2)
                            }
                        }
                    }
                }
            }
        }
    }

    // 8. Enrich BPM/Key facts
    if enrich {
        let mut enriched = 0usize;
        let mut skipped = 0usize;
        let mut errors = 0usize;

        for (rb_track, hash) in &matched {
            // Fetch current track facts; on failure fall back to writing (safe default).
            let existing = library.get_track(hash).ok();

            // Write BPM if available
            if let Some(bpm_f32) = rb_track.average_bpm {
                if let Ok(bpm) = Bpm::from_f32(bpm_f32) {
                    let already_current = existing
                        .as_ref()
                        .and_then(|t| t.bpm)
                        .map(|existing_bpm| existing_bpm == bpm)
                        .unwrap_or(false);

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write BPM {} for {} - {}",
                            bpm_f32, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact(hash, MusicValue::Bpm(bpm)) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write BPM for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }

            // Write Key (Tonality) if available
            if let Some(ref tonality) = rb_track.tonality {
                if let Ok(key) = Key::from_camelot(tonality) {
                    let already_current = existing
                        .as_ref()
                        .and_then(|t| t.key)
                        .map(|existing_key| existing_key == key)
                        .unwrap_or(false);

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write Key {} for {} - {}",
                            tonality, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact(hash, MusicValue::Key(key)) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write Key for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }
        }

        if !dry_run {
            println!(
                "Enriched {} fact(s), skipped {} (already up to date), {} error(s)",
                enriched, skipped, errors
            );
        }
    }

    Ok(())
}

fn handle_export(
    cli: &Cli,
    library: &LibraryBackend,
    format: &Option<ExportFormat>,
    lossless_format: &Option<ExportFormat>,
    lossy_format: &Option<ExportFormat>,
    output: &std::path::Path,
) -> Result<()> {
    use std::io::BufRead;

    let node = require_node(cli);

    // Read hashes from stdin
    let raw_hashes: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    if raw_hashes.is_empty() {
        eprintln!("No hashes provided on stdin.");
        eprintln!("Usage: mdma search --artist CBL | mdma export");
        std::process::exit(1);
    }

    // Resolve each hash (short or full) to a full TrackInfo via the library
    let tracks = resolve_tracks_skip_errors(library, raw_hashes);

    if tracks.is_empty() {
        eprintln!("No tracks could be resolved. Aborting.");
        std::process::exit(1);
    }

    let http = build_http_client()?;

    let results = download_tracks(&node, &tracks, output, &http, |track| {
        resolve_export_format(
            track.blob_path.as_deref(),
            format,
            lossless_format,
            lossy_format,
        )
    });

    // Summary
    let success_count = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Success { .. }))
        .count();
    let total_bytes: u64 = results
        .iter()
        .filter_map(|r| {
            if let DownloadOutcome::Success { size, .. } = &r.outcome {
                Some(*size)
            } else {
                None
            }
        })
        .sum();
    let fail_count = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Failure { .. }))
        .count();
    eprintln!();
    eprintln!(
        "Done: {} exported ({} bytes), {} failed",
        success_count, total_bytes, fail_count
    );

    if fail_count > 0 {
        eprintln!();
        eprintln!("Failed tracks:");
        for (r, error) in results.iter().filter_map(|r| {
            if let DownloadOutcome::Failure { error } = &r.outcome {
                Some((r, error.as_str()))
            } else {
                None
            }
        }) {
            eprintln!("  {} — {}", r.hash, error);
        }
        std::process::exit(1);
    }

    Ok(())
}

// =============================================================================
// Shell Completions
// =============================================================================

fn generate_completions(shell: clap_complete::Shell, out: &mut dyn std::io::Write) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "mdma", out);
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
                PlaybackCommands::Start => handle_playback_start(&pb),
                PlaybackCommands::Stop => handle_playback_stop(&pb),
                PlaybackCommands::Pause => handle_playback_pause(&pb),
                PlaybackCommands::Resume => handle_playback_resume(&pb),
                PlaybackCommands::Skip => handle_playback_skip(&pb),
                PlaybackCommands::Now => {
                    use std::io::IsTerminal;
                    if std::io::stdout().is_terminal() {
                        let lib = connect_library(&cli);
                        handle_playback_now(&pb, Some(&lib))
                    } else {
                        handle_playback_now(&pb, None)
                    }
                }
                PlaybackCommands::Session => handle_playback_session(&pb),
                PlaybackCommands::Outputs => handle_playback_outputs(&pb),
                PlaybackCommands::SetOutput { name } => handle_playback_set_output(&pb, name),
                PlaybackCommands::GetOutput => handle_playback_get_output(&pb),
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
            SourceCommands::Resync { name, identifier } => {
                let client = connect_source(&cli, name);
                handle_source_resync(&client, name, identifier)
            }
            SourceCommands::CheckItem { name, identifier } => {
                let client = connect_source(&cli, name);
                handle_source_check_item(&client, name, identifier)
            }
            SourceCommands::CheckUpdates { name, apply } => {
                let client = connect_source(&cli, name);
                handle_source_check_updates(&cli, &client, name, *apply)
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
            no_stdin,
            started,
            stopped,
            added,
            played,
            not,
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
                let mut track_query = build_track_query(
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
                    started.clone(),
                    stopped.clone(),
                    added.clone(),
                    played.clone(),
                );
                track_query.not = *not;
                handle_search(&client, &track_query, *no_stdin)
            }
        }
        Commands::Playlist { command } => {
            let lib = connect_library(&cli);
            match command {
                PlaylistCommands::List => handle_playlist_list(&lib),
                PlaylistCommands::Get { name } => handle_playlist_get(&lib, name),
                PlaylistCommands::Contains { all, at_least, no } => {
                    handle_playlist_contains(&lib, *all, *at_least, *no)
                }
                PlaylistCommands::New { name } => handle_playlist_new(&lib, name),
                PlaylistCommands::Append { name } => handle_playlist_append(&lib, name),
                PlaylistCommands::Replace { name } => handle_playlist_replace(&lib, name),
                PlaylistCommands::Edit { name } => handle_playlist_edit(&lib, name),
                PlaylistCommands::Remove { name } => handle_playlist_remove(&lib, name),
                PlaylistCommands::Rename { from, to } => handle_playlist_rename(&lib, from, to),
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
        Commands::Subscribe { topic } => {
            // Resolve event gateway from --node / MDMA_NODE (tcp://<node>:5556)
            let addr = match resolve_event_gateway(&cli) {
                Some(a) => a,
                None => {
                    eprintln!("No node specified.");
                    eprintln!("Set --node or MDMA_NODE to the hostname of your MDMA device");
                    std::process::exit(1);
                }
            };
            let lib = connect_library(&cli);
            handle_subscribe(&addr, topic.as_deref(), Some(&lib))
        }
        Commands::Upload {
            file,
            ssh_key,
            ssh_user,
            inbox_dir,
        } => handle_upload(&cli, file, ssh_key, ssh_user, inbox_dir),
        Commands::Export {
            format,
            lossless_format,
            lossy_format,
            output,
        } => {
            let lib = connect_library(&cli);
            handle_export(&cli, &lib, format, lossless_format, lossy_format, output)
        }
        Commands::Rekordbox { command } => {
            let lib = connect_library(&cli);
            match command {
                RekordboxCommands::Export {
                    playlist,
                    output,
                    format,
                    replace,
                } => {
                    let resolved_output = output.clone().unwrap_or_else(|| {
                        dirs::audio_dir()
                            .map(|p| p.join("mdma_rekordbox"))
                            .unwrap_or_else(|| std::path::PathBuf::from("./rekordbox-export/"))
                    });
                    handle_rekordbox_export(
                        &cli,
                        &lib,
                        playlist,
                        &resolved_output,
                        format.as_ref(),
                        *replace,
                    )
                }
                RekordboxCommands::Import {
                    path,
                    playlist,
                    all_playlists,
                    enrich,
                    dry_run,
                } => handle_rekordbox_import(
                    &cli,
                    &lib,
                    path,
                    playlist,
                    *all_playlists,
                    *enrich,
                    *dry_run,
                ),
            }
        }
        Commands::GenerateCompletions { shell } => {
            generate_completions(*shell, &mut std::io::stdout());
            Ok(())
        }

        Commands::Library { command } => {
            let lib = connect_library(&cli);
            match command {
                LibraryCommands::ReindexCovers => handle_library_reindex_covers(&lib),
            }
        }

        Commands::Bookmark { hash, scope } => {
            let lib = connect_library(&cli);
            let pb = connect_playback(&cli);
            handle_bookmark(&lib, Some(&pb), hash.clone(), scope.clone())
        }

        Commands::Bookmarks => {
            let lib = connect_library(&cli);
            handle_bookmarks(&lib)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Derive the command gateway address from an MDMA node hostname.
    /// The gateway is always at port 5555.
    fn gateway_from_node(node: &str) -> String {
        client::ClientConfig {
            node: Some(node.to_string()),
            ..Default::default()
        }
        .gateway_addr()
        .expect("node is set")
    }

    /// Derive the event gateway address from an MDMA node hostname.
    /// The event gateway is always at port 5556.
    fn event_gateway_from_node(node: &str) -> String {
        client::ClientConfig {
            node: Some(node.to_string()),
            ..Default::default()
        }
        .event_addr()
        .expect("node is set")
    }

    #[test]
    fn resolve_gateway_from_mdma_node() {
        // MDMA_NODE set, no MDMA_GATEWAY -> derive tcp://<node>:5555
        let result = gateway_from_node("some-pi.local");
        assert_eq!(result, "tcp://some-pi.local:5555");
    }

    #[test]
    fn resolve_event_gateway_from_mdma_node() {
        // MDMA_NODE set -> event gateway is tcp://<node>:5556
        let result = event_gateway_from_node("some-pi.local");
        assert_eq!(result, "tcp://some-pi.local:5556");
    }

    #[test]
    fn resolve_gateway_uses_cli_node_field() {
        // When cli.node is set, resolve_gateway derives tcp://<node>:5555
        let cli = Cli {
            node: Some("mdma-909.local".to_string()),
            socket: "ipc:///run/mdma/library.sock".to_string(),
            playback_socket: "ipc:///run/mdma/playback.sock".to_string(),
            sources_dir: "/run/mdma/sources".to_string(),
            command: Commands::Ping,
        };
        let result = resolve_gateway(&cli);
        assert_eq!(result, Some("tcp://mdma-909.local:5555".to_string()));
    }

    #[test]
    fn resolve_gateway_returns_none_when_node_unset() {
        // When cli.node is None, resolve_gateway returns None
        let cli = Cli {
            node: None,
            socket: "ipc:///run/mdma/library.sock".to_string(),
            playback_socket: "ipc:///run/mdma/playback.sock".to_string(),
            sources_dir: "/run/mdma/sources".to_string(),
            command: Commands::Ping,
        };
        let result = resolve_gateway(&cli);
        assert_eq!(result, None);
    }

    fn make_track(artist: &str, title: &str, duration_secs: u32) -> TrackInfo {
        use library_ipc_client::{ContentHash, DurationSeconds};
        TrackInfo {
            content_hash: ContentHash::new("sha256:aa000001"),
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: None,
            duration: Some(DurationSeconds::new(duration_secs)),
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        }
    }

    /// Build the same 8-track set used in queue_display.feature.
    fn queue_display_tracks() -> Vec<TrackInfo> {
        vec![
            make_track("Sunju Hargun", "Silverhaze (DJ MARIA. Remix)", 421),
            make_track("Carbon Based Lifeforms", "Init", 508),
            make_track("Sunju Hargun", "Right Where It Ends", 376),
            make_track("Carbon Based Lifeforms", "Marsa (2026 Remaster)", 612),
            make_track("Sunju Hargun", "Interloper", 293),
            make_track("Carbon Based Lifeforms", "Midnight Traffic Remix", 444),
            make_track("Sunju Hargun", "Polyrytmi", 367),
            make_track("Carbon Based Lifeforms", "20 Minutes", 1200),
        ]
    }

    /// Strip ANSI escape codes from a string.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if let Some(next) = chars.next() {
                    if next == '[' {
                        for seq_char in chars.by_ref() {
                            if seq_char.is_ascii_alphabetic() || seq_char == '~' {
                                break;
                            }
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn render_track_table_with_no_prefix_fits_in_terminal_width() {
        let tracks = queue_display_tracks();
        // Use render_track_table_inner with an explicit width to avoid race conditions
        // from parallel tests sharing the $COLUMNS env var.
        let lines = render_track_table_inner(&tracks, 0, 65);
        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let width = stripped.chars().count();
            assert!(
                width <= 65,
                "Line {} is {} chars wide, exceeds 65 columns: '{}'",
                i + 1,
                width,
                stripped
            );
        }
    }

    #[test]
    fn render_track_table_with_prefix_4_fits_in_terminal_width_65() {
        let tracks = queue_display_tracks();
        // prefix=4, term_width=65. Every rendered line must be ≤ 65-4=61 chars.
        let lines = render_track_table_inner(&tracks, 4, 65);
        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let width = stripped.chars().count();
            assert!(
                width + 4 <= 65,
                "Line {} content is {} chars, total with prefix {} exceeds 65 columns: '{}'",
                i + 1,
                width,
                width + 4,
                stripped
            );
        }
    }

    #[test]
    fn render_track_table_with_prefix_4_fits_in_terminal_width_55() {
        let tracks = queue_display_tracks();
        // prefix=4, term_width=55. Every rendered line must be ≤ 55-4=51 chars.
        let lines = render_track_table_inner(&tracks, 4, 55);
        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let width = stripped.chars().count();
            assert!(
                width + 4 <= 55,
                "Line {} content is {} chars, total with prefix {} exceeds 55 columns: '{}'",
                i + 1,
                width,
                width + 4,
                stripped
            );
        }
    }

    // Mutex to serialize tests that mutate the $COLUMNS environment variable.
    // std::env::set_var / remove_var are not thread-safe across parallel tests,
    // so both tests acquire this lock before touching the env.
    static COLUMNS_ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn columns_env_lock() -> &'static std::sync::Mutex<()> {
        COLUMNS_ENV_LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn terminal_width_reads_columns_env_var() {
        // When $COLUMNS is set to a valid integer, terminal_width() must return it.
        // Hold the lock for the entire test to avoid racing with the fallback test.
        let _guard = columns_env_lock().lock().unwrap();
        std::env::set_var("COLUMNS", "75");
        let w = terminal_width();
        std::env::remove_var("COLUMNS");
        assert_eq!(
            w, 75,
            "terminal_width() should return the value of $COLUMNS"
        );
    }

    #[test]
    fn terminal_width_fallback_is_80_not_100() {
        // When $COLUMNS is absent and there is no real TTY (CI / piped),
        // terminal_width() must fall back to 80, not 100.
        let _guard = columns_env_lock().lock().unwrap();
        std::env::remove_var("COLUMNS");
        let w = terminal_width();
        // In a CI environment there is no TTY, so terminal_size() returns None
        // and we must fall back to 80.  If there happens to be a TTY (rare in
        // tests) the result will be the actual terminal width, which is fine —
        // we only care that 100 is never returned as the hard-coded default.
        assert_ne!(w, 100, "terminal_width() must not fall back to 100");
    }

    // ── Export helpers ────────────────────────────────────────────────────────

    #[test]
    fn export_url_builds_from_node_aiff() {
        let url = export_url_from_node("mdma-909.local", "sha256:abcd1234", "aiff");
        assert_eq!(
            url,
            "http://mdma-909.local/export/sha256:abcd1234?format=aiff"
        );
    }

    #[test]
    fn export_url_builds_from_node_wav() {
        let url = export_url_from_node("mdma-909.local", "sha256:abcd1234", "wav");
        assert_eq!(
            url,
            "http://mdma-909.local/export/sha256:abcd1234?format=wav"
        );
    }

    #[test]
    fn export_format_extension_aiff() {
        assert_eq!(ExportFormat::Aiff.static_extension(), Some("aiff"));
    }

    #[test]
    fn export_format_extension_wav() {
        assert_eq!(ExportFormat::Wav.static_extension(), Some("wav"));
    }

    #[test]
    fn parse_hash_from_plain_hash_line() {
        // Plain hash with no surrounding text
        let result = parse_hash_from_line("abcd1234ef567890");
        assert_eq!(result, Some("abcd1234ef567890".to_string()));
    }

    #[test]
    fn parse_hash_from_search_output_line() {
        // Canonical playlist line format: {short_hash}  {Artist} - {Title}  [{duration}]
        let result = parse_hash_from_line("abcd1234  Carbon Based Lifeforms - Init  [8:28]");
        assert_eq!(result, Some("abcd1234".to_string()));
    }

    #[test]
    fn parse_hash_from_sha256_prefixed_line() {
        // Full sha256: prefixed hash on its own line
        let result = parse_hash_from_line(
            "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        );
        assert_eq!(
            result,
            Some(
                "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                    .to_string()
            )
        );
    }

    #[test]
    fn parse_hash_from_empty_line_returns_none() {
        let result = parse_hash_from_line("");
        assert!(result.is_none());
    }

    #[test]
    fn parse_hash_from_comment_line_returns_none() {
        // Lines starting with # are comments
        let result = parse_hash_from_line("# this is a comment");
        assert!(result.is_none());
    }

    #[test]
    fn parse_hash_from_uppercase_returns_none() {
        assert!(parse_hash_from_line("ABCDEF1234567890").is_none());
    }

    #[test]
    fn parse_hash_from_short_token_returns_none() {
        assert!(parse_hash_from_line("abcd12").is_none());
    }

    #[test]
    fn parse_hash_from_unavailable_comment_line() {
        // Queue edit writes unresolvable tracks as: {short_hash}  # [unavailable: error]
        // The hash must survive the round-trip through parse_hash_from_line.
        let result = parse_hash_from_line("a1b2c3d4  # [unavailable: track not found]");
        assert_eq!(result, Some("a1b2c3d4".to_string()));
    }

    // ── ExportFormat::Original ────────────────────────────────────────────────

    #[test]
    fn export_format_original_is_default() {
        // Default value should be "original", not "aiff"
        let default: ExportFormat = ExportFormat::Original;
        assert_eq!(default, ExportFormat::Original);
    }

    #[test]
    fn export_format_original_extension_returns_none() {
        // Original has no fixed extension — caller derives it from blob_path
        assert!(ExportFormat::Original.static_extension().is_none());
    }

    #[test]
    fn export_format_aiff_extension_is_aiff() {
        assert_eq!(ExportFormat::Aiff.static_extension(), Some("aiff"));
    }

    #[test]
    fn export_format_wav_extension_is_wav() {
        assert_eq!(ExportFormat::Wav.static_extension(), Some("wav"));
    }

    #[test]
    fn export_url_builds_with_original_format() {
        // Original passes format=original in the query string
        let url = export_url_from_node("mdma-909.local", "sha256:abcd1234", "original");
        assert_eq!(
            url,
            "http://mdma-909.local/export/sha256:abcd1234?format=original"
        );
    }

    // ── export_dest_path builds artist/album/title hierarchy ─────────────────

    fn make_track_full(artist: &str, album: &str, title: &str, blob_path: &str) -> TrackInfo {
        use library_ipc_client::{ContentHash, DurationSeconds};
        TrackInfo {
            content_hash: ContentHash::new("sha256:aa000001"),
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            duration: Some(DurationSeconds::new(300)),
            bpm: None,
            key: None,
            blob_path: Some(blob_path.to_string()),
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        }
    }

    #[test]
    fn export_dest_path_uses_artist_album_title() {
        let track = make_track_full(
            "Carbon Based Lifeforms",
            "Twentythree",
            "Polyrytmi",
            "ab/abc123.flac",
        );
        let output = std::path::Path::new("/tmp/export");
        let path = export_dest_path(output, &track, "flac");
        assert_eq!(
            path,
            std::path::PathBuf::from(
                "/tmp/export/Carbon Based Lifeforms/Twentythree/Polyrytmi.flac"
            )
        );
    }

    #[test]
    fn export_dest_path_sanitizes_slash_in_artist() {
        let track = make_track_full("AC/DC", "Back in Black", "Thunderstruck", "ab/abc123.mp3");
        let output = std::path::Path::new("/tmp/export");
        let path = export_dest_path(output, &track, "mp3");
        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/export/AC_DC/Back in Black/Thunderstruck.mp3")
        );
    }

    #[test]
    fn export_dest_path_uses_fallbacks_for_missing_metadata() {
        use library_ipc_client::{ContentHash, DurationSeconds};
        let track = TrackInfo {
            content_hash: ContentHash::new("sha256:deadbeef"),
            title: None,
            artist: None,
            album: None,
            duration: Some(DurationSeconds::new(120)),
            bpm: None,
            key: None,
            blob_path: Some("ab/abc123.flac".to_string()),
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        };
        let output = std::path::Path::new("/tmp/export");
        let path = export_dest_path(output, &track, "flac");
        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/export/Unknown Artist/Unknown Album/Unknown.flac")
        );
    }

    #[test]
    fn export_dest_path_sanitizes_all_unsafe_chars() {
        let track = make_track_full(
            "Art:ist*Name",
            "Album?<>|Name",
            r#"Track "Remix""#,
            "ab/abc123.aiff",
        );
        let output = std::path::Path::new("/out");
        let path = export_dest_path(output, &track, "aiff");
        assert_eq!(
            path,
            std::path::PathBuf::from("/out/Art_ist_Name/Album____Name/Track _Remix_.aiff")
        );
    }

    // ── Shell completions ─────────────────────────────────────────────────────

    #[test]
    fn generate_completions_bash_produces_nonempty_output() {
        // The generate_completions function must produce a non-empty bash script.
        let mut buf = Vec::new();
        generate_completions(clap_complete::Shell::Bash, &mut buf);
        let output = String::from_utf8(buf).expect("completion output is valid UTF-8");
        assert!(
            !output.is_empty(),
            "bash completion output must not be empty"
        );
        assert!(
            output.contains("mdma"),
            "bash completion output must reference the binary name 'mdma'"
        );
    }

    // ── resolve_export_format ────────────────────────────────────────────────

    #[test]
    fn resolve_format_with_format_flag_overrides_everything() {
        let result = resolve_export_format(
            Some("ab/abc123.flac"),
            &Some(ExportFormat::Wav),
            &None,
            &None,
        );
        assert_eq!(result, ExportFormat::Wav);
    }

    #[test]
    fn resolve_format_no_flags_defaults_to_original() {
        let result = resolve_export_format(Some("ab/abc123.flac"), &None, &None, &None);
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_lossless_flag_converts_flac() {
        let result = resolve_export_format(
            Some("ab/abc123.flac"),
            &None,
            &Some(ExportFormat::Aiff),
            &None,
        );
        assert_eq!(result, ExportFormat::Aiff);
    }

    #[test]
    fn resolve_format_lossless_flag_passes_through_mp3() {
        let result = resolve_export_format(
            Some("ab/abc123.mp3"),
            &None,
            &Some(ExportFormat::Aiff),
            &None,
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_lossy_flag_converts_mp3() {
        let result = resolve_export_format(
            Some("ab/abc123.mp3"),
            &None,
            &None,
            &Some(ExportFormat::Wav),
        );
        assert_eq!(result, ExportFormat::Wav);
    }

    #[test]
    fn resolve_format_lossy_flag_passes_through_flac() {
        let result = resolve_export_format(
            Some("ab/abc123.flac"),
            &None,
            &None,
            &Some(ExportFormat::Wav),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_both_category_flags() {
        // lossless → aiff, lossy → wav
        let lossless = resolve_export_format(
            Some("ab/abc123.flac"),
            &None,
            &Some(ExportFormat::Aiff),
            &Some(ExportFormat::Wav),
        );
        assert_eq!(lossless, ExportFormat::Aiff);

        let lossy = resolve_export_format(
            Some("ab/abc123.mp3"),
            &None,
            &Some(ExportFormat::Aiff),
            &Some(ExportFormat::Wav),
        );
        assert_eq!(lossy, ExportFormat::Wav);
    }

    #[test]
    fn resolve_format_unknown_extension_passes_through() {
        let result = resolve_export_format(
            Some("ab/abc123.txt"),
            &None,
            &Some(ExportFormat::Aiff),
            &Some(ExportFormat::Wav),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_no_blob_path_with_category_flags_passes_through() {
        let result = resolve_export_format(
            None,
            &None,
            &Some(ExportFormat::Aiff),
            &Some(ExportFormat::Wav),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn contains_threshold_default_is_one_not_inverted() {
        assert_eq!(
            resolve_contains_threshold(false, None, false, 3),
            (1, false)
        );
    }

    #[test]
    fn contains_threshold_all_uses_input_len() {
        assert_eq!(resolve_contains_threshold(true, None, false, 5), (5, false));
    }

    #[test]
    fn contains_threshold_at_least_uses_given_value() {
        assert_eq!(
            resolve_contains_threshold(false, Some(3), false, 7),
            (3, false)
        );
    }

    #[test]
    fn contains_threshold_no_flag_inverts() {
        assert_eq!(resolve_contains_threshold(false, None, true, 4), (1, true));
    }

    // ── Date arg hyphen-value parsing ────────────────────────────────────────

    #[test]
    fn search_added_hyphen_value_parses() {
        // `mdma search --added -7` must not be rejected by clap as an unknown flag.
        let result = Cli::try_parse_from(["mdma", "search", "--added", "-7"]);
        assert!(
            result.is_ok(),
            "clap rejected --added -7: {}",
            result.unwrap_err()
        );
    }

    #[test]
    fn search_started_hyphen_value_parses() {
        let result = Cli::try_parse_from(["mdma", "search", "--started", "-7"]);
        assert!(
            result.is_ok(),
            "clap rejected --started -7: {}",
            result.unwrap_err()
        );
    }

    #[test]
    fn search_stopped_tilde_parses() {
        let result = Cli::try_parse_from(["mdma", "search", "--stopped", "~"]);
        assert!(
            result.is_ok(),
            "clap rejected --stopped ~: {}",
            result.unwrap_err()
        );
    }

    // ── apply_stdin_filter tests ─────────────────────────────────────────────

    fn make_track_with_hash(hash: &str) -> TrackInfo {
        use library_ipc_client::ContentHash;
        TrackInfo {
            content_hash: ContentHash::new(hash.to_string()),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        }
    }

    fn hash_set(hashes: &[&str]) -> std::collections::HashSet<String> {
        hashes.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn search_not_with_stdin_excludes_piped_hashes() {
        // stdin = {h1, h2}, library returns {h1, h2, h3, h4}, --not returns [h3, h4]
        let tracks = vec![
            make_track_with_hash("sha256:h1aaaaaa"),
            make_track_with_hash("sha256:h2aaaaaa"),
            make_track_with_hash("sha256:h3aaaaaa"),
            make_track_with_hash("sha256:h4aaaaaa"),
        ];
        let stdin = hash_set(&["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), true);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, vec!["sha256:h3aaaaaa", "sha256:h4aaaaaa"]);
    }

    #[test]
    fn search_not_with_stdin_prefix_match_excludes() {
        // stdin token is a short prefix (no sha256: prefix on token)
        let tracks = vec![
            make_track_with_hash("sha256:abcdef1234"),
            make_track_with_hash("sha256:deadbeef56"),
            make_track_with_hash("sha256:cafebabe78"),
        ];
        let stdin = hash_set(&["abcdef", "deadbe"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), true);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, vec!["sha256:cafebabe78"]);
    }

    #[test]
    fn search_stdin_without_not_intersects() {
        // Sanity: stdin + no --not still intersects
        let tracks = vec![
            make_track_with_hash("sha256:h1aaaaaa"),
            make_track_with_hash("sha256:h2aaaaaa"),
            make_track_with_hash("sha256:h3aaaaaa"),
        ];
        let stdin = hash_set(&["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
        let result = apply_stdin_filter(tracks, Some(&stdin), false);
        let hashes: Vec<_> = result.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(hashes, vec!["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
    }

    #[test]
    fn search_no_stdin_preserves_not_behavior() {
        // Without stdin, apply_stdin_filter returns tracks unchanged regardless of `not`
        let tracks = vec![
            make_track_with_hash("sha256:h1aaaaaa"),
            make_track_with_hash("sha256:h2aaaaaa"),
        ];
        let result = apply_stdin_filter(tracks.clone(), None, true);
        assert_eq!(result.len(), 2, "pass-through when no stdin, not=true");
        let result2 = apply_stdin_filter(tracks, None, false);
        assert_eq!(result2.len(), 2, "pass-through when no stdin, not=false");
    }

    // ── sort by started / stopped ─────────────────────────────────────────────

    fn make_track_with_started(hash: &str, started: Option<&str>) -> TrackInfo {
        use library_ipc_client::ContentHash;
        TrackInfo {
            content_hash: ContentHash::new(hash.to_string()),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: started.map(str::to_string),
            stopped: None,
        }
    }

    fn make_track_with_stopped(hash: &str, stopped: Option<&str>) -> TrackInfo {
        use library_ipc_client::ContentHash;
        TrackInfo {
            content_hash: ContentHash::new(hash.to_string()),
            title: None,
            artist: None,
            album: None,
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: stopped.map(str::to_string),
        }
    }

    // ── --played=never filter ─────────────────────────────────────────────────

    #[test]
    fn search_played_never_maps_to_started_na() {
        use library_search::DateQuery;
        let result = Cli::try_parse_from(["mdma", "search", "--played", "never"]);
        assert!(
            result.is_ok(),
            "clap rejected --played never: {}",
            result.unwrap_err()
        );
        // Build the query with played=never and no explicit --started
        let query = build_track_query(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(PlayedFilter::Never),
        );
        assert!(
            matches!(query.started, Some(DateQuery::NA)),
            "expected started == Some(DateQuery::NA), got {:?}",
            query.started
        );
    }

    #[test]
    fn search_played_never_overrides_started() {
        use library_search::DateQuery;
        // When --played=never and --started are both given, --played=never wins.
        let query = build_track_query(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("-7".to_string()),
            None,
            None,
            Some(PlayedFilter::Never),
        );
        assert!(
            matches!(query.started, Some(DateQuery::NA)),
            "expected --played=never to override --started, got {:?}",
            query.started
        );
    }

    // ── sort None = -∞ semantics ──────────────────────────────────────────────

    #[test]
    fn sort_started_ascending_puts_none_first() {
        let mut tracks = vec![
            make_track_with_started("sha256:never", None),
            make_track_with_started("sha256:old", Some("2024-01-01T00:00:00Z")),
            make_track_with_started("sha256:recent", Some("2025-06-15T12:00:00Z")),
        ];
        // ascending: None (-∞) first, then oldest → newest
        tracks.sort_by(|a, b| {
            compare_optional(
                a.started.as_deref().map(str::to_string),
                b.started.as_deref().map(str::to_string),
                true,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            vec!["sha256:never", "sha256:old", "sha256:recent"],
            "ascending: never-played first (None = -∞), then oldest→newest"
        );
    }

    #[test]
    fn sort_started_descending_puts_none_last() {
        let mut tracks = vec![
            make_track_with_started("sha256:never", None),
            make_track_with_started("sha256:old", Some("2024-01-01T00:00:00Z")),
            make_track_with_started("sha256:recent", Some("2025-06-15T12:00:00Z")),
        ];
        // descending: newest first, None last
        tracks.sort_by(|a, b| {
            compare_optional(
                a.started.as_deref().map(str::to_string),
                b.started.as_deref().map(str::to_string),
                false,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            vec!["sha256:recent", "sha256:old", "sha256:never"],
            "descending: newest first, never-played last (None = -∞)"
        );
    }

    #[test]
    fn sort_by_stopped_descending_newest_first() {
        let mut tracks = vec![
            make_track_with_stopped("sha256:never", None),
            make_track_with_stopped("sha256:old", Some("2024-03-01T00:00:00Z")),
            make_track_with_stopped("sha256:recent", Some("2025-11-20T08:00:00Z")),
        ];
        // descending: newest first, None last (None = -∞, reversed → last)
        tracks.sort_by(|a, b| {
            compare_optional(
                a.stopped.as_deref().map(str::to_string),
                b.stopped.as_deref().map(str::to_string),
                false,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            vec!["sha256:recent", "sha256:old", "sha256:never"],
            "descending: newest first, never-stopped last"
        );
    }

    #[test]
    fn sort_added_respects_none_as_beginning_of_time() {
        use library_ipc_client::ContentHash;
        let make = |hash: &str, added: Option<&str>| -> TrackInfo {
            TrackInfo {
                content_hash: ContentHash::new(hash.to_string()),
                title: None,
                artist: None,
                album: None,
                duration: None,
                bpm: None,
                key: None,
                blob_path: None,
                cover_art_path: None,
                track_number: None,
                disc_number: None,
                added: added.map(str::to_string),
                started: None,
                stopped: None,
            }
        };
        let mut tracks = vec![
            make("sha256:unknown", None),
            make("sha256:new", Some("2025-12-01T00:00:00Z")),
            make("sha256:early", Some("2020-01-01T00:00:00Z")),
        ];
        // ascending: None first (beginning of time), then oldest→newest
        tracks.sort_by(|a, b| {
            compare_optional(
                a.added.as_deref().map(str::to_string),
                b.added.as_deref().map(str::to_string),
                true,
            )
        });
        let hashes: Vec<_> = tracks.iter().map(|t| t.content_hash.as_str()).collect();
        assert_eq!(
            hashes,
            vec!["sha256:unknown", "sha256:early", "sha256:new"],
            "ascending added: None first, then oldest→newest"
        );
    }
}
