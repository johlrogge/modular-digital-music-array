mod import;

use crate::args::SanitizedPlaylistName;
use crate::ctx::Ctx;
use crate::output::handle_source_error;
use crate::source::import::{finish_import, follow_import_job, is_terminal_phase};
use clap::{Parser, Subcommand};
use color_eyre::Result;
use mdma_client::SourceClient;
use source_protocol::{SourceError, SourceRequest, SourceResponse};
use std::convert::Infallible;
use std::str::FromStr;

/// Top level of `mdma source`: the global `list`, or `<source-name> <command>`.
///
/// `<source-name>` comes first, so it cannot be a positional of the group
/// (it would collide with `list`). Anything that is not `list` is captured as
/// an external subcommand and re-parsed as a [`SourceInvocation`], which keeps
/// full clap help and validation for the per-source commands. `list` is
/// therefore reserved and cannot be used as a source name.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// List available music sources
    List,

    /// `<SOURCE-NAME> <COMMAND>`: run a command on a source (see `mdma source <SOURCE-NAME> --help`)
    #[command(external_subcommand)]
    Named(Vec<String>),
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::List => list(ctx),
        Command::Named(args) => match parse_source_args(&args) {
            Ok((source, command)) => run_source_command(source, command, ctx),
            Err(e) => exit_source_parse_error(e),
        },
    }
}

/// The music source a per-source command runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceArg(pub mdma_client::SourceName);

impl FromStr for SourceArg {
    type Err = Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(mdma_client::SourceName::new(s)))
    }
}

impl SourceArg {
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Whether `check-updates` resyncs the stale items it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplyUpdates(pub bool);

/// A download of a source, as listed by `downloads`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DownloadArg(pub source_protocol::DownloadId);

impl FromStr for DownloadArg {
    type Err = Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(source_protocol::DownloadId::new(s)))
    }
}

/// A source-specific item identifier (e.g. "p123456" for bandcamp).
// Stays a String: no domain rule, the source interprets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemIdArg(pub String);

impl FromStr for ItemIdArg {
    type Err = Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

/// A source playlist to import: a slug or a URL.
// Stays a String: no domain rule, the source interprets it (slug or URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourcePlaylistArg(pub String);

impl FromStr for SourcePlaylistArg {
    type Err = Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

#[derive(clap::Args, Debug, PartialEq)]
pub(crate) struct ImportPlaylistArgs {
    /// MDMA playlist name (default: sanitised source title)
    #[arg(long)]
    name: Option<SanitizedPlaylistName>,

    /// Show the report without syncing or writing the playlist
    #[arg(long)]
    dry_run: bool,

    /// Print the job id and exit once the import has started
    #[arg(long)]
    no_wait: bool,

    /// Playlist slug or URL
    playlist: SourcePlaylistArg,
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
    command: SourceCommand,
}

#[derive(Subcommand, Debug, PartialEq)]
enum SourceCommand {
    /// Sync the source (download new items)
    Sync,

    /// Show source status
    Status,

    /// List downloads for the source
    Downloads,

    /// Cancel a download
    Cancel {
        /// Download ID to cancel
        id: DownloadArg,
    },

    /// Pause all downloads for the source
    Pause,

    /// Resume downloads for the source
    Resume,

    /// Force re-sync of a specific item (bypasses dedup)
    Resync {
        /// Source-specific item identifier (e.g. "p123456" for bandcamp)
        identifier: ItemIdArg,
    },

    /// Check if a specific item has changed upstream
    CheckItem {
        /// Source-specific item identifier
        identifier: ItemIdArg,
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
        #[command(flatten)]
        args: ImportPlaylistArgs,
    },

    /// Show the current or last playlist import job, following it while running
    /// (for sources that support playlists)
    ImportStatus,
}

/// Parse the arguments captured by [`Command::Named`]: `<name> <command> ...`.
fn parse_source_args(args: &[String]) -> Result<(SourceArg, SourceCommand), clap::Error> {
    let (name, rest) = args
        .split_first()
        .expect("external subcommand always has a name");
    let argv = std::iter::once(format!("mdma source {name}")).chain(rest.iter().cloned());
    let invocation = SourceInvocation::try_parse_from(argv)?;
    let source = name.parse::<SourceArg>().unwrap_or_else(|e| match e {});
    Ok((source, invocation.command))
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

/// Request timeout for `mdma source playlists`: listing may make several
/// rate-limited upstream requests (e.g. Bandcamp), well beyond the default 5s.
const SOURCE_LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let gateway = ctx.gateway();
    let sources = match mdma_client::list_available_sources(gateway.as_deref(), ctx.sources_dir()) {
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

/// Send `request` to the source `name` and hand the response to `accept`. A
/// response it declines (an error response or an unexpected one) or a failed
/// request is reported and exits 1. The declined response is boxed because
/// `SourceResponse` is large.
fn source_call<T>(
    source: &SourceClient,
    name: &str,
    request: &SourceRequest,
    accept: impl FnOnce(SourceResponse) -> std::result::Result<T, Box<SourceResponse>>,
) -> T {
    match source.request(name, request) {
        Ok(response) => accept(response).unwrap_or_else(|declined| match *declined {
            SourceResponse::Error(e) => handle_source_error(&e),
            _ => handle_source_error(&"Unexpected response"),
        }),
        Err(e) => handle_source_error(&e),
    }
}

/// Report an item the source does not have and exit 1.
fn item_not_found(identifier: &str, source: &str) -> ! {
    eprintln!(
        "Item {} not found in your {} collection",
        identifier, source
    );
    std::process::exit(1);
}

pub(crate) fn sync(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    println!("Syncing {}...", name);
    let (total_items, new_items) =
        source_call(
            &source,
            name,
            &SourceRequest::Sync,
            |response| match response {
                SourceResponse::SyncStarted {
                    total_items,
                    new_items,
                } => Ok((total_items, new_items)),
                other => Err(Box::new(other)),
            },
        );
    println!("Sync started for {}", name);
    println!("Total items: {}, New items: {}", total_items, new_items);
    Ok(())
}

pub(crate) fn status(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    let status = source_call(
        &source,
        name,
        &SourceRequest::GetStatus,
        |response| match response {
            SourceResponse::Status(status) => Ok(status),
            other => Err(Box::new(other)),
        },
    );
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

pub(crate) fn downloads(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    let downloads =
        source_call(
            &source,
            name,
            &SourceRequest::ListDownloads,
            |response| match response {
                SourceResponse::Downloads(downloads) => Ok(downloads),
                other => Err(Box::new(other)),
            },
        );
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

pub(crate) fn cancel(source: SourceArg, id: DownloadArg, ctx: &Ctx) -> Result<()> {
    let DownloadArg(id) = id;
    let name = source.as_str();
    let source = ctx.source(name, None);
    let request = SourceRequest::CancelDownload { id: id.clone() };
    source_call(&source, name, &request, |response| match response {
        SourceResponse::Cancelled { .. } => Ok(()),
        other => Err(Box::new(other)),
    });
    println!("Cancelled download: {}", id);
    Ok(())
}

pub(crate) fn pause(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    source_call(
        &source,
        name,
        &SourceRequest::PauseAll,
        |response| match response {
            SourceResponse::Paused => Ok(()),
            other => Err(Box::new(other)),
        },
    );
    println!("Downloads paused");
    Ok(())
}

pub(crate) fn resume(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    source_call(
        &source,
        name,
        &SourceRequest::ResumeAll,
        |response| match response {
            SourceResponse::Resumed => Ok(()),
            other => Err(Box::new(other)),
        },
    );
    println!("Downloads resumed");
    Ok(())
}

pub(crate) fn resync(source: SourceArg, identifier: ItemIdArg, ctx: &Ctx) -> Result<()> {
    let ItemIdArg(identifier) = identifier;
    let name = source.as_str();
    let source = ctx.source(name, None);
    println!("Requesting resync of {} from {}...", identifier, name);
    let request = SourceRequest::ResyncItem { identifier };
    let (identifier, tracks_queued) =
        source_call(&source, name, &request, |response| match response {
            SourceResponse::ResyncQueued {
                identifier,
                tracks_queued,
            } => Ok((identifier, tracks_queued)),
            SourceResponse::Error(SourceError::ItemNotFound { identifier }) => {
                item_not_found(&identifier, name)
            }
            other => Err(Box::new(other)),
        });
    println!(
        "Queued resync for {} ({} item queued)",
        identifier, tracks_queued
    );
    Ok(())
}

pub(crate) fn check_item(source: SourceArg, identifier: ItemIdArg, ctx: &Ctx) -> Result<()> {
    let ItemIdArg(identifier) = identifier;
    let name = source.as_str();
    let source = ctx.source(name, None);
    let request = SourceRequest::CheckItem { identifier };
    let (identifier, live_track_count, stored_track_count, stale) =
        source_call(&source, name, &request, |response| match response {
            SourceResponse::ItemChecked {
                identifier,
                live_track_count,
                stored_track_count,
                stale,
            } => Ok((identifier, live_track_count, stored_track_count, stale)),
            SourceResponse::Error(SourceError::ItemNotFound { identifier }) => {
                item_not_found(&identifier, name)
            }
            other => Err(Box::new(other)),
        });
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

pub(crate) fn check_updates(source: SourceArg, apply: ApplyUpdates, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let apply = apply.0;
    let source = ctx.source(name, None);
    // Fetch all ItemId values from the library for this source.
    // We use GetFactValues("ItemId") — returns all ItemId values the library knows about.
    let library = ctx.library();
    let all_item_ids = match library.request(&library_ipc_client::LibraryRequest::GetFactValues {
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

        match source.request(
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
            match source.request(
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

pub(crate) fn playlists(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, Some(SOURCE_LISTING_TIMEOUT));
    let playlists =
        source_call(
            &source,
            name,
            &SourceRequest::ListPlaylists,
            |response| match response {
                SourceResponse::Playlists(playlists) => Ok(playlists),
                SourceResponse::Error(e) => handle_playlist_source_error(name, "playlists", &e),
                other => Err(Box::new(other)),
            },
        );
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

/// The command hinted at to follow an import job (name first, as parsed).
pub(crate) fn import_status_command(source: &str) -> String {
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

// Import requests return immediately (the job runs server-side), so the
// default request timeout is enough for start and polls.
pub(crate) fn import_playlist(
    source: SourceArg,
    args: ImportPlaylistArgs,
    ctx: &Ctx,
) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    let ImportPlaylistArgs {
        name: playlist_name,
        dry_run,
        no_wait,
        playlist,
    } = args;
    // --name was validated and sanitised at parse time
    let explicit_name = playlist_name.map(|n| {
        if let Some(note) = n.change_note() {
            eprintln!("{note}");
        }
        n.name().as_str().to_string()
    });

    let SourcePlaylistArg(playlist) = playlist;
    let request = SourceRequest::ImportPlaylist {
        playlist,
        name: explicit_name,
        dry_run,
    };
    let job_id = source_call(&source, name, &request, |response| match response {
        SourceResponse::ImportStarted { job_id } => Ok(job_id),
        SourceResponse::Error(SourceError::ImportInProgress { job_id }) => {
            // The running job may be a different playlist or a dry run, so
            // following it would report someone else's result as ours.
            eprintln!("{}", import_not_started_message(name, job_id));
            std::process::exit(1);
        }
        SourceResponse::Error(e) => handle_playlist_source_error(name, "playlist import", &e),
        other => Err(Box::new(other)),
    });
    println!("Import started (job {})", job_id);

    if no_wait {
        println!("Check progress with '{}'", import_status_command(name));
        return Ok(());
    }
    follow_import_job(&source, name, job_id)
}

pub(crate) fn import_status(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None);
    let status =
        source_call(
            &source,
            name,
            &SourceRequest::GetImportStatus,
            |response| match response {
                SourceResponse::ImportStatus(status) => Ok(status),
                SourceResponse::Error(e) => {
                    handle_playlist_source_error(name, "playlist import", &e)
                }
                other => Err(Box::new(other)),
            },
        );
    match status {
        None => {
            println!("No import job");
            Ok(())
        }
        Some(status) if is_terminal_phase(&status.phase) => finish_import(&status),
        Some(status) => {
            println!("Import job {} is running", status.job_id);
            follow_import_job(&source, name, status.job_id)
        }
    }
}

/// Run one per-source command against the source `name`.
fn run_source_command(source: SourceArg, command: SourceCommand, ctx: &Ctx) -> Result<()> {
    match command {
        SourceCommand::Sync => sync(source, ctx),
        SourceCommand::Status => status(source, ctx),
        SourceCommand::Downloads => downloads(source, ctx),
        SourceCommand::Cancel { id } => cancel(source, id, ctx),
        SourceCommand::Pause => pause(source, ctx),
        SourceCommand::Resume => resume(source, ctx),
        SourceCommand::Resync { identifier } => resync(source, identifier, ctx),
        SourceCommand::CheckItem { identifier } => check_item(source, identifier, ctx),
        SourceCommand::CheckUpdates { apply } => check_updates(source, ApplyUpdates(apply), ctx),
        SourceCommand::Playlists => playlists(source, ctx),
        SourceCommand::ImportPlaylist { args } => import_playlist(source, args, ctx),
        SourceCommand::ImportStatus => import_status(source, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::Result;

    use crate::test_support::{parse, parses, usage_error};

    use playlist_import::sanitize_playlist_name;
    use pretty_assertions::assert_eq;

    use crate::Commands;

    #[test]
    fn source_value_args_parse_any_text() {
        assert_eq!(
            "bandcamp".parse::<SourceArg>().unwrap().as_str(),
            "bandcamp"
        );
        assert_eq!(
            "d1".parse::<DownloadArg>().unwrap(),
            DownloadArg(source_protocol::DownloadId::new("d1"))
        );
        assert_eq!("p123".parse::<ItemIdArg>().unwrap(), item("p123"));
    }

    #[test]
    fn refused_import_message_says_not_started_and_names_the_running_job() {
        assert_eq!(
            import_not_started_message("bandcamp", 7),
            "Import NOT started: job 7 is already running (see 'mdma source bandcamp import-status')"
        );
    }

    /// Parse a hinted command line with the real parser, as a user pasting it would.
    fn parse_hinted(command: &str) -> (SourceArg, SourceCommand) {
        let words: Vec<&str> = command.split_whitespace().collect();
        assert_eq!(&words[..2], ["mdma", "source"], "{command}");
        match parses(&words[1..]) {
            Commands::Source {
                command: Command::Named(args),
            } => parse_source_args(&args).unwrap_or_else(|e| panic!("{command}: {e}")),
            other => panic!("{command}: unexpected {other:?}"),
        }
    }

    #[test]
    fn import_status_hint_parses_to_import_status_for_that_source() {
        for src in ["bandcamp", "beatport"] {
            let (name, command) = parse_hinted(&import_status_command(src));
            assert_eq!(name.as_str(), src);
            assert_eq!(command, SourceCommand::ImportStatus);
            // The refusal message embeds the same hint.
            assert!(import_not_started_message(src, 1).contains(&import_status_command(src)));
        }
    }

    #[test]
    fn resync_hint_parses_to_resync_for_that_source() {
        let (name, command) = parse_hinted(&resync_command("bandcamp", "p123"));
        assert_eq!(name.as_str(), "bandcamp");
        assert_eq!(
            command,
            SourceCommand::Resync {
                identifier: item("p123")
            }
        );
    }

    fn item(s: &str) -> ItemIdArg {
        ItemIdArg(s.to_string())
    }

    fn parse_source(args: &[&str]) -> Result<(SourceArg, SourceCommand), clap::Error> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_source_args(&args)
    }

    #[test]
    fn every_per_source_command_parses_with_the_name_first() {
        let cases: Vec<(Vec<&str>, SourceCommand)> = vec![
            (vec!["bandcamp", "sync"], SourceCommand::Sync),
            (vec!["bandcamp", "status"], SourceCommand::Status),
            (vec!["bandcamp", "downloads"], SourceCommand::Downloads),
            (
                vec!["bandcamp", "cancel", "d1"],
                SourceCommand::Cancel {
                    id: DownloadArg(source_protocol::DownloadId::new("d1")),
                },
            ),
            (vec!["bandcamp", "pause"], SourceCommand::Pause),
            (vec!["bandcamp", "resume"], SourceCommand::Resume),
            (
                vec!["bandcamp", "resync", "p1"],
                SourceCommand::Resync {
                    identifier: item("p1"),
                },
            ),
            (
                vec!["bandcamp", "check-item", "p1"],
                SourceCommand::CheckItem {
                    identifier: item("p1"),
                },
            ),
            (
                vec!["bandcamp", "check-updates", "--apply"],
                SourceCommand::CheckUpdates { apply: true },
            ),
            (vec!["bandcamp", "playlists"], SourceCommand::Playlists),
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
                SourceCommand::ImportPlaylist {
                    args: ImportPlaylistArgs {
                        playlist: SourcePlaylistArg("my-list".into()),
                        name: Some("x".parse().unwrap()),
                        dry_run: true,
                        no_wait: true,
                    },
                },
            ),
            (
                vec!["bandcamp", "import-status"],
                SourceCommand::ImportStatus,
            ),
        ];
        for (args, expected) in cases {
            let (name, command) = parse_source(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert_eq!(name.as_str(), "bandcamp");
            assert_eq!(command, expected, "{args:?}");
        }
    }

    #[test]
    fn import_playlist_defaults_have_no_flags_set() {
        let (_, command) = parse_source(&["bandcamp", "import-playlist", "p"]).unwrap();
        assert_eq!(
            command,
            SourceCommand::ImportPlaylist {
                args: ImportPlaylistArgs {
                    playlist: SourcePlaylistArg("p".into()),
                    name: None,
                    dry_run: false,
                    no_wait: false,
                },
            }
        );
    }

    #[test]
    fn import_playlist_name_is_sanitized_at_parse_time() {
        let (_, command) =
            parse_source(&["bandcamp", "import-playlist", "p", "--name", "a/b"]).unwrap();
        match command {
            SourceCommand::ImportPlaylist { args } => {
                let name = args.name.expect("name given");
                assert!(name.change_note().is_some());
                assert_eq!(name.name().as_str(), sanitize_playlist_name("a/b"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn import_playlist_name_that_sanitizes_to_empty_is_a_usage_error() {
        let e = parse_source(&["bandcamp", "import-playlist", "p", "--name", ""]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::ValueValidation);
        assert_eq!(e.exit_code(), 2);
        assert!(e.to_string().contains("name sanitizes to empty"), "{e}");
    }

    #[test]
    fn source_list_parses_as_the_global_command() {
        assert!(matches!(
            parses(&["source", "list"]),
            Commands::Source {
                command: Command::List
            }
        ));
    }

    #[test]
    fn source_name_then_command_reaches_named() {
        match parses(&["source", "bandcamp", "sync"]) {
            Commands::Source {
                command: Command::Named(args),
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
        usage_error(&["bandcamp", "playlists"]);
    }

    #[test]
    fn source_help_lists_list_and_explains_the_name_first_form() {
        let e = parse(&["source", "--help"]).unwrap_err();
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
}
