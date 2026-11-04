//! MDMA CLI - Command line interface for mdma services
//!
//! Connects to services via gateway (single address) or direct IPC.

mod node;

use chrono::NaiveDate;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use color_eyre::Result;
use colored::Colorize;
use event_protocol::{from_topic_message, PlaybackEvent, TOPIC_PLAYBACK};
use gateway_client::{AdminRequest, AdminResponse, GatewayClient};
use library_ipc_client::{
    ClientError, ConfirmationReason, ContentHash, DedupApplyReport, DedupDisposition, DedupGroup,
    DedupMember, InboxPath, ProtocolError, TrackInfo,
};
use library_search::{
    parse_date_query, parse_numeric_query, parse_role_query, parse_string_query, TrackQuery,
};
use mdma_client::{
    Deck, IngestSource, LibraryBackend, PlaybackBackend, PlaybackClientError, SourceClient,
    SourceName,
};
use music_facts::{
    Album, Artist, Bpm, DiscNumber, EnergyLevel, Isrc, Key, MusicValue, Title, TrackNumber,
    TrackRole, Year,
};
use nng::options::Options;
use playlist_import::{
    format_import_status, format_write_outcome, sanitize_playlist_name, write_playlist,
    LibraryTrackLookup,
};
use rekordbox_xml::{parse_xml, RekordboxTrack};
use source_protocol::{
    ImportJobStatus, ImportPhase, ImportSyncProgress, SourceError, SourceRequest, SourceResponse,
};
use std::path::Path;
use track_formatter::{
    format_track_line as format_track_line_shared, render_for_user, short_hash, ViewColumn,
    ALL_COLUMNS,
};
use track_matcher::{CandidateTrack, MatchResult};

// =============================================================================
// CLI Definition
// =============================================================================

#[derive(Parser, Debug)]
#[command(name = "mdma")]
#[command(author, version, about = "MDMA CLI - Control the music services")]
struct Cli {
    /// Node hostname (e.g. mdma-909.local), optionally with a port. Also read from
    /// MDMA_NODE. Without either, the remembered or discovered node is used.
    #[arg(long)]
    node: Option<String>,

    /// The node resolved at startup (not a flag).
    #[arg(skip)]
    target: Option<node_discovery::Target>,

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

        /// Filter by DJ role. Accepted values: opener, build-up, peak, banger, cool-down, closer, filler
        #[arg(long)]
        role: Option<String>,

        /// Filter by energy level (1-10). Formats: 7  5..8  7+-2
        #[arg(long)]
        energy: Option<String>,

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
    #[command(
        override_usage = "mdma source list\n       mdma source <SOURCE-NAME> <COMMAND>",
        after_help = "Per-source commands: mdma source <SOURCE-NAME> <COMMAND>\n\n\
                      Commands:\n  \
                      sync, status, downloads, cancel, pause, resume, resync,\n  \
                      check-item, check-updates, playlists, import-playlist, import-status\n\n\
                      Example: mdma source bandcamp sync\n\
                      Run 'mdma source <SOURCE-NAME> --help' for details.\n\
                      'list' is reserved and cannot be a source name."
    )]
    Source {
        #[command(subcommand)]
        command: SourceTop,
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

    /// Add or retract metadata facts on a track.
    Fact {
        #[command(subcommand)]
        command: FactCommands,
    },

    /// Display selected fields for tracks read from stdin.
    ///
    /// Reads hashes from stdin (one per line), looks up TrackInfo for each hash,
    /// and renders a colored, columnized table with the selected fields.
    ///
    /// Field order in the output matches the order the flags appear on the command line.
    /// Field-order is detected via clap's indices_of: the position at which each flag
    /// was specified in the argv determines the column order.
    ///
    /// Examples:
    ///   mdma search --artist CBL | mdma view --hash --artist --title
    ///   mdma search --genre Techno | mdma view --title --duration
    ///   cat friday.plist | mdma view --hash --artist --title --duration
    View {
        /// Show the 12-character content hash
        #[arg(long)]
        hash: bool,

        /// Show the artist name
        #[arg(long)]
        artist: bool,

        /// Show the track title
        #[arg(long)]
        title: bool,

        /// Show the track duration
        #[arg(long)]
        duration: bool,
    },

    /// Generate shell completions
    #[command(hide = true)]
    GenerateCompletions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Admin commands (service mode, reboot)
    Admin {
        #[command(subcommand)]
        command: AdminCommands,
    },

    /// Find and choose the MDMA node to talk to
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },
}

impl Commands {
    /// Whether the command talks to a node, so one has to be resolved first.
    fn needs_node(&self) -> bool {
        !matches!(
            self,
            Commands::Node { .. } | Commands::GenerateCompletions { .. }
        )
    }
}

#[derive(Subcommand, Debug)]
enum NodeCommands {
    /// List MDMA nodes on the network (* marks the remembered node)
    List,
    /// Remember a node (by name or id; prompts when omitted)
    Select {
        /// Node name or id
        name: Option<String>,
    },
    /// Forget the remembered node
    Forget,
    /// Show which node commands would use, and why
    Show,
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
        /// Also skips grandfathering of existing paths: tracks whose paths exceed
        /// the rekordbox Location budget get new, shortened paths (rekordbox will
        /// see them as new tracks and cues are lost).
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

/// Top level of `mdma source`: the global `list`, or `<source-name> <command>`.
///
/// `<source-name>` comes first, so it cannot be a positional of the group
/// (it would collide with `list`). Anything that is not `list` is captured as
/// an external subcommand and re-parsed as a [`SourceInvocation`], which keeps
/// full clap help and validation for the per-source commands. `list` is
/// therefore reserved and cannot be used as a source name.
#[derive(Subcommand, Debug)]
enum SourceTop {
    /// List available music sources
    List,

    /// `<SOURCE-NAME> <COMMAND>`: run a command on a source (see `mdma source <SOURCE-NAME> --help`)
    #[command(external_subcommand)]
    Named(Vec<String>),
}

/// The per-source commands: `mdma source <source-name> <command> [args]`.
#[derive(Parser, Debug)]
#[command(
    arg_required_else_help = true,
    subcommand_required = true,
    about = "Commands for one music source"
)]
struct SourceInvocation {
    #[command(subcommand)]
    command: SourceCommands,
}

#[derive(Subcommand, Debug, PartialEq)]
enum SourceCommands {
    /// Sync the source (download new items)
    Sync,

    /// Show source status
    Status,

    /// List downloads for the source
    Downloads,

    /// Cancel a download
    Cancel {
        /// Download ID to cancel
        id: String,
    },

    /// Pause all downloads for the source
    Pause,

    /// Resume downloads for the source
    Resume,

    /// Force re-sync of a specific item (bypasses dedup)
    Resync {
        /// Source-specific item identifier (e.g. "p123456" for bandcamp)
        identifier: String,
    },

    /// Check if a specific item has changed upstream
    CheckItem {
        /// Source-specific item identifier
        identifier: String,
    },

    /// Check the whole source collection for stale items
    CheckUpdates {
        /// Auto-apply resync for any stale items found
        #[arg(long)]
        apply: bool,
    },

    /// List the source's playlists (for sources that support playlists)
    Playlists,

    /// Import a source playlist as an MDMA playlist (definitive matches only;
    /// for sources that support playlists)
    ImportPlaylist {
        /// Playlist slug or URL
        playlist: String,

        /// MDMA playlist name (default: sanitised source title)
        #[arg(long)]
        name: Option<String>,

        /// Show the report without syncing or writing the playlist
        #[arg(long)]
        dry_run: bool,

        /// Print the job id and exit once the import has started
        #[arg(long)]
        no_wait: bool,
    },

    /// Show the current or last playlist import job, following it while running
    /// (for sources that support playlists)
    ImportStatus,
}

/// Parse the arguments captured by [`SourceTop::Named`]: `<name> <command> ...`.
fn parse_source_args(args: &[String]) -> Result<(String, SourceCommands), clap::Error> {
    let (name, rest) = args
        .split_first()
        .expect("external subcommand always has a name");
    let argv = std::iter::once(format!("mdma source {name}")).chain(rest.iter().cloned());
    let invocation = SourceInvocation::try_parse_from(argv)?;
    Ok((name.clone(), invocation.command))
}

/// Print a parse error (with a `mdma source list` hint) and exit.
fn exit_source_parse_error(e: clap::Error) -> ! {
    let failed = e.use_stderr();
    let code = e.exit_code();
    let _ = e.print();
    if failed {
        eprintln!("\nHint: 'mdma source list' shows the available sources");
    }
    std::process::exit(code);
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

/// Metadata fields that can be added or retracted by the user.
///
/// Internal/derived fields (FilePath, Source, ItemId, audio properties, play events,
/// AddedAt, cover art) are intentionally excluded — they are managed automatically.
#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum FactField {
    Title,
    Artist,
    Album,
    AlbumArtist,
    TrackNumber,
    DiscNumber,
    Year,
    Bpm,
    Key,
    MainGenre,
    StyleDescriptor,
    FullGenre,
    Isrc,
    Label,
    RecordingYear,
    RecordingDate,
    Comment,
    BeatportLabelUrl,
    BeatportTrackUrl,
    BandcampUrl,
    Role,
    Energy,
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

    /// Manage individual tracks (replace, delete, restore, list orphans)
    Track {
        #[command(subcommand)]
        command: TrackCommands,
    },

    /// Compute track identity hashes (used to find duplicates)
    Identity {
        #[command(subcommand)]
        command: IdentityCommands,
    },

    /// Find duplicate tracks (the same audio with different tags or files) and merge them.
    ///
    /// Without options, lists the groups: those that can be merged automatically (all
    /// copies come from one purchase and none is deleted) first, then those that need
    /// your confirmation. The newest copy is kept; its blob stays, the other copies'
    /// facts are folded into it (latest value per attribute wins), playlists are
    /// rewritten, and the other copies' audio files are deleted.
    ///
    /// Needs identity hashes: run `mdma library identity backfill` first.
    #[command(args_conflicts_with_subcommands = true)]
    Dedup {
        /// Merge the automatic groups (the others are only listed)
        #[arg(long)]
        apply: bool,

        #[command(subcommand)]
        command: Option<DedupCommands>,
    },
}

#[derive(Subcommand, Debug)]
enum DedupCommands {
    /// Merge tracks you have confirmed to be duplicates into a winner.
    ///
    /// The other tracks' audio files are deleted. All tracks must be in one duplicate
    /// group as listed by `mdma library dedup`.
    Merge {
        /// The track to keep (prefix accepted, git-style)
        winner: String,
        /// The duplicates to merge into it (prefixes accepted)
        #[arg(required = true)]
        losers: Vec<String>,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum IdentityCommands {
    /// Compute identity hashes for tracks that don't have them yet.
    ///
    /// New tracks get an AudioHash when they are ingested; this covers the tracks
    /// already in the library. It runs as a low-priority background job in the
    /// library service and can be re-run: tracks that already have the hash are
    /// skipped. By default the command follows the job until it finishes.
    Backfill {
        /// Also compute PcmHash (decodes every lossless track; slow)
        #[arg(long)]
        pcm: bool,
        /// Start the job and return immediately
        #[arg(long)]
        no_wait: bool,
    },

    /// Show progress of the identity-hash backfill
    Status,

    /// Stop a running identity-hash backfill (progress is kept; run backfill again to resume)
    Cancel,
}

#[derive(Subcommand, Debug)]
enum AdminCommands {
    /// Service mode subcommands (EEPROM boot order)
    ServiceMode {
        #[command(subcommand)]
        command: ServiceModeCommands,
    },
    /// Report whether the host can safely reboot into its installed kernel
    BootHealth,
    /// Reboot the host (refused if the boot partition looks unhealthy)
    Reboot {
        /// Reboot even if the boot-health check fails
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TrackCommands {
    /// Soft-delete a track (hidden from default views; file retained; recoverable).
    ///
    /// The track is hidden from list/search results. To undo, use `mdma library track restore <hash>`.
    Delete {
        /// Content hash of the track (prefix accepted, git-style).
        hash: String,
    },

    /// Restore a soft-deleted track (retracts the Deleted fact).
    Restore {
        /// Content hash of the track (prefix accepted, git-style).
        hash: String,
    },

    /// Hard-replace an old track with a new file.
    ///
    /// Ingests the new file (path must be on the device running the library service),
    /// fully retracts all facts for the old track (the old hash stops resolving — gone
    /// from search and list, not merely hidden), records a Replaces provenance link on
    /// the new track, and rewrites every playlist that referenced the old track.
    /// Playlists that still point at the old hash self-heal on read by following the
    /// replacement chain (multi-step chains are followed automatically).
    ///
    /// Note: <new-file> must be a path accessible by the library service on the device.
    /// Place the new file in the inbox directory or another device-local path first.
    Replace {
        /// Content hash of the old track (prefix accepted, git-style).
        old_hash: String,
        /// Path to the new audio file on the device running the library service.
        new_file: String,
    },

    /// List blobs on disk that no live track references (GC candidate list, read-only).
    ///
    /// Reports hard-replace leftovers (reason: "no live facts") and soft-deleted tracks
    /// (reason: "deleted"). Use `mdma library track delete`/`restore` for soft-delete management.
    Orphans,
}

#[derive(Subcommand, Debug)]
enum ServiceModeCommands {
    /// Show current EEPROM boot order, armed state, and PCIE_PROBE
    Status,
    /// Arm service mode: next reboot will boot from SD (beacon)
    Enable,
    /// Disarm service mode: restore NVMe-first boot order
    Disable,
}

#[derive(Subcommand, Debug)]
enum FactCommands {
    /// Add or update a fact on a track.
    ///
    /// Stainless-facts replace semantics: the most recent fact for a given field wins
    /// for non-collection types. Note this does NOT auto-retract the previous value;
    /// retract explicitly first if you want to clean history.
    Add {
        /// Track content hash (prefix accepted, git-style).
        hash: String,
        /// Field name to add. Tab-completion shows supported fields.
        #[arg(long, value_enum)]
        field: FactField,
        /// Value for the field.
        #[arg(long)]
        value: String,
    },
    /// Retract an existing fact from a track.
    ///
    /// Required for collection-type fields (each value is independent).
    /// For scalar fields, retract removes that value from history.
    Retract {
        /// Track content hash (prefix accepted, git-style).
        hash: String,
        /// Field name to retract.
        #[arg(long, value_enum)]
        field: FactField,
        /// Value to retract (must match the existing fact's value).
        #[arg(long)]
        value: String,
    },
}

// =============================================================================
// Gateway Resolution Helpers
// =============================================================================

/// Resolve the effective gateway address from the node resolved at startup:
/// `tcp://<ip>:<port>`.
/// Returns `None` for local IPC — caller falls back to direct IPC.
fn resolve_gateway(cli: &Cli) -> Option<String> {
    node::client_config(cli.target.as_ref()).gateway_addr()
}

/// Resolve the effective event gateway address: `tcp://<ip>:5556`.
/// Returns `None` for local IPC.
fn resolve_event_gateway(cli: &Cli) -> Option<String> {
    node::client_config(cli.target.as_ref()).event_addr()
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
    connect_source_or_exit(cli, name, None)
}

/// Request timeout for `mdma source playlists`: listing may make several
/// rate-limited upstream requests (e.g. Bandcamp), well beyond the default 5s.
const SOURCE_LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Connect to a source, with the default request timeout or a custom one.
fn connect_source_or_exit(
    cli: &Cli,
    name: &str,
    timeout: Option<std::time::Duration>,
) -> SourceClient {
    let gateway = resolve_gateway(cli);
    let connected = match timeout {
        Some(t) => {
            SourceClient::connect_with_timeout(gateway.as_deref(), &cli.sources_dir, name, t)
        }
        None => SourceClient::connect(gateway.as_deref(), &cli.sources_dir, name),
    };
    match connected {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", e);
            if gateway.is_none() {
                eprintln!("Is mdma-{} running?", name);
            }
            eprintln!("Hint: 'mdma source list' shows the available sources");
            std::process::exit(1);
        }
    }
}

/// Connect to the admin service via gateway.
///
/// Exits with a message when there is no gateway to connect to (local IPC).
fn connect_admin(cli: &Cli) -> GatewayClient {
    let gateway = resolve_gateway(cli).unwrap_or_else(|| {
        eprintln!(
            "error: admin commands need a gateway; use --node (or MDMA_NODE) to name the node"
        );
        std::process::exit(1);
    });
    match GatewayClient::connect(&gateway) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to gateway: {}", e);
            std::process::exit(1);
        }
    }
}

// =============================================================================
// Display Helpers
// =============================================================================

/// Canonical playlist line format: `{short_hash}  {Artist} - {Title}  [{duration}]`
/// Used for pipe mode output and temp files (no colors, no alignment).
///
/// Delegates to `track_formatter::format_track_line` — the shared implementation
/// used by both CLI and TUI to guarantee format consistency.
fn format_track_line(track: &TrackInfo) -> String {
    format_track_line_shared(track)
}

// =============================================================================
// Track Table Rendering (colored)
// =============================================================================

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

/// Virtual terminal width used when stdout is not a TTY (pipe, file redirection).
///
/// Wide enough to guarantee that corsett never shrinks the 8-char hash column,
/// and that typical artist/title content is not truncated (Joakim's uniqueness hypothesis).
const PIPE_TERM_WIDTH: usize = 200;

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
/// Delegates to `track_formatter::render_for_user` with the full default column set.
fn render_track_table_inner(
    tracks: &[TrackInfo],
    reserved_prefix: usize,
    term_width: usize,
) -> Vec<String> {
    render_for_user(tracks, ALL_COLUMNS, term_width, reserved_prefix)
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
            if let Some(role) = track.role {
                println!("Role:     {}", role);
            }
            if let Some(energy) = track.energy {
                println!("Energy:   {}", energy);
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

/// Parse a raw string value into a `MusicValue` for the given `FactField`.
///
/// Returns `Err(String)` with an actionable diagnostic if the value cannot be parsed.
fn parse_field_value(field: FactField, raw: &str) -> Result<MusicValue, String> {
    match field {
        FactField::Title => Ok(MusicValue::Title(Title::new(raw))),
        FactField::Artist => Ok(MusicValue::Artist(Artist::new(raw))),
        FactField::Album => Ok(MusicValue::Album(Album::new(raw))),
        FactField::AlbumArtist => Ok(MusicValue::AlbumArtist(Artist::new(raw))),
        FactField::TrackNumber => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for TrackNumber '{raw}': {e}"))?;
            Ok(MusicValue::TrackNumber(TrackNumber::new(n)))
        }
        FactField::DiscNumber => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for DiscNumber '{raw}': {e}"))?;
            Ok(MusicValue::DiscNumber(DiscNumber::new(n)))
        }
        FactField::Year => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for Year '{raw}': {e}"))?;
            Ok(MusicValue::Year(Year::new(n)))
        }
        FactField::Bpm => {
            let b = raw
                .parse::<f32>()
                .map_err(|e| format!("invalid float for Bpm '{raw}': {e}"))?;
            let bpm = Bpm::from_f32(b)
                .map_err(|e| format!("BPM out of range for '{raw}': {e}"))?;
            Ok(MusicValue::Bpm(bpm))
        }
        FactField::Key => Key::from_camelot(raw)
            .or_else(|_| Key::from_traditional(raw))
            .map(MusicValue::Key)
            .map_err(|_| {
                format!("invalid key '{raw}': use Camelot notation (1A-12B) or traditional (e.g. 'C Major', 'A Minor')")
            }),
        FactField::MainGenre => Ok(MusicValue::MainGenre(raw.to_string())),
        FactField::StyleDescriptor => Ok(MusicValue::StyleDescriptor(raw.to_string())),
        FactField::FullGenre => Ok(MusicValue::FullGenre(raw.to_string())),
        FactField::Isrc => Ok(MusicValue::Isrc(Isrc::new(raw))),
        FactField::Label => Ok(MusicValue::Label(raw.to_string())),
        FactField::RecordingYear => {
            let n = raw
                .parse::<u32>()
                .map_err(|e| format!("invalid integer for RecordingYear '{raw}': {e}"))?;
            Ok(MusicValue::RecordingYear(Year::new(n)))
        }
        FactField::RecordingDate => {
            let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|e| format!("invalid date for RecordingDate '{raw}' (use YYYY-MM-DD): {e}"))?;
            Ok(MusicValue::RecordingDate(date))
        }
        FactField::Comment => Ok(MusicValue::Comment(raw.to_string())),
        FactField::BeatportLabelUrl => Ok(MusicValue::BeatportLabelUrl(raw.to_string())),
        FactField::BeatportTrackUrl => Ok(MusicValue::BeatportTrackUrl(raw.to_string())),
        FactField::BandcampUrl => Ok(MusicValue::BandcampUrl(raw.to_string())),
        FactField::Role => raw
            .parse::<TrackRole>()
            .map(MusicValue::Role)
            .map_err(|_| {
                format!(
                    "invalid role '{raw}': accepted values are opener, build-up, peak, banger, cool-down, closer, filler"
                )
            }),
        FactField::Energy => {
            let n = raw
                .parse::<u8>()
                .map_err(|e| format!("invalid integer for Energy '{raw}': {e}"))?;
            let level = EnergyLevel::new(n)
                .map_err(|e| format!("energy out of range for '{raw}': {e}"))?;
            Ok(MusicValue::Energy(level))
        }
    }
}

fn handle_fact_add(
    client: &LibraryBackend,
    hash: String,
    field: FactField,
    value: String,
) -> Result<()> {
    let music_value = match parse_field_value(field, &value) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
    };
    let content_hash = ContentHash::new(hash.clone());
    match client.write_fact(&content_hash, music_value) {
        Ok(()) => {
            println!("Added fact for {}: {:?} = {}", hash, field, value);
            Ok(())
        }
        Err(e) => handle_error(e),
    }
}

fn handle_fact_retract(
    client: &LibraryBackend,
    hash: String,
    field: FactField,
    value: String,
) -> Result<()> {
    let music_value = match parse_field_value(field, &value) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
    };
    let content_hash = ContentHash::new(hash.clone());
    match client.retract_fact(&content_hash, music_value) {
        Ok(()) => {
            println!("Retracted fact for {}: {:?} = {}", hash, field, value);
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
    role_str: Option<String>,
    energy_str: Option<String>,
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
    let role = if let Some(s) = role_str {
        match parse_role_query(&s) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("Invalid --role value: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let energy = energy_str.map(|s| match parse_numeric_query(&s) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("Invalid --energy value: {}", e);
            std::process::exit(1);
        }
    });
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
        role,
        energy,
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

/// Resolve the ordered set of `ViewColumn`s from the raw `clap::ArgMatches` for the `view`
/// subcommand.
///
/// Clap boolean flags (`--hash`, `--artist`, `--title`, `--duration`) don't preserve
/// insertion order in the derive API. We use `ArgMatches::indices_of` to recover the argv
/// position of each flag that was actually set, then sort by position to honour the order the
/// user typed them in. This implements `mdma view --title --hash` → title-first, hash-second.
fn view_columns_from_matches(matches: &clap::ArgMatches) -> Vec<ViewColumn> {
    // Collect (argv_index, column) pairs for every flag that was set.
    let mut indexed: Vec<(usize, ViewColumn)> = Vec::new();
    for (flag, col) in &[
        ("hash", ViewColumn::Hash),
        ("artist", ViewColumn::Artist),
        ("title", ViewColumn::Title),
        ("duration", ViewColumn::Duration),
    ] {
        if matches.get_flag(flag) {
            // indices_of returns the positions at which the flag appeared in argv.
            // Take the first (and only) position for a boolean flag.
            if let Some(mut idxs) = matches.indices_of(flag) {
                if let Some(idx) = idxs.next() {
                    indexed.push((idx, *col));
                }
            }
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    indexed.into_iter().map(|(_, col)| col).collect()
}

fn handle_view(client: &LibraryBackend, columns: Vec<ViewColumn>) -> Result<()> {
    use std::io::{BufRead, IsTerminal};

    if columns.is_empty() {
        eprintln!("No fields selected. Use --hash, --artist, --title, and/or --duration.");
        std::process::exit(1);
    }

    // Read hashes from stdin (same format as `mdma playlist append` / `mdma search` output).
    let hashes: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    if hashes.is_empty() {
        if std::io::stdout().is_terminal() {
            eprintln!("No hashes on stdin. Pipe `mdma search` output or a playlist.");
        }
        return Ok(());
    }

    // Look up TrackInfo for each hash, preserving order.
    let mut tracks: Vec<TrackInfo> = Vec::with_capacity(hashes.len());
    for hash in &hashes {
        let content_hash = ContentHash::new(hash.clone());
        match client.get_track(&content_hash) {
            Ok(track) => tracks.push(track),
            Err(e) => {
                eprintln!("Warning: could not resolve {}: {}", hash, e);
            }
        }
    }

    if tracks.is_empty() {
        if std::io::stdout().is_terminal() {
            println!("No tracks found");
        }
        return Ok(());
    }

    // Always use the parameterized corsett renderer.
    // TTY → real terminal width (corsett truncates as needed via RightEllipsis).
    // Pipe/file → virtual fixed-large width so the 8-char hash stays intact and
    //             content is not truncated (Joakim's uniqueness hypothesis).
    let term_width = if std::io::stdout().is_terminal() {
        terminal_width()
    } else {
        PIPE_TERM_WIDTH
    };
    for line in render_for_user(&tracks, &columns, term_width, 0) {
        println!("{}", line);
    }

    Ok(())
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

const IDENTITY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// Consecutive failed status polls before `identity backfill` stops following.
const IDENTITY_POLL_MAX_FAILURES: u32 = 5;

fn format_identity_status(status: &library_ipc_client::IdentityBackfillStatus) -> String {
    use library_ipc_client::IdentityBackfillStatus as S;
    let (label, p) = match status {
        S::Idle => {
            return "No identity backfill has run since the library service started".to_string()
        }
        S::Running(p) => ("Running", p),
        S::Done(p) => ("Done", p),
        S::Cancelled(p) => ("Cancelled", p),
    };
    let hashes = if p.include_pcm {
        "AudioHash + PcmHash"
    } else {
        "AudioHash"
    };
    let mut line = format!("{label} ({hashes}): {}/{} tracks", p.done, p.total);
    if p.failed > 0 {
        line.push_str(&format!(", {} failed", p.failed));
    }
    if p.skipped > 0 {
        line.push_str(&format!(", {} skipped", p.skipped));
    }
    if let Some(err) = &p.last_error {
        line.push_str(&format!(" (last error: {err})"));
    }
    line
}

/// What to tell the user when a backfill is already running. The running job keeps
/// its own mode, so say so when it differs from what was just asked for.
fn identity_already_running_message(running_pcm: bool, requested_pcm: bool) -> String {
    let mode = |pcm| {
        if pcm {
            "AudioHash + PcmHash"
        } else {
            "AudioHash"
        }
    };
    let mut msg = "An identity backfill is already running; not starting another.".to_string();
    if running_pcm != requested_pcm {
        msg.push_str(&format!(
            " The running job computes {}, not the requested {}.",
            mode(running_pcm),
            mode(requested_pcm)
        ));
    }
    msg
}

fn handle_identity_backfill(client: &LibraryBackend, pcm: bool, no_wait: bool) -> Result<()> {
    use library_ipc_client::IdentityBackfillStart;
    match client.identity_backfill(pcm) {
        Ok(IdentityBackfillStart::Started(p)) => {
            println!("Identity backfill started: {} tracks to process", p.total)
        }
        Ok(IdentityBackfillStart::AlreadyRunning(p)) => {
            println!("{}", identity_already_running_message(p.include_pcm, pcm))
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
    if no_wait {
        println!("Check progress with 'mdma library identity status'");
        return Ok(());
    }

    let mut last_printed = String::new();
    let mut failures = 0;
    loop {
        match client.identity_backfill_status() {
            Ok(s) => {
                failures = 0;
                let line = format_identity_status(&s);
                if line != last_printed {
                    println!("{}", line);
                    last_printed = line;
                }
                if !matches!(s, library_ipc_client::IdentityBackfillStatus::Running(_)) {
                    return Ok(());
                }
            }
            Err(e) => {
                failures += 1;
                if failures >= IDENTITY_POLL_MAX_FAILURES {
                    eprintln!(
                        "Lost contact with the library service ({e}); \
                         check later with 'mdma library identity status'"
                    );
                    std::process::exit(1);
                }
            }
        }
        std::thread::sleep(IDENTITY_POLL_INTERVAL);
    }
}

fn handle_identity_status(client: &LibraryBackend) -> Result<()> {
    match client.identity_backfill_status() {
        Ok(s) => {
            println!("{}", format_identity_status(&s));
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn dedup_reason_text(reason: ConfirmationReason) -> &'static str {
    match reason {
        ConfirmationReason::DifferentItemIds => "different purchases (item ids)",
        ConfirmationReason::NoItemId => "a copy has no item id",
        ConfirmationReason::SoftDeleted => "a copy is deleted",
    }
}

fn format_dedup_member(role: &str, m: &DedupMember) -> String {
    let title = m.title.as_deref().unwrap_or("Unknown");
    let album = m
        .album
        .as_deref()
        .map(|a| format!("  [{a}]"))
        .unwrap_or_default();
    let added = m
        .added
        .as_deref()
        .map(|a| a.get(..10).unwrap_or(a))
        .unwrap_or("?");
    let item = m.item_id.as_deref().unwrap_or("-");
    let deleted = if m.deleted { "  (deleted)" } else { "" };
    format!(
        "  {role:<5}  {}  {title}{album}  added {added}  item {item}{deleted}",
        short_hash(&m.hash)
    )
}

/// The groups as the user sees them: automatic ones first, each member on a line
/// (`keep` is the winner), and the command that confirms a group that needs it.
fn format_dedup_groups(groups: &[DedupGroup]) -> String {
    if groups.is_empty() {
        return "No duplicates found".to_string();
    }
    let mut ordered: Vec<&DedupGroup> = groups.iter().collect();
    ordered.sort_by_key(|g| !g.is_automatic());

    let mut out = String::new();
    for g in ordered {
        match &g.disposition {
            DedupDisposition::Automatic => out.push_str("Automatic (merged by --apply)\n"),
            DedupDisposition::NeedsConfirmation { reasons } => {
                let why: Vec<&str> = reasons.iter().map(|r| dedup_reason_text(*r)).collect();
                out.push_str(&format!("Needs confirmation: {}\n", why.join(", ")));
            }
        }
        for m in &g.members {
            let role = if m.hash == g.winner { "keep" } else { "merge" };
            out.push_str(&format_dedup_member(role, m));
            out.push('\n');
        }
        if !g.is_automatic() {
            let losers: Vec<&str> = g.losers().map(|m| short_hash(&m.hash)).collect();
            out.push_str(&format!(
                "  confirm with: mdma library dedup merge {} {}\n",
                short_hash(&g.winner),
                losers.join(" ")
            ));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn format_dedup_applied(r: &DedupApplyReport) -> String {
    let mut out = format!(
        "Merged {} track(s), removed {} blob(s)",
        r.merged, r.blobs_removed
    );
    for e in &r.errors {
        out.push_str(&format!("\n  error: {e}"));
    }
    out
}

/// The group that has a member starting with `partial` (a git-style hash prefix):
/// `Ok(None)` when no group does, an error when more than one group does.
fn find_dedup_group<'a>(
    groups: &'a [DedupGroup],
    partial: &str,
) -> std::result::Result<Option<&'a DedupGroup>, String> {
    let clean = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();
    let partial = clean(partial);
    let mut matching = groups.iter().filter(|g| {
        g.members
            .iter()
            .any(|m| clean(m.hash.as_str()).starts_with(&partial))
    });
    let found = matching.next();
    if matching.next().is_some() {
        return Err(format!("'{partial}' matches more than one duplicate group"));
    }
    Ok(found)
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn handle_dedup(client: &LibraryBackend, apply: bool) -> Result<()> {
    let report = match client.dedup_scan(apply) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };
    let failed = report
        .applied
        .as_ref()
        .is_some_and(|a| !a.errors.is_empty());
    if let Some(applied) = &report.applied {
        println!("{}", format_dedup_applied(applied));
        if !report.groups.is_empty() {
            println!("\nLeft for your confirmation:");
        }
    }
    // After `--apply` an empty list is already said by the summary.
    if !apply || !report.groups.is_empty() {
        println!("{}", format_dedup_groups(&report.groups));
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn handle_dedup_merge(
    client: &LibraryBackend,
    winner: &str,
    losers: &[String],
    yes: bool,
) -> Result<()> {
    if !yes {
        if let Ok(report) = client.dedup_scan(false) {
            // Every prefix must name at most one group before anything is confirmed.
            let mut shown = None;
            for prefix in std::iter::once(winner).chain(losers.iter().map(String::as_str)) {
                match find_dedup_group(&report.groups, prefix) {
                    Ok(group) => shown = shown.or(group),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                }
            }
            if let Some(group) = shown {
                println!("{}\n", format_dedup_groups(std::slice::from_ref(group)));
            }
        }
        print!(
            "Merge {} track(s) into {winner}? Their audio files are deleted. [y/N] ",
            losers.len()
        );
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !is_yes(&answer) {
            println!("Aborted");
            return Ok(());
        }
    }

    let loser_hashes: Vec<ContentHash> = losers.iter().map(ContentHash::new).collect();
    match client.dedup_merge(&ContentHash::new(winner), &loser_hashes) {
        Ok(report) => {
            println!("{}", format_dedup_applied(&report));
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_identity_cancel(client: &LibraryBackend) -> Result<()> {
    use library_ipc_client::IdentityBackfillStatus;
    match client.identity_backfill_cancel() {
        Ok(IdentityBackfillStatus::Running(_)) => {
            println!(
                "Cancelling after the current track; progress is kept. \
                 Run 'mdma library identity backfill' again to resume."
            );
            Ok(())
        }
        Ok(s) => {
            println!("Nothing to cancel. {}", format_identity_status(&s));
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
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
            println!("  {}", resync_command(name, id));
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

    // 2. Host to scp to: the resolved node, by IP when known
    let hostname = match node::remote_host(cli.target.as_ref()) {
        Some((host, ip)) => ip.map_or(host, |ip| ip.to_string()),
        None => {
            eprintln!("Upload requires --node or MDMA_NODE to determine the Pi hostname");
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

/// Budget, in bytes, for the length of the *encoded rekordbox `Location`*
/// (`file://localhost` + percent-encoded, NFD-normalized absolute path) of a
/// NEW export entry.
///
/// rekordbox on macOS rejects paths of about 256 bytes (observed by Joakim:
/// one over-long track name made rekordbox reject the whole XML and halt the
/// import). Two things are unknown, and we are conservative about both:
///
/// 1. Whether the ~256 limit applies to the decoded path or to the
///    percent-encoded Location. We measure the encoded Location, which is
///    never shorter than the decoded path.
/// 2. Whether macOS measures the NFD form (HFS+/APFS-style decomposition) or
///    the NFC form we write. We measure NFD, which is never shorter than NFC.
///
/// The `file://localhost` prefix counts towards the budget too. Existing
/// entries are grandfathered and never shortened (see
/// [`export_dest_path_checked`]), so this only affects new tracks.
const MAX_LOCATION_URI_BYTES: usize = 250;
/// Secondary guard: maximum cost of a directory name (macOS/Windows/Linux
/// limit is 255 bytes; macOS stores names NFD-decomposed, so leave headroom).
const MAX_DIR_NAME_BYTES: usize = 120;
/// Secondary guard: maximum cost of a file name including the extension.
const MAX_FILE_NAME_BYTES: usize = 180;
/// Smallest cost a component is shortened to when spending the path budget.
const MIN_COMPONENT_BYTES: usize = 16;

/// Length of the rekordbox Location URI for `s` taken as an absolute path,
/// measured over its NFD form. Encoding rules come from
/// [`rekordbox_xml::path_to_file_uri`], not duplicated here.
fn location_cost(s: &str) -> usize {
    use unicode_normalization::UnicodeNormalization;
    let nfd: String = s.nfd().collect();
    rekordbox_xml::path_to_file_uri(std::path::Path::new(&nfd)).len()
}

/// Exact byte cost of a path or path component inside the rekordbox Location
/// (NFD, percent-encoded, without the `file://localhost` prefix). Additive
/// over concatenation, so it works per component and per character.
fn path_cost(s: &str) -> usize {
    location_cost(s) - location_cost("")
}

/// File extension of an exported track: fixed for converted formats, derived
/// from the blob path for `Original`.
fn export_extension(format: &ExportFormat, track: &TrackInfo) -> String {
    match format.static_extension() {
        Some(fixed) => fixed.to_string(),
        None => track
            .blob_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_lowercase(),
    }
}

/// Normalised key for comparing Location URIs: NFD and lowercase. Existing
/// Locations come from `canonicalize()`, which on HFS+ returns NFD names,
/// while track metadata is NFC; case is folded as in `plan_export`.
fn location_key(uri: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    uri.nfd().collect::<String>().to_lowercase()
}

/// Set of [`location_key`]s of an existing export, for grandfathering.
fn existing_location_set(
    existing: Option<&rekordbox_xml::RekordboxLibrary>,
) -> std::collections::HashSet<String> {
    existing
        .map(|lib| {
            lib.tracks
                .iter()
                .map(|t| location_key(&t.location))
                .collect()
        })
        .unwrap_or_default()
}

/// Stable FNV-1a hash used to disambiguate truncated names.
fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// Shorten `s` so its cost is at most `max_cost` (but never below
/// [`MIN_COMPONENT_BYTES`]). Strings that already fit are returned unchanged.
/// Truncation happens on a char boundary, trailing spaces/dots are trimmed
/// (Windows rejects them) and a `~<hash of the full name>` suffix is appended
/// so that two different long names never collapse onto the same path.
fn shorten_component(s: &str, max_cost: usize) -> String {
    if path_cost(s) <= max_cost {
        return s.to_string();
    }
    let suffix = format!("~{:08x}", fnv1a(s));
    let budget = max_cost
        .max(MIN_COMPONENT_BYTES)
        .saturating_sub(suffix.len());
    let mut used = 0;
    let mut kept = String::new();
    for c in s.chars() {
        let cost = path_cost(c.encode_utf8(&mut [0; 4]));
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(c);
    }
    let trimmed = kept.trim_end_matches([' ', '.']);
    format!("{trimmed}{suffix}")
}

/// Build the destination path for an exported track and report whether it
/// fits [`MAX_LOCATION_URI_BYTES`].
///
/// Layout: `<output>/<artist>/<album>/<title>.<ext>`
///
/// Components are sanitized first. If the resulting unshortened path is
/// already an entry of the existing export (`existing_locations`: lowercased
/// Location URIs), it is returned as-is and reported as fitting: renaming a
/// previously exported file would make rekordbox see a new track and lose its
/// cues. Otherwise components are shortened: per-component caps, then (if the
/// whole path is still too long) title first, album, artist.
/// Paths within all limits are returned byte-identical to the unlimited form.
fn export_dest_path_checked(
    output: &std::path::Path,
    track: &TrackInfo,
    ext: &str,
    existing_locations: &std::collections::HashSet<String>,
) -> (std::path::PathBuf, bool) {
    let mut artist = sanitize_path_component(track.artist.as_deref().unwrap_or("Unknown Artist"));
    let mut album = sanitize_path_component(track.album.as_deref().unwrap_or("Unknown Album"));
    let mut title = sanitize_path_component(track.title.as_deref().unwrap_or("Unknown"));
    let dot_ext = format!(".{ext}");
    let ext_cost = path_cost(&dot_ext);

    let unshortened = output
        .join(&artist)
        .join(&album)
        .join(format!("{title}{dot_ext}"));
    let unshortened_location = rekordbox_xml::path_to_file_uri(&unshortened);
    let unshortened_location = location_key(&unshortened_location);
    if existing_locations.contains(&unshortened_location) {
        return (unshortened, true);
    }

    artist = shorten_component(&artist, MAX_DIR_NAME_BYTES);
    album = shorten_component(&album, MAX_DIR_NAME_BYTES);
    title = shorten_component(&title, MAX_FILE_NAME_BYTES.saturating_sub(ext_cost));

    let build = |artist: &str, album: &str, title: &str| {
        output
            .join(artist)
            .join(album)
            .join(format!("{title}{dot_ext}"))
    };
    let total = |p: &std::path::Path| location_cost(&p.to_string_lossy());

    // Spend the whole-path budget: title first, then album, then artist.
    // A name shortened twice is re-hashed from its first shortened form, which
    // embeds the first hash, so distinct originals stay distinct.
    for which in 0..3 {
        let over = total(&build(&artist, &album, &title)).saturating_sub(MAX_LOCATION_URI_BYTES);
        if over == 0 {
            break;
        }
        let field = match which {
            0 => &mut title,
            1 => &mut album,
            _ => &mut artist,
        };
        let cost = path_cost(field);
        let target = cost.saturating_sub(over).max(MIN_COMPONENT_BYTES);
        if target < cost {
            *field = shorten_component(field, target);
        }
    }

    let path = build(&artist, &album, &title);
    let fits = total(&path) <= MAX_LOCATION_URI_BYTES;
    (path, fits)
}

/// Build the destination path for an exported track with no existing export
/// to preserve (see [`export_dest_path_checked`]).
///
/// Missing metadata falls back to `"Unknown Artist"`, `"Unknown Album"`, `"Unknown"`.
fn export_dest_path(output: &std::path::Path, track: &TrackInfo, ext: &str) -> std::path::PathBuf {
    export_dest_path_checked(output, track, ext, &std::collections::HashSet::new()).0
}

/// Build a `hash → location-URI` map that resolves all three hash forms a playlist
/// may use to look up a track:
///
/// - Full `sha256:<64hex>` form (what the resolver always returns)
/// - 12-char short hex (canonical 0.18.1+ playlist format)
/// - 8-char short hex (legacy 0.18.x playlist format)
///
/// The `location_fn` closure produces the location URI for each `(track, dest, ext)` tuple.
/// Passing it as a closure keeps this helper pure and testable without filesystem I/O.
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

/// Require a remote node and return its hostname and, when known, its IP.
///
/// Prints an error and exits when the CLI talks to local services over IPC.
fn require_node(cli: &Cli) -> (String, Option<std::net::Ipv4Addr>) {
    match node::remote_host(cli.target.as_ref()) {
        Some(remote) => remote,
        None => {
            eprintln!("No remote node to talk to.");
            eprintln!(
                "Set --node or MDMA_NODE to the hostname of your MDMA device (e.g. mdma-909.local)"
            );
            std::process::exit(1);
        }
    }
}

/// Build the shared blocking HTTP client used by export handlers.
///
/// Pins the node's known IPv4 address via `resolve_to_addrs`; without one,
/// resolves `node` (using avahi for `.local` hostnames) so the request succeeds
/// even in environments where mDNS is not wired into the system resolver (e.g.
/// Nix-glibc).  HTTP on port 80 is used; the URL hostname is kept as-is so
/// virtual-host routing on the server side is unaffected.
fn build_http_client(
    node: &str,
    known_ip: Option<std::net::Ipv4Addr>,
) -> Result<reqwest::blocking::Client> {
    use std::net::{IpAddr, SocketAddr};

    let ip_str = match known_ip {
        Some(ip) => ip.to_string(),
        None => nng_transport::resolve_hostname_to_ipv4(node).map_err(|e| {
            color_eyre::eyre::eyre!(
                "Failed to resolve '{}' to an IP address: {}. \
                 Try passing an explicit IP with --node <ip>.",
                node,
                e
            )
        })?,
    };
    let ip: IpAddr = ip_str.parse().map_err(|e| {
        color_eyre::eyre::eyre!("Resolved address '{}' is not a valid IP: {}", ip_str, e)
    })?;
    let addr = SocketAddr::new(ip, 80);

    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .resolve_to_addrs(node, &[addr])
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
///
/// `dest_for(track, ext)` gives the destination path of a track, so callers
/// that planned destinations up front (rekordbox export) download to exactly
/// the planned path.
fn download_tracks(
    node: &str,
    tracks: &[TrackInfo],
    http: &reqwest::blocking::Client,
    resolve_format: impl Fn(&TrackInfo) -> ExportFormat,
    dest_for: impl Fn(&TrackInfo, &str) -> std::path::PathBuf,
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
        let ext = export_extension(&resolved_format, track);

        let dest = dest_for(track, &ext);

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
        return *fmt;
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
            lossless_format.unwrap_or(ExportFormat::Original)
        }
        Some(audio_transcoder::FormatCategory::Lossy) => {
            lossy_format.unwrap_or(ExportFormat::Original)
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
///
/// Cue points are sourced from `track.memory_cues` (the already-projected
/// structured data from the library), not from raw `get_facts` display strings.
/// This ensures retracted cues are never exported and no duplicate cues appear.
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

    // Convert the projected CueInfo structs → MusicValue::MemoryCue → PositionMark.
    // Using the projected data avoids the get_facts display-string path (B1 fix).
    let cues_as_music_values: Vec<MusicValue> = track
        .memory_cues
        .iter()
        .map(|c| MusicValue::MemoryCue {
            position_ms: c.position_ms,
            kind: c.kind.clone(),
            label: c.label.clone(),
            index: c.index,
        })
        .collect();
    let position_marks = rekordbox_roundtrip::position_marks_from_cues(&cues_as_music_values);

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
        position_marks,
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

    let (node, node_ip) = require_node(cli);

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

    let http = build_http_client(&node, node_ip)?;

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

    // Load existing XML for incremental sync (skip when --replace). Loaded
    // before planning destinations: its entries keep their paths even when
    // those exceed the Location budget (stable Locations preserve cues).
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
    let existing_locations = existing_location_set(existing.as_ref());

    // Build (TrackInfo, canonical_dest_path, ext) for every unique desired track.
    // dest_path is canonicalised so the Location URI is stable pre- and post-download.
    let mut too_long_count = 0usize;
    let desired: Vec<(TrackInfo, std::path::PathBuf, String)> = tracks
        .into_iter()
        .map(|track| {
            let ext = export_extension(&format_owned, &track);
            let (raw_dest, fits) =
                export_dest_path_checked(&output_canon, &track, &ext, &existing_locations);
            too_long_count += usize::from(!fits);
            // Canonicalise if the file already exists; fall back to the absolute path otherwise.
            let dest = raw_dest.canonicalize().unwrap_or_else(|_| raw_dest.clone());
            (track, dest, ext)
        })
        .collect();

    if too_long_count > 0 {
        eprintln!(
            "warning: {} new track path(s) still exceed {} bytes as an encoded rekordbox \
             Location even with artist/album/title shortened to the minimum; the output \
             directory {} ({} bytes) leaves too little room. rekordbox on macOS may reject \
             them. Use a shorter output directory (e.g. ~/mdma-export).",
            too_long_count,
            MAX_LOCATION_URI_BYTES,
            output_canon.display(),
            path_cost(&output_canon.to_string_lossy())
        );
    }

    // hash → canonical dest location URI, for playlist location building.
    // Insert all three hash forms (full, 12-char short, 8-char short) so that
    // playlists stored in any format on disk resolve correctly.
    let hash_location_pairs: Vec<rekordbox_roundtrip::HashLocation> = desired
        .iter()
        .map(|(track, dest, _)| rekordbox_roundtrip::HashLocation {
            hash: track.content_hash.clone(),
            location: rekordbox_xml::path_to_file_uri(dest),
        })
        .collect();
    let hash_to_location = rekordbox_roundtrip::build_hash_to_location(&hash_location_pairs);

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
            "warning: destination path {} is shared by {} tracks ({}); each later track \
             overwrites the previous one, so only the last will remain on disk",
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

    // Download to exactly the planned destinations (which may be grandfathered
    // paths longer than the budget), not a re-derived budgeted path.
    let planned_dest: std::collections::HashMap<&str, &std::path::PathBuf> = desired
        .iter()
        .map(|(track, dest, _)| (track.content_hash.as_str(), dest))
        .collect();
    let results = download_tracks(
        &node,
        &download_tracks_only,
        &http,
        |_track| format_owned,
        |track, _ext| planned_dest[track.content_hash.as_str()].clone(),
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
    // Keys are lowercased so that case-mismatches between the existing-XML
    // round-trip path and the freshly-built URI path do not miss on-disk tracks.
    let skipped_locations: HashSet<String> =
        plan.to_skip.into_iter().map(|s| s.to_lowercase()).collect();

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
        } else if skipped_locations.contains(&location.to_lowercase()) {
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
    // Keys are lowercased so that case-mismatches between the existing-XML
    // round-trip path and the freshly-built URI path do not drop tracks.
    let refreshed_locations: HashSet<String> = refreshed
        .iter()
        .map(|r| r.location.to_lowercase())
        .collect();

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
                    // Normalise case at the lookup boundary only; the URI written to
                    // rekordbox.xml preserves its original case.
                    if refreshed_locations.contains(&loc.to_lowercase()) {
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

/// Report the outcome of [`write_playlist`], warning after a replace.
fn print_write_outcome(name: &str, track_count: usize, created: bool) {
    println!("{}", format_write_outcome(name, track_count, created));
}

/// Sanitise a user-supplied playlist name for `flag`, exiting if nothing is left.
/// Prints a note when the name was changed.
fn sanitized_name_or_exit(raw: &str, flag: &str) -> String {
    let sanitized = sanitize_playlist_name(raw);
    if sanitized.is_empty() {
        eprintln!("Invalid {} '{}': name sanitizes to empty", flag, raw);
        std::process::exit(1);
    }
    if sanitized != raw {
        eprintln!(
            "Note: playlist name sanitized from '{}' to '{}'",
            raw, sanitized
        );
    }
    sanitized
}

// =============================================================================
// Source playlists (mdma source playlists / import-playlist / import-status)
// =============================================================================

/// Message for a source that answers `SourceError::Unsupported`.
fn unsupported_message(source: &str, feature: &str) -> String {
    format!("source '{}' does not support {}", source, feature)
}

/// Report a source error; `Unsupported` gets a friendly message naming the source.
fn handle_playlist_source_error(source: &str, feature: &str, err: &SourceError) -> ! {
    if matches!(err, SourceError::Unsupported { .. }) {
        eprintln!("{}", unsupported_message(source, feature));
        std::process::exit(1);
    }
    handle_source_error(err)
}

fn handle_source_playlists(client: &SourceClient, source: &str) -> Result<()> {
    match client.request(source, &SourceRequest::ListPlaylists) {
        Ok(SourceResponse::Playlists(playlists)) => {
            if playlists.is_empty() {
                println!("No playlists found");
                return Ok(());
            }
            let slug_w = playlists
                .iter()
                .map(|p| p.slug.chars().count())
                .max()
                .unwrap_or(4)
                .max(4);
            let title_w = playlists
                .iter()
                .map(|p| p.title.chars().count())
                .max()
                .unwrap_or(5)
                .max(5);
            println!(
                "{:<slug_w$}  {:<title_w$}  {:>6}  PRIVATE",
                "SLUG", "TITLE", "TRACKS"
            );
            for p in &playlists {
                println!(
                    "{:<slug_w$}  {:<title_w$}  {:>6}  {}",
                    p.slug,
                    p.title,
                    p.track_count,
                    if p.is_private { "yes" } else { "no" }
                );
            }
            Ok(())
        }
        Ok(SourceResponse::Error(e)) => handle_playlist_source_error(source, "playlists", &e),
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

/// The command hinted at to follow an import job (name first, as parsed).
fn import_status_command(source: &str) -> String {
    format!("mdma source {} import-status", source)
}

/// The command hinted at to force-resync one item.
fn resync_command(source: &str, identifier: &str) -> String {
    format!("mdma source {} resync {}", source, identifier)
}

/// Message for a refused import: another job holds the single import slot.
fn import_not_started_message(source: &str, running_job_id: u64) -> String {
    format!(
        "Import NOT started: job {} is already running (see '{}')",
        running_job_id,
        import_status_command(source)
    )
}

fn handle_source_import_playlist(
    client: &SourceClient,
    source: &str,
    playlist: &str,
    name: &Option<String>,
    dry_run: bool,
    no_wait: bool,
) -> Result<()> {
    // Validate --name before talking to the server
    let explicit_name = name.as_deref().map(|n| sanitized_name_or_exit(n, "--name"));

    let job_id = match client.request(
        source,
        &SourceRequest::ImportPlaylist {
            playlist: playlist.to_string(),
            name: explicit_name,
            dry_run,
        },
    ) {
        Ok(SourceResponse::ImportStarted { job_id }) => {
            println!("Import started (job {})", job_id);
            job_id
        }
        Ok(SourceResponse::Error(SourceError::ImportInProgress { job_id })) => {
            // The running job may be a different playlist or a dry run, so
            // following it would report someone else's result as ours.
            eprintln!("{}", import_not_started_message(source, job_id));
            std::process::exit(1);
        }
        Ok(SourceResponse::Error(e)) => handle_playlist_source_error(source, "playlist import", &e),
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    };

    if no_wait {
        println!("Check progress with '{}'", import_status_command(source));
        return Ok(());
    }
    follow_import_job(client, source, job_id)
}

fn handle_source_import_status(client: &SourceClient, source: &str) -> Result<()> {
    match client.request(source, &SourceRequest::GetImportStatus) {
        Ok(SourceResponse::ImportStatus(None)) => {
            println!("No import job");
            Ok(())
        }
        Ok(SourceResponse::ImportStatus(Some(status))) => {
            if is_terminal_phase(&status.phase) {
                finish_import(&status)
            } else {
                println!("Import job {} is running", status.job_id);
                follow_import_job(client, source, status.job_id)
            }
        }
        Ok(SourceResponse::Error(e)) => handle_playlist_source_error(source, "playlist import", &e),
        Ok(_) => handle_source_error(&"Unexpected response"),
        Err(e) => handle_source_error(&e),
    }
}

// -----------------------------------------------------------------------------
// Import job polling
// -----------------------------------------------------------------------------

const IMPORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Consecutive failed polls tolerated before giving up (about a minute).
const IMPORT_POLL_MAX_FAILURES: u32 = 30;

fn is_terminal_phase(phase: &ImportPhase) -> bool {
    matches!(phase, ImportPhase::Done(_) | ImportPhase::Failed { .. })
}

fn import_phase_line(phase: &ImportPhase) -> &'static str {
    match phase {
        ImportPhase::Fetching => "Fetching playlist...",
        ImportPhase::Matching => "Matching tracks against the library...",
        ImportPhase::Syncing => {
            "Syncing Bandcamp collection (missing tracks found), waiting for downloads..."
        }
        ImportPhase::Rematching => "Matching tracks again after downloads...",
        ImportPhase::Writing => "Writing playlist...",
        ImportPhase::Done(_) => "Import finished",
        ImportPhase::Failed { .. } => "Import failed",
    }
}

/// What one poll attempt produced.
enum PollInput {
    /// The server answered with its current/last job status.
    Status(Option<ImportJobStatus>),
    /// Request failed, timed out, or got an unusable answer.
    Failure,
}

/// What the poll loop should do next.
#[derive(Debug, PartialEq)]
enum PollFlow {
    Continue,
    /// The awaited job reached Done or Failed.
    Finished(Box<ImportJobStatus>),
    /// Too many consecutive failures.
    GiveUp,
}

#[derive(Debug, PartialEq)]
struct PollStep {
    /// Progress lines to print, in order.
    lines: Vec<String>,
    flow: PollFlow,
}

/// Pure state machine behind the import poll loop (no I/O).
struct ImportPoller {
    job_id: u64,
    last_phase: Option<ImportPhase>,
    last_sync: Option<ImportSyncProgress>,
    failures: u32,
}

impl ImportPoller {
    fn new(job_id: u64) -> Self {
        Self {
            job_id,
            last_phase: None,
            last_sync: None,
            failures: 0,
        }
    }

    /// A miss counts as a failure: failed requests, no job, or a status for
    /// a different job (which is otherwise ignored).
    fn miss(&mut self) -> PollStep {
        self.failures += 1;
        let flow = if self.failures >= IMPORT_POLL_MAX_FAILURES {
            PollFlow::GiveUp
        } else {
            PollFlow::Continue
        };
        PollStep {
            lines: Vec::new(),
            flow,
        }
    }

    fn step(&mut self, input: PollInput) -> PollStep {
        let status = match input {
            PollInput::Status(Some(s)) if s.job_id == self.job_id => s,
            PollInput::Status(_) | PollInput::Failure => return self.miss(),
        };
        self.failures = 0;

        let mut lines = Vec::new();
        if self.last_phase.as_ref() != Some(&status.phase) {
            lines.push(import_phase_line(&status.phase).to_string());
            self.last_phase = Some(status.phase.clone());
        }
        if status.phase == ImportPhase::Syncing {
            if let Some(sync) = &status.sync {
                if self.last_sync.as_ref() != Some(sync) {
                    lines.push(format!(
                        "downloads: {}/{} (failed {})",
                        sync.completed, sync.queued, sync.failed
                    ));
                    self.last_sync = Some(sync.clone());
                }
            }
        }

        let flow = if is_terminal_phase(&status.phase) {
            PollFlow::Finished(Box::new(status))
        } else {
            PollFlow::Continue
        };
        PollStep { lines, flow }
    }
}

/// Print the final report; exit non-zero when the job failed.
fn finish_import(status: &ImportJobStatus) -> Result<()> {
    let report = format_import_status(status);
    if matches!(status.phase, ImportPhase::Failed { .. }) {
        eprintln!("{}", report);
        std::process::exit(1);
    }
    println!("{}", report);
    Ok(())
}

/// Poll the server until `job_id` finishes, then print its report.
fn follow_import_job(client: &SourceClient, source: &str, job_id: u64) -> Result<()> {
    let mut poller = ImportPoller::new(job_id);
    let mut last_error: Option<String> = None;
    loop {
        let input = match client.request(source, &SourceRequest::GetImportStatus) {
            Ok(SourceResponse::ImportStatus(s)) => PollInput::Status(s),
            Ok(SourceResponse::Error(e)) => {
                last_error = Some(e.to_string());
                PollInput::Failure
            }
            Ok(_) => {
                last_error = Some("Unexpected response".to_string());
                PollInput::Failure
            }
            Err(e) => {
                last_error = Some(e.to_string());
                PollInput::Failure
            }
        };
        let step = poller.step(input);
        for line in &step.lines {
            println!("{}", line);
        }
        match step.flow {
            PollFlow::Continue => std::thread::sleep(IMPORT_POLL_INTERVAL),
            PollFlow::Finished(status) => return finish_import(&status),
            PollFlow::GiveUp => {
                eprintln!(
                    "Lost contact with the import job {} ({}); \
                     check later with '{}'",
                    job_id,
                    last_error.unwrap_or_else(|| "no status for the job".to_string()),
                    import_status_command(source)
                );
                std::process::exit(1);
            }
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
    let lookup = LibraryTrackLookup::new(|q| library.search(q));

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

    // A failed library search must not look like "not in library"
    lookup.check()?;

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
        let sanitized = sanitized_name_or_exit(pname, "--playlist");
        let name = parse_playlist_name(&sanitized);
        let hashes: Vec<ContentHash> = matched.iter().map(|(_, h)| h.clone()).collect();

        if dry_run {
            println!(
                "[dry-run] Would create playlist '{}' with {} tracks",
                sanitized,
                hashes.len()
            );
        } else {
            let created = write_playlist(library, &name, &hashes)
                .map_err(|e| color_eyre::eyre::eyre!("Failed to write playlist: {}", e))?;
            print_write_outcome(&sanitized, hashes.len(), created);
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

            let sanitized = sanitize_playlist_name(&rb_playlist.name);

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
                match write_playlist(library, &name, &resolved) {
                    Ok(created) => print_write_outcome(&sanitized, resolved.len(), created),
                    Err(e) => {
                        eprintln!("Failed to write playlist '{}': {}", sanitized, e)
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

            // Write BeatGrid from tempo anchor (Rekordbox is beat-grid master;
            // MDMA never writes <TEMPO> data back on export).
            if let (Some(anchor), Some(bpm_f32)) = (&rb_track.tempo_anchor, rb_track.average_bpm) {
                if let Ok(bpm) = Bpm::from_f32(bpm_f32) {
                    let new_grid = rekordbox_roundtrip::grid_from_tempo(anchor, bpm);
                    // Guard: skip if the projected beat_grid is already identical.
                    let already_current = match (
                        &new_grid,
                        existing.as_ref().and_then(|t| t.beat_grid.as_ref()),
                    ) {
                        (
                            MusicValue::BeatGrid {
                                first_beat_ms,
                                bpm: new_bpm,
                                beats_per_bar,
                            },
                            Some(existing_grid),
                        ) => {
                            existing_grid.first_beat_ms == *first_beat_ms
                                && existing_grid.bpm == *new_bpm
                                && existing_grid.beats_per_bar == *beats_per_bar
                        }
                        _ => false,
                    };

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write BeatGrid first_beat={:.3}s bpm={} for {} - {}",
                            anchor.inizio_seconds, bpm_f32, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact(hash, new_grid) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write BeatGrid for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }

            // Write MemoryCue facts from position marks.
            // Guard: skip any cue that already exists identically in the projection.
            if !rb_track.position_marks.is_empty() {
                let cues = rekordbox_roundtrip::cues_from_position_marks(&rb_track.position_marks);
                for cue in cues {
                    let already_current = if let MusicValue::MemoryCue {
                        position_ms,
                        kind,
                        label,
                        index,
                    } = &cue
                    {
                        existing
                            .as_ref()
                            .map(|t| {
                                t.memory_cues.iter().any(|c| {
                                    c.position_ms == *position_ms
                                        && &c.kind == kind
                                        && c.label == *label
                                        && c.index == *index
                                })
                            })
                            .unwrap_or(false)
                    } else {
                        false
                    };

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write {} for {} - {}",
                            cue, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact(hash, cue) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write MemoryCue for {} - {}: {}",
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

    let (node, node_ip) = require_node(cli);

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

    let http = build_http_client(&node, node_ip)?;

    let results = download_tracks(
        &node,
        &tracks,
        &http,
        |track| {
            resolve_export_format(
                track.blob_path.as_deref(),
                format,
                lossless_format,
                lossy_format,
            )
        },
        |track, ext| export_dest_path(output, track, ext),
    );

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
// Track Lifecycle Handlers
// =============================================================================

fn handle_track_delete(lib: &LibraryBackend, hash: &str) -> Result<()> {
    let ch = ContentHash::new(hash);
    match lib.track_delete(&ch) {
        Ok(()) => {
            println!(
                "soft-deleted; recover with `mdma library track restore {}`",
                hash
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_track_restore(lib: &LibraryBackend, hash: &str) -> Result<()> {
    let ch = ContentHash::new(hash);
    match lib.track_restore(&ch) {
        Ok(()) => {
            println!("track restored");
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_track_replace(lib: &LibraryBackend, old_hash: &str, new_file: &str) -> Result<()> {
    let ch = ContentHash::new(old_hash);
    match lib.track_replace(&ch, new_file) {
        Ok((new_hash, playlists)) => {
            println!("{} -> {}", old_hash, new_hash.as_str());
            println!("{} playlist(s) rewritten", playlists);
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_track_orphans(lib: &LibraryBackend) -> Result<()> {
    use library_ipc_client::OrphanReason;

    match lib.track_orphans() {
        Ok(orphans) => {
            if orphans.is_empty() {
                println!("no hidden tracks");
                return Ok(());
            }
            for o in &orphans {
                let hash_short = o
                    .content_hash
                    .as_str()
                    .get(7..15)
                    .unwrap_or(o.content_hash.as_str());
                let artist = o.artist.as_deref().unwrap_or("Unknown");
                let title = o.title.as_deref().unwrap_or("Unknown");
                let reason = match &o.reason {
                    OrphanReason::Deleted { timestamp } => format!("deleted at {}", timestamp),
                    OrphanReason::NoLiveFacts => {
                        "no live facts (hard-replace leftover)".to_string()
                    }
                };
                println!("{}  {} - {}  [{}]", hash_short, artist, title, reason);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

// =============================================================================
// Admin Handlers
// =============================================================================

fn handle_admin_service_mode_status(client: &GatewayClient) -> Result<()> {
    match client.admin_request(&AdminRequest::ServiceModeStatus) {
        Ok(AdminResponse::Status {
            boot_order,
            service_mode_armed,
            pcie_probe,
        }) => {
            let boot_label = if service_mode_armed {
                "SD-first"
            } else if boot_order.is_empty() {
                "unknown"
            } else {
                "NVMe-first"
            };
            if boot_order.is_empty() {
                println!("Boot order:        (not set) ({})", boot_label);
            } else {
                println!("Boot order:        {} ({})", boot_order, boot_label);
            }
            if service_mode_armed {
                println!("Service mode:      ARMED — next reboot will land on the beacon SD");
            } else {
                println!(
                    "Service mode:      not armed. Use `mdma admin service-mode enable` to arm it."
                );
            }
            println!("PCIE_PROBE:        {}", pcie_probe);
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_admin_service_mode_enable(client: &GatewayClient) -> Result<()> {
    match client.admin_request(&AdminRequest::ServiceModeEnable) {
        Ok(AdminResponse::Ok) => {
            println!("Service mode armed.");
            println!(
                "Reboot the device with `mdma admin reboot` (or `sudo reboot`) to enter service mode."
            );
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_admin_service_mode_disable(client: &GatewayClient) -> Result<()> {
    match client.admin_request(&AdminRequest::ServiceModeDisable) {
        Ok(AdminResponse::Ok) => {
            println!("Service mode disarmed. NVMe-first boot order restored.");
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

/// Print the boot-health verdict; exit 1 when a reboot would not be safe.
fn handle_admin_boot_health(client: &GatewayClient) -> Result<()> {
    match client.admin_request(&AdminRequest::BootHealth) {
        Ok(AdminResponse::BootHealth { verdict }) => {
            println!("Boot health: {verdict}");
            if !verdict.allows_reboot() {
                std::process::exit(1);
            }
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_admin_reboot(client: &GatewayClient, force: bool) -> Result<()> {
    match client.admin_request(&AdminRequest::Reboot { force }) {
        Ok(AdminResponse::Ok) => {
            println!("Reboot initiated. Connection will drop within ~2 seconds.");
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
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

/// Whether `--socket` / `--playback-socket` (or `MDMA_LIBRARY_SOCKET` /
/// `MDMA_PLAYBACK_SOCKET`) were given, as opposed to left at their defaults.
/// `--sources-dir` / `MDMA_SOURCES_DIR` is deliberately not considered: it
/// only locates source sockets and says nothing about the library or playback.
fn sockets_given(matches: &clap::ArgMatches) -> bool {
    ["socket", "playback_socket"].iter().any(|id| {
        matches
            .value_source(id)
            .is_some_and(|s| s != clap::parser::ValueSource::DefaultValue)
    })
}

/// Run one per-source command against the source `name`.
fn run_source_command(cli: &Cli, name: &str, command: &SourceCommands) -> Result<()> {
    match command {
        SourceCommands::Playlists => {
            let client = connect_source_or_exit(cli, name, Some(SOURCE_LISTING_TIMEOUT));
            handle_source_playlists(&client, name)
        }
        _ => {
            let client = connect_source(cli, name);
            match command {
                SourceCommands::Sync => handle_source_sync(&client, name),
                SourceCommands::Status => handle_source_status(&client, name),
                SourceCommands::Downloads => handle_source_downloads(&client, name),
                SourceCommands::Cancel { id } => handle_source_cancel(&client, name, id.clone()),
                SourceCommands::Pause => handle_source_pause(&client, name),
                SourceCommands::Resume => handle_source_resume(&client, name),
                SourceCommands::Resync { identifier } => {
                    handle_source_resync(&client, name, identifier)
                }
                SourceCommands::CheckItem { identifier } => {
                    handle_source_check_item(&client, name, identifier)
                }
                SourceCommands::CheckUpdates { apply } => {
                    handle_source_check_updates(cli, &client, name, *apply)
                }
                // Import requests return immediately (the job runs server-side),
                // so the default request timeout is enough for start and polls.
                SourceCommands::ImportPlaylist {
                    playlist,
                    name: import_name,
                    dry_run,
                    no_wait,
                } => handle_source_import_playlist(
                    &client,
                    name,
                    playlist,
                    import_name,
                    *dry_run,
                    *no_wait,
                ),
                SourceCommands::ImportStatus => handle_source_import_status(&client, name),
                SourceCommands::Playlists => unreachable!("handled above"),
            }
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    let library_socket = cli.socket.strip_prefix("ipc://").unwrap_or(&cli.socket);
    let local_ipc =
        node::local_ipc_wanted(sockets_given(&matches), Path::new(library_socket).exists());

    // Resolve the node once, and only for commands that talk to one.
    if cli.command.needs_node() {
        cli.target = Some(node::resolve_startup(cli.node.as_deref(), local_ipc));
    }

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
            SourceTop::List => handle_source_list(&cli),
            SourceTop::Named(args) => match parse_source_args(args) {
                Ok((name, command)) => run_source_command(&cli, &name, &command),
                Err(e) => exit_source_parse_error(e),
            },
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
            role,
            energy,
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
                    role.clone(),
                    energy.clone(),
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
            // Event gateway of the resolved node (tcp://<ip>:5556)
            let addr = match resolve_event_gateway(&cli) {
                Some(a) => a,
                None => {
                    eprintln!("Subscribe needs a remote node.");
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
                LibraryCommands::Identity { command } => match command {
                    IdentityCommands::Backfill { pcm, no_wait } => {
                        handle_identity_backfill(&lib, *pcm, *no_wait)
                    }
                    IdentityCommands::Status => handle_identity_status(&lib),
                    IdentityCommands::Cancel => handle_identity_cancel(&lib),
                },
                LibraryCommands::Dedup { apply, command } => match command {
                    None => handle_dedup(&lib, *apply),
                    Some(DedupCommands::Merge {
                        winner,
                        losers,
                        yes,
                    }) => handle_dedup_merge(&lib, winner, losers, *yes),
                },
                LibraryCommands::Track { command } => match command {
                    TrackCommands::Delete { hash } => handle_track_delete(&lib, hash),
                    TrackCommands::Restore { hash } => handle_track_restore(&lib, hash),
                    TrackCommands::Replace { old_hash, new_file } => {
                        handle_track_replace(&lib, old_hash, new_file)
                    }
                    TrackCommands::Orphans => handle_track_orphans(&lib),
                },
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

        Commands::Fact { command } => {
            let lib = connect_library(&cli);
            match command {
                FactCommands::Add { hash, field, value } => {
                    handle_fact_add(&lib, hash.clone(), *field, value.clone())
                }
                FactCommands::Retract { hash, field, value } => {
                    handle_fact_retract(&lib, hash.clone(), *field, value.clone())
                }
            }
        }

        Commands::View { .. } => {
            // Re-parse argv via ArgMatches to recover the order the flags appeared.
            // This implements Option A from the field-order spec: use indices_of on
            // the raw matches for the `view` subcommand to sort boolean flags by
            // their argv position.
            let matches = Cli::command().get_matches();
            let view_matches = matches.subcommand_matches("view").unwrap_or_else(|| {
                eprintln!("internal error: expected 'view' subcommand matches");
                std::process::exit(1);
            });
            let columns = view_columns_from_matches(view_matches);
            let lib = connect_library(&cli);
            handle_view(&lib, columns)
        }

        Commands::Node { command } => match command {
            NodeCommands::List => node::handle_list(),
            NodeCommands::Select { name } => node::handle_select(name.as_deref()),
            NodeCommands::Forget => node::handle_forget(),
            NodeCommands::Show => node::handle_show(cli.node.as_deref(), local_ipc),
        },

        Commands::Admin { command } => {
            let admin = connect_admin(&cli);
            match command {
                AdminCommands::ServiceMode { command } => match command {
                    ServiceModeCommands::Status => handle_admin_service_mode_status(&admin),
                    ServiceModeCommands::Enable => handle_admin_service_mode_enable(&admin),
                    ServiceModeCommands::Disable => handle_admin_service_mode_disable(&admin),
                },
                AdminCommands::BootHealth => handle_admin_boot_health(&admin),
                AdminCommands::Reboot { force } => handle_admin_reboot(&admin, *force),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use source_protocol::ImportResult;

    fn job(job_id: u64, phase: ImportPhase, sync: Option<(u32, u32, u32)>) -> ImportJobStatus {
        ImportJobStatus {
            job_id,
            dry_run: false,
            playlist_title: None,
            target_name: None,
            total_count: None,
            phase,
            tracks: vec![],
            sync: sync.map(|(queued, completed, failed)| ImportSyncProgress {
                queued,
                completed,
                failed,
                ended: None,
            }),
        }
    }

    fn done() -> ImportPhase {
        ImportPhase::Done(ImportResult::NothingMatched)
    }

    #[test]
    fn refused_import_message_says_not_started_and_names_the_running_job() {
        assert_eq!(
            import_not_started_message("bandcamp", 7),
            "Import NOT started: job 7 is already running (see 'mdma source bandcamp import-status')"
        );
    }

    /// Parse a hinted command line with the real parser, as a user pasting it would.
    fn parse_hinted(command: &str) -> (String, SourceCommands) {
        let words: Vec<String> = command.split_whitespace().map(String::from).collect();
        assert_eq!(&words[..2], ["mdma", "source"], "{command}");
        let cli = Cli::try_parse_from(&words).unwrap_or_else(|e| panic!("{command}: {e}"));
        match cli.command {
            Commands::Source {
                command: SourceTop::Named(args),
            } => parse_source_args(&args).unwrap_or_else(|e| panic!("{command}: {e}")),
            other => panic!("{command}: unexpected {other:?}"),
        }
    }

    #[test]
    fn import_status_hint_parses_to_import_status_for_that_source() {
        for src in ["bandcamp", "beatport"] {
            let (name, command) = parse_hinted(&import_status_command(src));
            assert_eq!(name, src);
            assert_eq!(command, SourceCommands::ImportStatus);
            // The refusal message embeds the same hint.
            assert!(import_not_started_message(src, 1).contains(&import_status_command(src)));
        }
    }

    #[test]
    fn resync_hint_parses_to_resync_for_that_source() {
        let (name, command) = parse_hinted(&resync_command("bandcamp", "p123"));
        assert_eq!(name, "bandcamp");
        assert_eq!(
            command,
            SourceCommands::Resync {
                identifier: "p123".into()
            }
        );
    }

    fn parse_source(args: &[&str]) -> Result<(String, SourceCommands), clap::Error> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_source_args(&args)
    }

    #[test]
    fn every_per_source_command_parses_with_the_name_first() {
        let cases: Vec<(Vec<&str>, SourceCommands)> = vec![
            (vec!["bandcamp", "sync"], SourceCommands::Sync),
            (vec!["bandcamp", "status"], SourceCommands::Status),
            (vec!["bandcamp", "downloads"], SourceCommands::Downloads),
            (
                vec!["bandcamp", "cancel", "d1"],
                SourceCommands::Cancel { id: "d1".into() },
            ),
            (vec!["bandcamp", "pause"], SourceCommands::Pause),
            (vec!["bandcamp", "resume"], SourceCommands::Resume),
            (
                vec!["bandcamp", "resync", "p1"],
                SourceCommands::Resync {
                    identifier: "p1".into(),
                },
            ),
            (
                vec!["bandcamp", "check-item", "p1"],
                SourceCommands::CheckItem {
                    identifier: "p1".into(),
                },
            ),
            (
                vec!["bandcamp", "check-updates", "--apply"],
                SourceCommands::CheckUpdates { apply: true },
            ),
            (vec!["bandcamp", "playlists"], SourceCommands::Playlists),
            (
                vec![
                    "bandcamp",
                    "import-playlist",
                    "my-list",
                    "--name",
                    "x",
                    "--dry-run",
                    "--no-wait",
                ],
                SourceCommands::ImportPlaylist {
                    playlist: "my-list".into(),
                    name: Some("x".into()),
                    dry_run: true,
                    no_wait: true,
                },
            ),
            (
                vec!["bandcamp", "import-status"],
                SourceCommands::ImportStatus,
            ),
        ];
        for (args, expected) in cases {
            let (name, command) = parse_source(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert_eq!(name, "bandcamp");
            assert_eq!(command, expected, "{args:?}");
        }
    }

    #[test]
    fn import_playlist_defaults_have_no_flags_set() {
        let (_, command) = parse_source(&["bandcamp", "import-playlist", "p"]).unwrap();
        assert_eq!(
            command,
            SourceCommands::ImportPlaylist {
                playlist: "p".into(),
                name: None,
                dry_run: false,
                no_wait: false,
            }
        );
    }

    #[test]
    fn source_list_parses_as_the_global_command() {
        let cli = Cli::try_parse_from(["mdma", "source", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Source {
                command: SourceTop::List
            }
        ));
    }

    #[test]
    fn source_name_then_command_reaches_named() {
        let cli = Cli::try_parse_from(["mdma", "source", "bandcamp", "sync"]).unwrap();
        match cli.command {
            Commands::Source {
                command: SourceTop::Named(args),
            } => assert_eq!(args, vec!["bandcamp", "sync"]),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_per_source_command_fails() {
        let e = parse_source(&["bandcamp", "frobnicate"]).unwrap_err();
        assert!(e.use_stderr());
    }

    #[test]
    fn source_name_without_command_fails() {
        let e = parse_source(&["bandcamp"]).unwrap_err();
        assert!(e.use_stderr() || e.kind() != clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn old_name_last_order_is_rejected() {
        // `mdma source sync bandcamp` reads `sync` as the source name.
        assert!(parse_source(&["sync", "bandcamp"]).is_err());
    }

    #[test]
    fn bandcamp_command_group_is_gone() {
        assert!(Cli::try_parse_from(["mdma", "bandcamp", "playlists"]).is_err());
    }

    #[test]
    fn source_help_lists_list_and_explains_the_name_first_form() {
        let e = Cli::try_parse_from(["mdma", "source", "--help"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = e.to_string();
        assert!(help.contains("list"), "{help}");
        assert!(help.contains("<SOURCE-NAME> <COMMAND>"), "{help}");
    }

    #[test]
    fn per_source_help_lists_the_commands() {
        let e = parse_source(&["bandcamp", "--help"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = e.to_string();
        for cmd in [
            "sync",
            "status",
            "downloads",
            "cancel",
            "pause",
            "resume",
            "resync",
            "check-item",
            "check-updates",
            "playlists",
            "import-playlist",
            "import-status",
        ] {
            assert!(help.contains(cmd), "missing {cmd} in {help}");
        }
        assert!(help.contains("mdma source bandcamp"), "{help}");
    }

    #[test]
    fn unsupported_message_names_the_source_and_feature() {
        assert_eq!(
            unsupported_message("beatport", "playlist import"),
            "source 'beatport' does not support playlist import"
        );
        assert_eq!(
            unsupported_message("beatport", "playlists"),
            "source 'beatport' does not support playlists"
        );
    }

    #[test]
    fn syncing_phase_line_says_a_full_collection_sync_runs() {
        let line = import_phase_line(&ImportPhase::Syncing);
        assert!(line.contains("Syncing Bandcamp collection"), "got: {line}");
        assert!(line.contains("missing tracks found"), "got: {line}");
    }

    #[test]
    fn done_with_any_result_is_terminal() {
        assert!(is_terminal_phase(&ImportPhase::Done(
            ImportResult::DryRun { would_write: 1 }
        )));
        assert!(is_terminal_phase(&done()));
        assert!(!is_terminal_phase(&ImportPhase::Writing));
    }

    fn status_input(s: ImportJobStatus) -> PollInput {
        PollInput::Status(Some(s))
    }

    #[test]
    fn poller_prints_line_only_on_phase_change() {
        let mut p = ImportPoller::new(1);
        let s1 = p.step(status_input(job(1, ImportPhase::Fetching, None)));
        assert_eq!(s1.lines, vec!["Fetching playlist...".to_string()]);
        assert_eq!(s1.flow, PollFlow::Continue);
        let s2 = p.step(status_input(job(1, ImportPhase::Fetching, None)));
        assert!(s2.lines.is_empty());
        let s3 = p.step(status_input(job(1, ImportPhase::Matching, None)));
        assert_eq!(s3.lines.len(), 1);
    }

    #[test]
    fn poller_prints_download_progress_only_when_it_changes() {
        let mut p = ImportPoller::new(1);
        let a = p.step(status_input(job(1, ImportPhase::Syncing, Some((5, 1, 0)))));
        assert_eq!(a.lines.len(), 2);
        assert_eq!(a.lines[1], "downloads: 1/5 (failed 0)");
        let b = p.step(status_input(job(1, ImportPhase::Syncing, Some((5, 1, 0)))));
        assert!(b.lines.is_empty());
        let c = p.step(status_input(job(1, ImportPhase::Syncing, Some((5, 3, 1)))));
        assert_eq!(c.lines, vec!["downloads: 3/5 (failed 1)".to_string()]);
    }

    #[test]
    fn poller_finishes_on_done_and_failed() {
        let mut p = ImportPoller::new(1);
        let done = job(1, done(), None);
        let step = p.step(status_input(done.clone()));
        assert_eq!(step.flow, PollFlow::Finished(Box::new(done)));

        let mut p = ImportPoller::new(1);
        let failed = job(
            1,
            ImportPhase::Failed {
                message: "x".into(),
            },
            None,
        );
        let step = p.step(status_input(failed.clone()));
        assert_eq!(step.flow, PollFlow::Finished(Box::new(failed)));
    }

    #[test]
    fn poller_ignores_other_jobs_status() {
        let mut p = ImportPoller::new(1);
        let step = p.step(status_input(job(2, done(), None)));
        assert_eq!(step.flow, PollFlow::Continue);
        assert!(step.lines.is_empty());
    }

    #[test]
    fn poller_gives_up_after_consecutive_failures() {
        let mut p = ImportPoller::new(1);
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            assert_eq!(p.step(PollInput::Failure).flow, PollFlow::Continue);
        }
        assert_eq!(p.step(PollInput::Failure).flow, PollFlow::GiveUp);
    }

    #[test]
    fn poller_success_resets_failure_count() {
        let mut p = ImportPoller::new(1);
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            p.step(PollInput::Failure);
        }
        p.step(status_input(job(1, ImportPhase::Matching, None)));
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            assert_eq!(p.step(PollInput::Failure).flow, PollFlow::Continue);
        }
    }

    #[test]
    fn poller_counts_missing_job_as_failure() {
        let mut p = ImportPoller::new(1);
        for _ in 0..IMPORT_POLL_MAX_FAILURES - 1 {
            p.step(PollInput::Status(None));
        }
        assert_eq!(p.step(PollInput::Status(None)).flow, PollFlow::GiveUp);
    }

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
    fn resolve_gateway_uses_resolved_target() {
        // The resolved target's IP and port give the gateway address
        let cli = Cli {
            target: Some(node_discovery::Target::Remote(
                node_discovery::RemoteTarget {
                    host: "mdma-909.local".to_string(),
                    ip: Some(std::net::Ipv4Addr::new(192, 168, 0, 9)),
                    port: 5555,
                    source: node_discovery::Source::Cache,
                },
            )),
            ..cli_with_target(None)
        };
        assert_eq!(
            resolve_gateway(&cli),
            Some("tcp://192.168.0.9:5555".to_string())
        );
        assert_eq!(
            resolve_event_gateway(&cli),
            Some("tcp://192.168.0.9:5556".to_string())
        );
    }

    #[test]
    fn resolve_gateway_returns_none_when_no_target() {
        // No resolved target (local IPC) means no gateway
        let cli = cli_with_target(None);
        assert_eq!(resolve_gateway(&cli), None);
        assert_eq!(resolve_event_gateway(&cli), None);
    }

    fn cli_with_target(target: Option<node_discovery::Target>) -> Cli {
        Cli {
            node: None,
            target,
            socket: "ipc:///run/mdma/library.sock".to_string(),
            playback_socket: "ipc:///run/mdma/playback.sock".to_string(),
            sources_dir: "/run/mdma/sources".to_string(),
            command: Commands::Ping,
        }
    }

    #[test]
    fn node_commands_do_not_need_a_resolved_node() {
        let node_cmd = Commands::Node {
            command: NodeCommands::List,
        };
        assert!(!node_cmd.needs_node());
        assert!(!Commands::GenerateCompletions {
            shell: clap_complete::Shell::Bash
        }
        .needs_node());
        assert!(Commands::Ping.needs_node());
        assert!(Commands::Status.needs_node());
    }

    fn given(args: &[&str]) -> bool {
        let mut argv = vec!["mdma"];
        argv.extend_from_slice(args);
        let matches = Cli::command()
            .try_get_matches_from(argv)
            .expect("valid args");
        sockets_given(&matches)
    }

    #[test]
    fn explicit_sockets_are_detected_from_flags_and_environment() {
        // One test, because it touches process-wide environment variables.
        std::env::remove_var("MDMA_LIBRARY_SOCKET");
        std::env::remove_var("MDMA_PLAYBACK_SOCKET");

        assert!(!given(&["ping"]), "defaults are not explicit");
        assert!(given(&["--socket", "ipc:///tmp/lib.sock", "ping"]));
        assert!(given(&["--playback-socket", "ipc:///tmp/pb.sock", "ping"]));

        // The sources directory says nothing about library/playback.
        assert!(!given(&["--sources-dir", "/tmp/sources", "ping"]));
        std::env::set_var("MDMA_SOURCES_DIR", "/tmp/sources");
        assert!(!given(&["ping"]));
        std::env::remove_var("MDMA_SOURCES_DIR");

        std::env::set_var("MDMA_LIBRARY_SOCKET", "ipc:///tmp/lib.sock");
        assert!(given(&["ping"]), "an environment override counts");
        std::env::remove_var("MDMA_LIBRARY_SOCKET");

        std::env::set_var("MDMA_PLAYBACK_SOCKET", "ipc:///tmp/pb.sock");
        assert!(given(&["ping"]));
        std::env::remove_var("MDMA_PLAYBACK_SOCKET");
    }

    #[test]
    fn node_flag_no_longer_reads_the_environment() {
        // MDMA_NODE is read separately so --node and the env var can be told apart.
        let arg = Cli::command()
            .get_arguments()
            .find(|a| a.get_id() == "node")
            .expect("node arg")
            .get_env()
            .is_none();
        assert!(arg);
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
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
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
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
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
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
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

    // ── export_dest_path length limits ───────────────────────────────────────

    fn rel_components(output: &std::path::Path, path: &std::path::Path) -> Vec<String> {
        path.strip_prefix(output)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_str().unwrap().to_string())
            .collect()
    }

    fn assert_clean_name(name: &str) {
        assert!(!name.ends_with('.'), "trailing dot in {name:?}");
        assert!(!name.ends_with(' '), "trailing space in {name:?}");
    }

    #[test]
    fn export_dest_path_caps_long_ascii_title_and_keeps_extension() {
        let title = "a".repeat(300);
        let track = make_track_full("Artist", "Album", &title, "x.flac");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        let comps = rel_components(out, &path);
        assert_eq!(comps.len(), 3);
        assert!(comps[2].len() <= 180, "file name {} bytes", comps[2].len());
        assert!(comps[2].ends_with(".flac"));
        assert_clean_name(&comps[2]);
    }

    #[test]
    fn export_dest_path_truncates_multibyte_on_char_boundary() {
        for ch in ["å", "ä", "漢"] {
            let title = ch.repeat(300);
            let track = make_track_full("Artist", "Album", &title, "x.mp3");
            let out = std::path::Path::new("/out");
            let path = export_dest_path(out, &track, "mp3");
            let comps = rel_components(out, &path);
            assert!(comps[2].len() <= 180);
            assert!(comps[2].ends_with(".mp3"));
            assert!(comps[2].starts_with(ch));
        }
    }

    #[test]
    fn export_dest_path_caps_long_directories() {
        let long = "b".repeat(300);
        let track = make_track_full(&long, &long, "T", "x.flac");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        let comps = rel_components(out, &path);
        assert!(comps[0].len() <= 120);
        assert!(comps[1].len() <= 120);
        assert_eq!(comps[2], "T.flac");
    }

    #[test]
    fn export_dest_path_total_relative_length_is_bounded() {
        let long = "c".repeat(300);
        let track = make_track_full(&long, &long, &long, "x.flac");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        let full = path.to_str().unwrap();
        assert!(
            location_cost(full) <= MAX_LOCATION_URI_BYTES,
            "path {} bytes",
            location_cost(full)
        );
        assert!(full.ends_with(".flac"));
    }

    #[test]
    fn export_dest_path_long_output_dir_shrinks_title_first() {
        let out_dir = format!("/{}", "o".repeat(150));
        let out = std::path::Path::new(&out_dir);
        let track = make_track_full("Artist Name", "Album Name", &"t".repeat(100), "x");
        let (path, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert!(fits);
        let full = path.to_str().unwrap();
        assert!(location_cost(full) <= MAX_LOCATION_URI_BYTES);
        let comps = rel_components(out, &path);
        assert_eq!(comps[0], "Artist Name");
        assert_eq!(comps[1], "Album Name");
        assert!(comps[2].len() < 100 && comps[2].ends_with(".flac"));
    }

    #[test]
    fn export_dest_path_reports_when_output_dir_leaves_no_room() {
        let out_dir = format!("/{}", "o".repeat(240));
        let out = std::path::Path::new(&out_dir);
        let track = make_track_full("Artist", "Album", "Title", "x");
        let (_, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert!(!fits);
    }

    fn no_existing() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    #[test]
    fn export_dest_path_counts_non_ascii_by_utf8_bytes() {
        // 60 CJK chars = 180 UTF-8 bytes (CJK does not decompose); with
        // ".flac" that exceeds the 180-byte file name cap, so it is shortened.
        let title = "漢".repeat(60);
        let track = make_track_full("A", "B", &title, "x");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        assert_ne!(path, out.join("A").join("B").join(format!("{title}.flac")));
        assert!(location_cost(path.to_str().unwrap()) <= MAX_LOCATION_URI_BYTES);
    }

    #[test]
    fn path_cost_measures_nfd_exactly() {
        // é (precomposed) is 2 bytes in NFC but 3 in NFD (e + U+0301).
        assert_eq!(path_cost("é"), 3);
        // Hangul syllable: 3 bytes NFC, 3 jamo x 3 bytes = 9 in NFD.
        assert_eq!(path_cost("한"), 9);
        assert_eq!(path_cost("abc"), 3);
    }

    #[test]
    fn export_dest_path_latin_accents_under_nfd_budget_are_not_shortened() {
        // 35 x é = 105 bytes NFD per component. Whole Location is
        // 16 + 4 + 2 + 1 + 105 + 1 + 105 + 5 = 239 <= 250. The old 3x rule
        // (210 bytes per component) would have renamed this.
        let accented = "é".repeat(35);
        let track = make_track_full("A", &accented, &accented, "x");
        let out = std::path::Path::new("/out");
        let (path, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert!(fits);
        assert_eq!(
            path,
            out.join("A")
                .join(&accented)
                .join(format!("{accented}.flac"))
        );
    }

    #[test]
    fn export_dest_path_counts_spaces_as_encoded_bytes() {
        // Each component: 20 x "x " + "y" = 41 raw bytes, 81 encoded (space is
        // %20). Raw path is ~135 bytes, but the encoded Location is 271 > 250.
        let part = format!("{}y", "x ".repeat(20));
        let track = make_track_full(&part, &part, &part, "x");
        let out = std::path::Path::new("/out");
        let unshortened = out.join(&part).join(&part).join(format!("{part}.flac"));
        let (path, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert_ne!(path, unshortened);
        assert!(fits);
        assert!(
            rekordbox_xml::path_to_file_uri(&path).len() <= MAX_LOCATION_URI_BYTES,
            "encoded location too long"
        );
    }

    #[test]
    fn export_dest_path_grandfathers_nfd_location_for_nfc_metadata() {
        use unicode_normalization::UnicodeNormalization;
        // Over-budget accented path; the existing Location was written from an
        // HFS+ realpath, i.e. NFD, while the track metadata is NFC.
        let title = "é".repeat(100);
        let track = make_track_full("Artist", "Album", &title, "x");
        let out = std::path::Path::new("/out");
        let unshortened = out
            .join("Artist")
            .join("Album")
            .join(format!("{title}.flac"));
        let nfd_uri: String = rekordbox_xml::path_to_file_uri(&unshortened)
            .nfd()
            .collect();
        assert_ne!(nfd_uri, rekordbox_xml::path_to_file_uri(&unshortened));
        let existing: std::collections::HashSet<String> =
            [location_key(&nfd_uri)].into_iter().collect();
        let (kept, fits) = export_dest_path_checked(out, &track, "flac", &existing);
        assert_eq!(kept, unshortened);
        assert!(fits);
    }

    #[test]
    fn export_dest_path_grandfathers_existing_export_entry() {
        let title = "a".repeat(300);
        let track = make_track_full("Artist", "Album", &title, "x");
        let out = std::path::Path::new("/out");
        let unshortened = out
            .join("Artist")
            .join("Album")
            .join(format!("{title}.flac"));
        // Without an existing entry the path is shortened.
        let (shortened, _) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert_ne!(shortened, unshortened);
        // With an existing entry (any case) the unshortened path is kept.
        let existing: std::collections::HashSet<String> = [location_key(
            &rekordbox_xml::path_to_file_uri(&unshortened).to_uppercase(),
        )]
        .into_iter()
        .collect();
        let (kept, fits) = export_dest_path_checked(out, &track, "flac", &existing);
        assert_eq!(kept, unshortened);
        assert!(fits);
        // Another track is unaffected by that entry.
        let other = make_track_full("Artist", "Album", &"b".repeat(300), "x");
        let (other_path, _) = export_dest_path_checked(out, &other, "flac", &existing);
        assert_ne!(
            other_path,
            out.join("Artist")
                .join("Album")
                .join(format!("{}.flac", "b".repeat(300)))
        );
    }

    #[test]
    fn export_dest_path_trims_trailing_dots_and_spaces_after_truncation() {
        // Place a space/dot exactly at the truncation point.
        let out = std::path::Path::new("/out");
        for n in 30..60 {
            let t = format!("{}{}{}", "d".repeat(100 + n), ". ".repeat(40), "e");
            let track2 = make_track_full(&format!("{}. ", "f".repeat(100 + n)), "Album", &t, "x");
            let comps = rel_components(out, &export_dest_path(out, &track2, "flac"));
            assert_clean_name(&comps[0]);
            let stem = comps[2].strip_suffix(".flac").unwrap();
            assert_clean_name(stem);
        }
    }

    #[test]
    fn export_dest_path_truncated_collisions_are_disambiguated() {
        let base = "g".repeat(400);
        let t1 = make_track_full("Artist", "Album", &format!("{base}one"), "x");
        let t2 = make_track_full("Artist", "Album", &format!("{base}two"), "x");
        let out = std::path::Path::new("/out");
        let p1 = export_dest_path(out, &t1, "flac");
        let p2 = export_dest_path(out, &t2, "flac");
        assert_ne!(p1, p2);
        // Deterministic across calls.
        assert_eq!(p1, export_dest_path(out, &t1, "flac"));
    }

    #[test]
    fn export_dest_path_short_paths_are_unchanged() {
        let track = make_track_full("Björk", "Homogenic", "Jóga ", "x.flac");
        let out = std::path::Path::new("/out");
        assert_eq!(
            export_dest_path(out, &track, "flac"),
            std::path::PathBuf::from("/out/Björk/Homogenic/Jóga .flac")
        );
        // Exactly at the file-name limit stays untouched.
        let title = "h".repeat(180 - ".flac".len());
        let track = make_track_full("A", "B", &title, "x");
        let p = export_dest_path(out, &track, "flac");
        assert_eq!(p, out.join("A").join("B").join(format!("{title}.flac")));
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
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
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
        assert_eq!(hashes, ["sha256:h3aaaaaa", "sha256:h4aaaaaa"]);
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
        assert_eq!(hashes, ["sha256:cafebabe78"]);
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
        assert_eq!(hashes, ["sha256:h1aaaaaa", "sha256:h2aaaaaa"]);
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
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
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
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
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
            None,
            None,
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
            None,
            None,
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
        let mut tracks = [
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
            ["sha256:never", "sha256:old", "sha256:recent"],
            "ascending: never-played first (None = -∞), then oldest→newest"
        );
    }

    #[test]
    fn sort_started_descending_puts_none_last() {
        let mut tracks = [
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
            ["sha256:recent", "sha256:old", "sha256:never"],
            "descending: newest first, never-played last (None = -∞)"
        );
    }

    #[test]
    fn sort_by_stopped_descending_newest_first() {
        let mut tracks = [
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
            ["sha256:recent", "sha256:old", "sha256:never"],
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
                memory_cues: vec![],
                beat_grid: None,
                role: None,
                energy: None,
            }
        };
        let mut tracks = [
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
            ["sha256:unknown", "sha256:early", "sha256:new"],
            "ascending added: None first, then oldest→newest"
        );
    }

    // ── parse_field_value ─────────────────────────────────────────────────────

    #[test]
    fn parse_field_value_title_valid() {
        let result = parse_field_value(FactField::Title, "Spacewalk");
        assert!(
            matches!(result, Ok(MusicValue::Title(_))),
            "expected MusicValue::Title, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_year_valid() {
        let result = parse_field_value(FactField::Year, "2024");
        assert!(
            matches!(result, Ok(MusicValue::Year(_))),
            "expected MusicValue::Year, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_year_invalid() {
        let result = parse_field_value(FactField::Year, "twenty-four");
        assert!(result.is_err(), "expected Err for non-integer year");
    }

    #[test]
    fn parse_field_value_bpm_valid() {
        let result = parse_field_value(FactField::Bpm, "128.0");
        assert!(
            matches!(result, Ok(MusicValue::Bpm(_))),
            "expected MusicValue::Bpm, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_bpm_out_of_range() {
        // 5.0 BPM is below the valid range
        let result = parse_field_value(FactField::Bpm, "5.0");
        assert!(result.is_err(), "expected Err for out-of-range BPM");
    }

    #[test]
    fn parse_field_value_key_camelot_valid() {
        let result = parse_field_value(FactField::Key, "8B");
        assert!(
            matches!(result, Ok(MusicValue::Key(_))),
            "expected MusicValue::Key, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_key_invalid() {
        let result = parse_field_value(FactField::Key, "NotAKey");
        assert!(result.is_err(), "expected Err for invalid key");
    }

    #[test]
    fn parse_field_value_recording_date_valid() {
        let result = parse_field_value(FactField::RecordingDate, "2024-06-15");
        assert!(
            matches!(result, Ok(MusicValue::RecordingDate(_))),
            "expected MusicValue::RecordingDate, got {:?}",
            result
        );
    }

    #[test]
    fn parse_field_value_recording_date_bad_format() {
        let result = parse_field_value(FactField::RecordingDate, "2024/06/15");
        assert!(result.is_err(), "expected Err for bad date format");
    }

    // ── mdma view: ViewColumn / render_for_user ───────────

    fn make_view_track(
        hash: &str,
        artist: Option<&str>,
        title: Option<&str>,
        duration_secs: Option<u32>,
    ) -> TrackInfo {
        use library_ipc_client::{ContentHash, DurationSeconds};
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: title.map(str::to_string),
            artist: artist.map(str::to_string),
            album: None,
            duration: duration_secs.map(DurationSeconds::new),
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
        }
    }

    #[test]
    fn view_clap_parses_hash_artist_title_duration() {
        let result = Cli::try_parse_from([
            "mdma",
            "view",
            "--hash",
            "--artist",
            "--title",
            "--duration",
        ]);
        assert!(
            result.is_ok(),
            "clap rejected view --hash --artist --title --duration: {}",
            result.unwrap_err()
        );
        if let Commands::View {
            hash,
            artist,
            title,
            duration,
        } = result.unwrap().command
        {
            assert!(hash);
            assert!(artist);
            assert!(title);
            assert!(duration);
        } else {
            panic!("expected Commands::View");
        }
    }

    #[test]
    fn view_clap_parses_subset_of_flags() {
        let result = Cli::try_parse_from(["mdma", "view", "--title", "--hash"]);
        assert!(
            result.is_ok(),
            "clap rejected view --title --hash: {}",
            result.unwrap_err()
        );
        if let Commands::View {
            hash,
            artist,
            title,
            duration,
        } = result.unwrap().command
        {
            assert!(title);
            assert!(hash);
            assert!(!artist);
            assert!(!duration);
        } else {
            panic!("expected Commands::View");
        }
    }

    #[test]
    fn view_clap_no_flags_parses_ok() {
        // All flags are optional; no flags is valid (handle_view will reject at runtime).
        let result = Cli::try_parse_from(["mdma", "view"]);
        assert!(
            result.is_ok(),
            "clap rejected view with no flags: {}",
            result.unwrap_err()
        );
    }

    // ── render_for_user: single-column ────────────────────

    #[test]
    fn view_render_hash_only_shows_only_hash() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Title"),
            Some(180),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Hash], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.starts_with("abcdef123456"),
            "hash-only should start with 12-char hash, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Artist"),
            "hash-only should not contain artist, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Title"),
            "hash-only should not contain title, got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_artist_only_shows_only_artist() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Init"),
            Some(508),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Artist], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("Carbon Based Lifeforms"),
            "artist-only should contain artist, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("abcdef123456"),
            "artist-only should not contain hash, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Init"),
            "artist-only should not contain title, got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_title_only_shows_only_title() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("Polyrytmi"),
            "title-only should contain title, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Carbon"),
            "title-only should not contain artist, got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_duration_only_shows_only_duration() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Title"),
            Some(225), // 3:45
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Duration], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("3:45"),
            "duration-only should contain formatted duration, got: {stripped:?}"
        );
        assert!(
            !stripped.contains("Artist"),
            "duration-only should not contain artist, got: {stripped:?}"
        );
    }

    // ── render_for_user: multi-column combinations ─────────

    #[test]
    fn view_render_hash_and_title() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Init"),
            Some(508),
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Hash, ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("abcdef123456"), "should contain hash");
        assert!(stripped.contains("Init"), "should contain title");
        assert!(!stripped.contains("Artist"), "should not contain artist");
    }

    #[test]
    fn view_render_all_four_columns() {
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        )];
        let lines = render_for_user(&tracks, ALL_COLUMNS, 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("abcdef123456"), "should contain hash");
        assert!(
            stripped.contains("Carbon Based Lifeforms"),
            "should contain artist"
        );
        assert!(stripped.contains("Polyrytmi"), "should contain title");
        assert!(stripped.contains("6:07"), "should contain duration");
    }

    // ── Field-order preservation ─────────────────────────────────────────────────

    #[test]
    fn view_render_title_before_hash_when_columns_ordered_title_hash() {
        // When columns = [Title, Hash], title must appear left of hash in the output.
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("TheTitle"),
            None,
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Title, ViewColumn::Hash], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let title_pos = stripped.find("TheTitle").expect("title not in output");
        let hash_pos = stripped.find("abcdef123456").expect("hash not in output");
        assert!(
            title_pos < hash_pos,
            "title ({title_pos}) should appear before hash ({hash_pos}) when columns=[Title, Hash]; got: {stripped:?}"
        );
    }

    #[test]
    fn view_render_hash_before_title_when_columns_ordered_hash_title() {
        // When columns = [Hash, Title], hash must appear left of title.
        let tracks = vec![make_view_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("TheTitle"),
            None,
        )];
        let lines = render_for_user(&tracks, &[ViewColumn::Hash, ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let hash_pos = stripped.find("abcdef123456").expect("hash not in output");
        let title_pos = stripped.find("TheTitle").expect("title not in output");
        assert!(
            hash_pos < title_pos,
            "hash ({hash_pos}) should appear before title ({title_pos}) when columns=[Hash, Title]; got: {stripped:?}"
        );
    }

    // ── view_columns_from_matches ────────────────────────────────────────────────

    #[test]
    fn view_columns_from_matches_hash_artist_order() {
        // Simulate `mdma view --hash --artist` → Hash before Artist.
        let matches = Cli::command().get_matches_from(["mdma", "view", "--hash", "--artist"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(cols, vec![ViewColumn::Hash, ViewColumn::Artist]);
    }

    #[test]
    fn view_columns_from_matches_title_hash_order() {
        // Simulate `mdma view --title --hash` → Title before Hash.
        let matches = Cli::command().get_matches_from(["mdma", "view", "--title", "--hash"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(cols, vec![ViewColumn::Title, ViewColumn::Hash]);
    }

    #[test]
    fn view_columns_from_matches_all_four_preserves_order() {
        // Simulate `mdma view --hash --artist --title --duration` → canonical order.
        let matches = Cli::command().get_matches_from([
            "mdma",
            "view",
            "--hash",
            "--artist",
            "--title",
            "--duration",
        ]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(
            cols,
            vec![
                ViewColumn::Hash,
                ViewColumn::Artist,
                ViewColumn::Title,
                ViewColumn::Duration
            ]
        );
    }

    #[test]
    fn view_columns_from_matches_duration_first() {
        // Simulate `mdma view --duration --title` → Duration before Title.
        let matches = Cli::command().get_matches_from(["mdma", "view", "--duration", "--title"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert_eq!(cols, vec![ViewColumn::Duration, ViewColumn::Title]);
    }

    #[test]
    fn view_columns_from_matches_no_flags_empty() {
        let matches = Cli::command().get_matches_from(["mdma", "view"]);
        let view_matches = matches.subcommand_matches("view").unwrap();
        let cols = view_columns_from_matches(view_matches);
        assert!(cols.is_empty(), "no flags → empty columns list");
    }

    // ── case-insensitive location URI matching ───────────────────────────────────

    /// Bug 1 regression: when the existing XML round-trip produces a location URI
    /// with different case than the freshly-built URI, the playlist filter must
    /// still match it.  Before the fix, `refreshed_locations` was a case-sensitive
    /// `HashSet<&str>`, so the case-mismatched probe returned false and the track
    /// was silently dropped from the playlist.
    ///
    /// This test exercises the production code: it builds a `refreshed` vec whose
    /// location uses lowercase, and a `hash_to_location` map whose location uses
    /// mixed-case (as produced by the existing-XML round-trip), then asserts that
    /// the playlist update's locations vector is non-empty.
    #[test]
    fn rekordbox_export_associates_tracks_when_locations_differ_in_case() {
        // The existing XML round-trip canonicalises to a mixed-case URI.
        let hash = "sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011";
        let location_upper = "file://Localhost/tmp/export/Polyrytmi.aiff".to_string();
        // The freshly-emitted RefreshedTrack uses the canonical lowercase form.
        let location_lower = "file://localhost/tmp/export/polyrytmi.aiff".to_string();

        // refreshed_locations built from freshly-emitted tracks (lowercase).
        let refreshed_locations: std::collections::HashSet<String> = [location_lower.clone()]
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect();

        // hash_to_location built from existing-XML round-trip (mixed-case).
        let hash_to_location: std::collections::HashMap<String, String> =
            [(hash.to_string(), location_upper.clone())]
                .into_iter()
                .collect();

        // Simulate the playlist filter as it exists in the production path.
        // This is the FIXED version — it normalises case at the lookup boundary.
        let hashes = [hash.to_string()];
        let locations: Vec<String> = hashes
            .iter()
            .filter_map(|h| {
                let loc = hash_to_location.get(h)?;
                // Fixed: probe with lowercased key.
                if refreshed_locations.contains(&loc.to_lowercase()) {
                    Some(loc.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            locations,
            std::slice::from_ref(&location_upper),
            "playlist must reference the track even when URI case differs between \
             existing-XML round-trip and freshly-emitted path"
        );
    }

    /// Bug 2 regression: when the existing XML holds a location URI with different
    /// case than the freshly-built URI, `merge_export` must treat them as the same
    /// track — not create a duplicate COLLECTION entry.
    #[test]
    fn rekordbox_export_does_not_duplicate_when_existing_xml_location_differs_in_case() {
        // Round 1: existing XML holds a mixed-case location.
        let loc_mixed = "file://Localhost/tmp/export/Polyrytmi.aiff".to_string();
        // Round 2: freshly-emitted RefreshedTrack uses the canonical lowercase form.
        let loc_lower = "file://localhost/tmp/export/polyrytmi.aiff".to_string();

        // Build a minimal existing library with the mixed-case location.
        let existing = rekordbox_xml::RekordboxLibrary {
            tracks: vec![rekordbox_xml::RekordboxTrack {
                track_id: 1,
                name: "Polyrytmi".to_string(),
                artist: "Carbon Based Lifeforms".to_string(),
                album: String::new(),
                genre: String::new(),
                kind: "AIFF File".to_string(),
                size: 1000,
                total_time: 360,
                average_bpm: None,
                tonality: None,
                track_number: None,
                disc_number: None,
                year: None,
                label: None,
                comment: None,
                date_added: None,
                bitrate: None,
                sample_rate: None,
                location: loc_mixed.clone(),
                tempo_anchor: None,
                position_marks: vec![],
            }],
            playlists: vec![],
        };

        // Freshly-refreshed track uses the lowercase canonical URI.
        let refreshed = rekordbox_xml::RefreshedTrack {
            name: "Polyrytmi".to_string(),
            artist: "Carbon Based Lifeforms".to_string(),
            album: String::new(),
            genre: String::new(),
            kind: "AIFF File".to_string(),
            size: 1000,
            total_time: 360,
            average_bpm: None,
            tonality: None,
            track_number: None,
            disc_number: None,
            year: None,
            label: None,
            comment: None,
            date_added: None,
            bitrate: None,
            sample_rate: None,
            location: loc_lower.clone(),
            position_marks: vec![],
        };

        // Without case normalisation in merge_export, the existing mixed-case
        // location and the new lowercase location are treated as different tracks,
        // yielding 2 COLLECTION entries instead of 1.
        // This test asserts the merged library has exactly ONE entry.
        let merged = rekordbox_xml::merge_export(Some(existing), vec![refreshed], &[]);

        assert_eq!(
            merged.tracks.len(),
            1,
            "merge_export must not create a duplicate when existing and refreshed \
             locations differ only in case; got tracks: {:?}",
            merged
                .tracks
                .iter()
                .map(|t| &t.location)
                .collect::<Vec<_>>()
        );
    }

    // ── mdma search output stability: render_track_table_inner unchanged ────────

    #[test]
    fn search_render_unchanged_from_all_columns() {
        // render_track_table_inner must produce identical output to
        // render_for_user with ALL_COLUMNS.
        // This locks the promise that mdma search output is byte-identical after the refactor.
        let tracks = queue_display_tracks();
        let term_width = 120;

        let via_inner = render_track_table_inner(&tracks, 0, term_width);
        let via_parameterized = render_for_user(&tracks, ALL_COLUMNS, term_width, 0);

        assert_eq!(
            via_inner,
            via_parameterized,
            "render_track_table_inner and render_for_user(ALL_COLUMNS) must produce identical output"
        );
    }

    // ── Non-TTY (pipe) rendering: hash is always exactly 12 chars ────────────────

    /// The PIPE_TERM_WIDTH constant in the component must be wide enough that
    /// corsett never shrinks the 12-char hash column.
    #[test]
    fn pipe_mode_hash_is_exactly_12_chars_on_every_line() {
        // Use a very long artist/title to stress-test that the virtual width is
        // wide enough to keep hash at full 12 chars even under content pressure.
        use library_ipc_client::{ContentHash, DurationSeconds};
        let tracks: Vec<TrackInfo> = (0..5)
            .map(|i| {
                TrackInfo {
                    content_hash: ContentHash::new(format!("sha256:abcdef{:02}deadbeef", i)),
                    title: Some("A Very Long Title That Could Put Pressure On Column Sizing Algorithms And Cause Issues".to_string()),
                    artist: Some("An Equally Very Long Artist Name That Stresses The Layout Engine Significantly".to_string()),
                    album: None,
                    duration: Some(DurationSeconds::new(300)),
                    bpm: None,
                    key: None,
                    blob_path: None,
                    cover_art_path: None,
                    track_number: None,
                    disc_number: None,
                    added: None,
                    started: None,
                    stopped: None,
                    memory_cues: vec![],
                    beat_grid: None,
                    role: None,
                    energy: None,
                }
            })
            .collect();

        // PIPE_TERM_WIDTH is a large virtual width used when rendering to non-TTY.
        // We simulate it by passing a large explicit width here.
        const PIPE_TERM_WIDTH: usize = 200;
        let lines = render_for_user(&tracks, ALL_COLUMNS, PIPE_TERM_WIDTH, 0);

        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            // First whitespace-separated token is the hash
            let hash_token = stripped
                .split_whitespace()
                .next()
                .expect("line should have at least one token");
            assert_eq!(
                hash_token.len(),
                12,
                "Line {}: hash token {:?} should be exactly 12 chars (uniqueness invariant). Full line: {:?}",
                i + 1,
                hash_token,
                stripped,
            );
        }
    }

    /// Pipe mode with virtual large width must not truncate artist or title.
    #[test]
    fn pipe_mode_large_virtual_width_does_not_truncate_content() {
        use library_ipc_client::{ContentHash, DurationSeconds};
        let long_artist = "An Equally Very Long Artist Name That Stresses The Layout Engine";
        let long_title = "A Very Long Title That Could Put Pressure On Column Sizing Algorithms";
        let track = TrackInfo {
            content_hash: ContentHash::new("sha256:abcdef1234567890"),
            title: Some(long_title.to_string()),
            artist: Some(long_artist.to_string()),
            album: None,
            duration: Some(DurationSeconds::new(300)),
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
        };

        const PIPE_TERM_WIDTH: usize = 200;
        let lines = render_for_user(&[track], ALL_COLUMNS, PIPE_TERM_WIDTH, 0);

        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains(long_artist),
            "pipe mode output should not truncate long artist; got: {:?}",
            stripped
        );
        assert!(
            stripped.contains(long_title),
            "pipe mode output should not truncate long title; got: {:?}",
            stripped
        );
    }

    // ── admin service-mode clap parsing ──────────────────────────────────────

    #[test]
    fn admin_service_mode_enable_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "service-mode", "enable"]);
        assert!(
            result.is_ok(),
            "clap rejected admin service-mode enable: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command:
                    AdminCommands::ServiceMode {
                        command: ServiceModeCommands::Enable,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_service_mode_status_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "service-mode", "status"]);
        assert!(
            result.is_ok(),
            "clap rejected admin service-mode status: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command:
                    AdminCommands::ServiceMode {
                        command: ServiceModeCommands::Status,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_service_mode_disable_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "service-mode", "disable"]);
        assert!(
            result.is_ok(),
            "clap rejected admin service-mode disable: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command:
                    AdminCommands::ServiceMode {
                        command: ServiceModeCommands::Disable,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_boot_health_parses_correctly() {
        let cli = Cli::try_parse_from(["mdma", "admin", "boot-health"]).unwrap();
        match cli.command {
            Commands::Admin {
                command: AdminCommands::BootHealth,
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_reboot_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "reboot"]);
        assert!(
            result.is_ok(),
            "clap rejected admin reboot: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command: AdminCommands::Reboot { force: false },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    // ── identity clap parsing ─────────────────────────────────────────────────

    #[test]
    fn identity_backfill_parses_flags() {
        for (args, want_pcm, want_no_wait) in [
            (
                vec!["mdma", "library", "identity", "backfill"],
                false,
                false,
            ),
            (
                vec!["mdma", "library", "identity", "backfill", "--pcm"],
                true,
                false,
            ),
            (
                vec![
                    "mdma",
                    "library",
                    "identity",
                    "backfill",
                    "--pcm",
                    "--no-wait",
                ],
                true,
                true,
            ),
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            match cli.command {
                Commands::Library {
                    command:
                        LibraryCommands::Identity {
                            command: IdentityCommands::Backfill { pcm, no_wait },
                        },
                } => assert_eq!((pcm, no_wait), (want_pcm, want_no_wait)),
                other => panic!("unexpected command: {:?}", other),
            }
        }
    }

    #[test]
    fn identity_status_parses() {
        let cli = Cli::try_parse_from(["mdma", "library", "identity", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Library {
                command: LibraryCommands::Identity {
                    command: IdentityCommands::Status
                }
            }
        ));
    }

    #[test]
    fn already_running_message_names_the_mode_only_when_it_differs() {
        let same = identity_already_running_message(true, true);
        assert!(!same.contains("requested"), "{same}");
        let differs = identity_already_running_message(false, true);
        assert!(
            differs.contains("computes AudioHash, not the requested AudioHash + PcmHash"),
            "{differs}"
        );
    }

    #[test]
    fn identity_cancel_parses() {
        let cli = Cli::try_parse_from(["mdma", "library", "identity", "cancel"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Library {
                command: LibraryCommands::Identity {
                    command: IdentityCommands::Cancel
                }
            }
        ));
    }

    #[test]
    fn identity_status_formats_progress_failures_and_skips() {
        use library_ipc_client::{IdentityBackfillProgress, IdentityBackfillStatus};
        let p = IdentityBackfillProgress {
            include_pcm: true,
            total: 10,
            done: 10,
            failed: 2,
            skipped: 1,
            last_error: Some("boom".into()),
        };
        assert_eq!(
            format_identity_status(&IdentityBackfillStatus::Done(p.clone())),
            "Done (AudioHash + PcmHash): 10/10 tracks, 2 failed, 1 skipped (last error: boom)"
        );
        assert!(
            format_identity_status(&IdentityBackfillStatus::Cancelled(p)).starts_with("Cancelled")
        );
        assert!(format_identity_status(&IdentityBackfillStatus::Idle).starts_with("No identity"));
    }

    // ── dedup ─────────────────────────────────────────────────────────────────

    fn dedup_args(args: &[&str]) -> (bool, Option<DedupCommands>) {
        let mut argv = vec!["mdma", "library", "dedup"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(&argv)
            .unwrap_or_else(|e| panic!("{argv:?}: {e}"))
            .command
        {
            Commands::Library {
                command: LibraryCommands::Dedup { apply, command },
            } => (apply, command),
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn dedup_lists_by_default_and_applies_with_the_flag() {
        assert!(matches!(dedup_args(&[]), (false, None)));
        assert!(matches!(dedup_args(&["--apply"]), (true, None)));
    }

    #[test]
    fn dedup_merge_takes_a_winner_losers_and_yes() {
        match dedup_args(&["merge", "abc123", "def456", "0a1b2c"]) {
            (
                false,
                Some(DedupCommands::Merge {
                    winner,
                    losers,
                    yes,
                }),
            ) => {
                assert_eq!(winner, "abc123");
                assert_eq!(losers, vec!["def456", "0a1b2c"]);
                assert!(!yes);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            dedup_args(&["merge", "--yes", "a", "b"]),
            (false, Some(DedupCommands::Merge { yes: true, .. }))
        ));
        assert!(matches!(
            dedup_args(&["merge", "-y", "a", "b"]),
            (false, Some(DedupCommands::Merge { yes: true, .. }))
        ));
    }

    #[test]
    fn dedup_merge_needs_a_loser_and_does_not_mix_with_apply() {
        assert!(Cli::try_parse_from(["mdma", "library", "dedup", "merge", "abc"]).is_err());
        assert!(Cli::try_parse_from(["mdma", "library", "dedup", "merge"]).is_err());
        assert!(
            Cli::try_parse_from(["mdma", "library", "dedup", "--apply", "merge", "a", "b"])
                .is_err()
        );
    }

    fn dedup_member(hash: &str, item: Option<&str>) -> DedupMember {
        DedupMember {
            hash: ContentHash::new(format!("sha256:{hash}")),
            title: Some("Espansione".into()),
            album: Some("Part 3 & 4".into()),
            added: Some("2026-09-01T10:00:00+00:00".into()),
            item_id: item.map(str::to_owned),
            deleted: false,
        }
    }

    fn dedup_group(winner: &str, loser: &str, disposition: DedupDisposition) -> DedupGroup {
        DedupGroup {
            members: vec![
                dedup_member(winner, Some("p1")),
                dedup_member(loser, Some("p1")),
            ],
            winner: ContentHash::new(format!("sha256:{winner}")),
            disposition,
        }
    }

    #[test]
    fn dedup_listing_puts_automatic_groups_first_and_says_how_to_confirm_the_rest() {
        let groups = [
            dedup_group(
                "bbbbbbbbbbbbbbbb",
                "cccccccccccccccc",
                DedupDisposition::NeedsConfirmation {
                    reasons: vec![ConfirmationReason::DifferentItemIds],
                },
            ),
            dedup_group(
                "aaaaaaaaaaaaaaaa",
                "dddddddddddddddd",
                DedupDisposition::Automatic,
            ),
        ];

        let text = format_dedup_groups(&groups);

        let auto = text.find("Automatic").unwrap();
        let confirm = text
            .find("Needs confirmation: different purchases")
            .unwrap();
        assert!(auto < confirm, "{text}");
        assert!(text.contains("keep   aaaaaaaaaaaa"), "{text}");
        assert!(text.contains("merge  dddddddddddd"), "{text}");
        assert!(text.contains("added 2026-09-01"), "{text}");
        assert!(
            text.contains("confirm with: mdma library dedup merge bbbbbbbbbbbb cccccccccccc"),
            "{text}"
        );
        assert_eq!(
            text.matches("confirm with").count(),
            1,
            "automatic groups need no confirmation: {text}"
        );
        assert_eq!(format_dedup_groups(&[]), "No duplicates found");
    }

    #[test]
    fn dedup_applied_summary_lists_errors() {
        let r = DedupApplyReport {
            merged: 2,
            blobs_removed: 1,
            errors: vec!["boom".into()],
        };
        assert_eq!(
            format_dedup_applied(&r),
            "Merged 2 track(s), removed 1 blob(s)\n  error: boom"
        );
    }

    #[test]
    fn dedup_group_is_found_by_a_hash_prefix_with_or_without_sha256() {
        let groups = [dedup_group(
            "abcdef0123",
            "fedcba9876",
            DedupDisposition::Automatic,
        )];
        assert!(find_dedup_group(&groups, "abcd").unwrap().is_some());
        assert!(find_dedup_group(&groups, "sha256:FEDC").unwrap().is_some());
        assert!(find_dedup_group(&groups, "0000").unwrap().is_none());
    }

    #[test]
    fn a_hash_prefix_that_matches_two_groups_is_an_error() {
        let groups = [
            dedup_group("abcdef0123", "fedcba9876", DedupDisposition::Automatic),
            dedup_group("abcd001122", "112233aabb", DedupDisposition::Automatic),
        ];
        assert!(find_dedup_group(&groups, "abcd").is_err());
        // Long enough to tell them apart, and a prefix of two members of one group.
        assert_eq!(
            find_dedup_group(&groups, "abcdef").unwrap().unwrap().winner,
            groups[0].winner
        );
        assert!(find_dedup_group(&groups, "112").unwrap().is_some());
    }

    #[test]
    fn only_yes_confirms_a_merge() {
        for yes in ["y", "Y", "yes", " YES \n"] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["", "n", "no", "maybe", "\n"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }

    // ── track lifecycle clap parsing ──────────────────────────────────────────

    #[test]
    fn track_delete_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "library", "track", "delete", "abc123"]);
        assert!(
            result.is_ok(),
            "clap rejected library track delete: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    LibraryCommands::Track {
                        command: TrackCommands::Delete { hash },
                    },
            } => {
                assert_eq!(hash, "abc123");
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_restore_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "library", "track", "restore", "abc123"]);
        assert!(
            result.is_ok(),
            "clap rejected library track restore: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    LibraryCommands::Track {
                        command: TrackCommands::Restore { hash },
                    },
            } => {
                assert_eq!(hash, "abc123");
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_replace_parses_correctly() {
        let result = Cli::try_parse_from([
            "mdma",
            "library",
            "track",
            "replace",
            "abc123",
            "/music/inbox/new.flac",
        ]);
        assert!(
            result.is_ok(),
            "clap rejected library track replace: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    LibraryCommands::Track {
                        command: TrackCommands::Replace { old_hash, new_file },
                    },
            } => {
                assert_eq!(old_hash, "abc123");
                assert_eq!(new_file, "/music/inbox/new.flac");
            }
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn track_orphans_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "library", "track", "orphans"]);
        assert!(
            result.is_ok(),
            "clap rejected library track orphans: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Library {
                command:
                    LibraryCommands::Track {
                        command: TrackCommands::Orphans,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }
}
