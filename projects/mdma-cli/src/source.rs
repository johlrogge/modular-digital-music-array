mod import;

use crate::args::sanitized_name_or_exit;
use crate::ctx::Ctx;
use crate::output::handle_source_error;
use crate::source::import::finish_import;
use crate::source::import::follow_import_job;
use crate::source::import::is_terminal_phase;
use clap::{Parser, Subcommand};
use color_eyre::Result;
use mdma_client::SourceClient;
use source_protocol::{SourceError, SourceRequest, SourceResponse};

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
    match &command {
        Command::List => handle_source_list(ctx),
        Command::Named(args) => match parse_source_args(args) {
            Ok((name, command)) => run_source_command(ctx, &name, &command),
            Err(e) => exit_source_parse_error(e),
        },
    }
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

/// Parse the arguments captured by [`Command::Named`]: `<name> <command> ...`.
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

/// Request timeout for `mdma source playlists`: listing may make several
/// rate-limited upstream requests (e.g. Bandcamp), well beyond the default 5s.
const SOURCE_LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

// =============================================================================
// Source Command Handlers
// =============================================================================

fn handle_source_list(ctx: &Ctx) -> Result<()> {
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
    ctx: &Ctx,
    source_client: &SourceClient,
    name: &str,
    apply: bool,
) -> Result<()> {
    // Fetch all ItemId values from the library for this source.
    // We use GetFactValues("ItemId") — returns all ItemId values the library knows about.
    let lib = ctx.library();
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

/// Run one per-source command against the source `name`.
fn run_source_command(ctx: &Ctx, name: &str, command: &SourceCommands) -> Result<()> {
    match command {
        SourceCommands::Playlists => {
            let client = ctx.source(name, Some(SOURCE_LISTING_TIMEOUT));
            handle_source_playlists(&client, name)
        }
        _ => {
            let client = ctx.source(name, None);
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
                    handle_source_check_updates(ctx, &client, name, *apply)
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

// Adopted by a later chunk (per-command arg types); unused until then.
#[allow(dead_code)]
/// Name of a music source (not to be confused with `mdma_client::SourceName`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceName(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;
    use color_eyre::Result;

    use crate::Cli;

    use pretty_assertions::assert_eq;

    use super::Command as SourceTop;
    use crate::Commands;

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
}
