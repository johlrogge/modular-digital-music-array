mod import;

use crate::args::{impl_arg, SanitizedPlaylistName};
use crate::ctx::Ctx;
use crate::output::{display_error, interpret_library, interpret_source, source_label};
use crate::source::import::{finish_import, follow_import_job, is_terminal_phase};
use bpaf::Bpaf;
use color_eyre::eyre::eyre;
use color_eyre::Result;
use hicli::Arg;
use mdma_client::SourceClient;
use source_protocol::{SourceError, SourceRequest, SourceResponse};
use std::convert::Infallible;
use std::str::FromStr;

/// The music source a per-source command runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceArg(pub mdma_client::SourceName);

impl FromStr for SourceArg {
    type Err = Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(mdma_client::SourceName::new(s)))
    }
}

impl_arg!(SourceArg, positional "SOURCE-NAME", "Music source name");

impl SourceArg {
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Whether `check-updates` resyncs the stale items it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplyUpdates(pub bool);

impl_arg!(
    ApplyUpdates,
    switch "apply",
    "Auto-apply resync for any stale items found"
);

/// A download of a source, as listed by `downloads`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DownloadArg(pub source_protocol::DownloadId);

impl FromStr for DownloadArg {
    type Err = Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(source_protocol::DownloadId::new(s)))
    }
}

impl_arg!(DownloadArg, positional "ID", "Download ID to cancel");
impl_arg!(ItemIdArg, positional "IDENTIFIER", "Source-specific item identifier");
impl_arg!(SourcePlaylistArg, positional "PLAYLIST", "Playlist slug or URL");

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

// `playlist` takes its help from its `Arg` impl.
#[derive(Debug, PartialEq, Bpaf, Arg)]
pub(crate) struct ImportPlaylistArgs {
    /// MDMA playlist name (default: sanitised source title)
    #[bpaf(argument("NAME"))]
    name: Option<SanitizedPlaylistName>,

    /// Show the report without syncing or writing the playlist
    dry_run: bool,

    /// Print the job id and exit once the import has started
    no_wait: bool,

    #[bpaf(external(SourcePlaylistArg::parser))]
    playlist: SourcePlaylistArg,
}

/// Request timeout for `mdma source playlists`: listing may make several
/// rate-limited upstream requests (e.g. Bandcamp), well beyond the default 5s.
const SOURCE_LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// List available music sources
#[hicli::command]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let gateway = ctx.gateway()?;
    let sources = mdma_client::list_available_sources(gateway.as_deref(), ctx.sources_dir())
        .map_err(display_error)?;
    if sources.is_empty() {
        println!("No sources available");
    } else {
        for name in sources {
            println!("{}", name);
        }
    }
    Ok(())
}

/// Send `request` to the source `name` and interpret the reply with
/// [`interpret_source`]; error responses are reported as they are.
fn source_call<T>(
    source: &SourceClient,
    name: &str,
    request: &SourceRequest,
    accept: impl FnOnce(SourceResponse) -> Option<T>,
) -> Result<T> {
    source_call_with(source, name, request, display_error, accept)
}

/// Like [`source_call`], with `on_error` deciding how an error response is reported.
fn source_call_with<T>(
    source: &SourceClient,
    name: &str,
    request: &SourceRequest,
    on_error: impl FnOnce(SourceError) -> color_eyre::Report,
    accept: impl FnOnce(SourceResponse) -> Option<T>,
) -> Result<T> {
    interpret_source(name, source.request(name, request), on_error, accept)
}

/// The failure for an item the source does not have.
fn item_not_found(source: &str, identifier: &str) -> color_eyre::Report {
    eyre!(
        "Item {} not found in your {} collection",
        identifier,
        source
    )
}

/// Report an item-level source error: `ItemNotFound` names the item and source.
fn item_error(source: &str, err: SourceError) -> color_eyre::Report {
    match err {
        SourceError::ItemNotFound { identifier } => item_not_found(source, &identifier),
        other => display_error(other),
    }
}

/// Sync the source (download new items)
#[hicli::command(curried)]
pub(crate) fn sync(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
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
                } => Some((total_items, new_items)),
                _ => None,
            },
        )?;
    println!("Sync started for {}", name);
    println!("Total items: {}, New items: {}", total_items, new_items);
    Ok(())
}

/// Show source status
#[hicli::command(curried)]
pub(crate) fn status(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    let status = source_call(
        &source,
        name,
        &SourceRequest::GetStatus,
        |response| match response {
            SourceResponse::Status(status) => Some(status),
            _ => None,
        },
    )?;
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

/// List downloads for the source
#[hicli::command(curried)]
pub(crate) fn downloads(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    let downloads =
        source_call(
            &source,
            name,
            &SourceRequest::ListDownloads,
            |response| match response {
                SourceResponse::Downloads(downloads) => Some(downloads),
                _ => None,
            },
        )?;
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

/// Cancel a download
#[hicli::command(curried)]
pub(crate) fn cancel(source: SourceArg, id: DownloadArg, ctx: &Ctx) -> Result<()> {
    let DownloadArg(id) = id;
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    let request = SourceRequest::CancelDownload { id: id.clone() };
    source_call(&source, name, &request, |response| match response {
        SourceResponse::Cancelled { .. } => Some(()),
        _ => None,
    })?;
    println!("Cancelled download: {}", id);
    Ok(())
}

/// Pause all downloads for the source
#[hicli::command(curried)]
pub(crate) fn pause(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    source_call(
        &source,
        name,
        &SourceRequest::PauseAll,
        |response| match response {
            SourceResponse::Paused => Some(()),
            _ => None,
        },
    )?;
    println!("Downloads paused");
    Ok(())
}

/// Resume downloads for the source
#[hicli::command(curried)]
pub(crate) fn resume(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    source_call(
        &source,
        name,
        &SourceRequest::ResumeAll,
        |response| match response {
            SourceResponse::Resumed => Some(()),
            _ => None,
        },
    )?;
    println!("Downloads resumed");
    Ok(())
}

/// Force re-sync of a specific item (bypasses dedup)
#[hicli::command(curried)]
pub(crate) fn resync(source: SourceArg, identifier: ItemIdArg, ctx: &Ctx) -> Result<()> {
    let ItemIdArg(identifier) = identifier;
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    println!("Requesting resync of {} from {}...", identifier, name);
    let request = SourceRequest::ResyncItem { identifier };
    let (identifier, tracks_queued) = source_call_with(
        &source,
        name,
        &request,
        |e| item_error(name, e),
        |response| match response {
            SourceResponse::ResyncQueued {
                identifier,
                tracks_queued,
            } => Some((identifier, tracks_queued)),
            _ => None,
        },
    )?;
    println!(
        "Queued resync for {} ({} item queued)",
        identifier, tracks_queued
    );
    Ok(())
}

/// Check if a specific item has changed upstream
#[hicli::command(curried)]
pub(crate) fn check_item(source: SourceArg, identifier: ItemIdArg, ctx: &Ctx) -> Result<()> {
    let ItemIdArg(identifier) = identifier;
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    let request = SourceRequest::CheckItem { identifier };
    let (identifier, live_track_count, stored_track_count, stale) = source_call_with(
        &source,
        name,
        &request,
        |e| item_error(name, e),
        |response| match response {
            SourceResponse::ItemChecked {
                identifier,
                live_track_count,
                stored_track_count,
                stale,
            } => Some((identifier, live_track_count, stored_track_count, stale)),
            _ => None,
        },
    )?;
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

/// Check the whole source collection for stale items
#[hicli::command(curried)]
pub(crate) fn check_updates(source: SourceArg, apply: ApplyUpdates, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let apply = apply.0;
    let source = ctx.source(name, None)?;
    // Fetch all ItemId values from the library for this source.
    // We use GetFactValues("ItemId") — returns all ItemId values the library knows about.
    let library = ctx.library()?;
    let all_item_ids = interpret_library(
        library.request(&library_ipc_client::LibraryRequest::GetFactValues {
            fact_type: library_ipc_client::FactType::new("ItemId"),
        }),
        |response| match response {
            library_ipc_client::LibraryResponse::FactValues(values) => Some(values),
            _ => None,
        },
    )?;

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
    format!("{} does not support {}", source_label(source), feature)
}

/// The failure for a source error; `Unsupported` gets a friendly message naming the source.
fn playlist_source_error(source: &str, feature: &str, err: &SourceError) -> color_eyre::Report {
    if matches!(err, SourceError::Unsupported { .. }) {
        return eyre!("{}", unsupported_message(source, feature));
    }
    display_error(err)
}

/// List the source's playlists
///
/// Only for sources that support playlists.
#[hicli::command(curried)]
pub(crate) fn playlists(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, Some(SOURCE_LISTING_TIMEOUT))?;
    let playlists = source_call_with(
        &source,
        name,
        &SourceRequest::ListPlaylists,
        |e| playlist_source_error(name, "playlists", &e),
        |response| match response {
            SourceResponse::Playlists(playlists) => Some(playlists),
            _ => None,
        },
    )?;
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
/// Import a source playlist as an MDMA playlist
///
/// Definitive matches only. Only for sources that support playlists.
#[hicli::command(curried)]
pub(crate) fn import_playlist(
    source: SourceArg,
    args: ImportPlaylistArgs,
    ctx: &Ctx,
) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
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
    let job_id = source_call_with(
        &source,
        name,
        &request,
        |e| match e {
            // The running job may be a different playlist or a dry run, so
            // following it would report someone else's result as ours.
            SourceError::ImportInProgress { job_id } => {
                eyre!("{}", import_not_started_message(name, job_id))
            }
            e => playlist_source_error(name, "playlist import", &e),
        },
        |response| match response {
            SourceResponse::ImportStarted { job_id } => Some(job_id),
            _ => None,
        },
    )?;
    println!("Import started (job {})", job_id);

    if no_wait {
        println!("Check progress with '{}'", import_status_command(name));
        return Ok(());
    }
    follow_import_job(&source, name, job_id)
}

/// Show the current or last playlist import job, following it while running
///
/// Only for sources that support playlists.
#[hicli::command(curried)]
pub(crate) fn import_status(source: SourceArg, ctx: &Ctx) -> Result<()> {
    let name = source.as_str();
    let source = ctx.source(name, None)?;
    let status = source_call_with(
        &source,
        name,
        &SourceRequest::GetImportStatus,
        |e| playlist_source_error(name, "playlist import", &e),
        |response| match response {
            SourceResponse::ImportStatus(status) => Some(status),
            _ => None,
        },
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{help, parses, run_arg, usage_error};

    #[test]
    fn source_positional_arg_parsers_read_one_word() {
        assert_eq!(
            run_arg::<SourceArg>(&["bandcamp"]),
            Ok(SourceArg(mdma_client::SourceName::new("bandcamp")))
        );
        assert_eq!(
            run_arg::<DownloadArg>(&["d-1"]),
            Ok(DownloadArg(source_protocol::DownloadId::new("d-1")))
        );
        assert_eq!(
            run_arg::<ItemIdArg>(&["p123456"]),
            Ok(ItemIdArg("p123456".to_string()))
        );
        assert_eq!(
            run_arg::<SourcePlaylistArg>(&["my-slug"]),
            Ok(SourcePlaylistArg("my-slug".to_string()))
        );
        assert!(run_arg::<SourceArg>(&[]).is_err());
        assert!(run_arg::<DownloadArg>(&[]).is_err());
        assert!(run_arg::<ItemIdArg>(&[]).is_err());
        assert!(run_arg::<SourcePlaylistArg>(&[]).is_err());
    }

    #[test]
    fn apply_updates_arg_parser_is_a_switch() {
        assert_eq!(run_arg::<ApplyUpdates>(&[]), Ok(ApplyUpdates(false)));
        assert_eq!(
            run_arg::<ApplyUpdates>(&["--apply"]),
            Ok(ApplyUpdates(true))
        );
        assert!(run_arg::<ApplyUpdates>(&["--aply"]).is_err());
    }

    use playlist_import::sanitize_playlist_name;
    use pretty_assertions::assert_eq;

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
        assert_eq!(
            "p123".parse::<ItemIdArg>().unwrap(),
            ItemIdArg("p123".to_string())
        );
    }

    #[test]
    fn refused_import_message_says_not_started_and_names_the_running_job() {
        assert_eq!(
            import_not_started_message("bandcamp", 7),
            "Import NOT started: job 7 is already running (see 'mdma source bandcamp import-status')"
        );
    }

    /// The path a hinted command line reaches, as a user pasting it would.
    fn hinted_path(command: &str) -> Vec<&'static str> {
        let words: Vec<&str> = command.split_whitespace().collect();
        assert_eq!(&words[..2], ["mdma", "source"], "{command}");
        parses(&words[1..])
    }

    #[test]
    fn import_status_hint_parses_to_import_status_for_that_source() {
        for src in ["bandcamp", "beatport"] {
            assert_eq!(
                hinted_path(&import_status_command(src)),
                ["source", "import-status"]
            );
            // The refusal message embeds the same hint.
            assert!(import_not_started_message(src, 1).contains(&import_status_command(src)));
        }
    }

    #[test]
    fn resync_hint_parses_to_resync_for_that_source() {
        assert_eq!(
            hinted_path(&resync_command("bandcamp", "p123")),
            ["source", "resync"]
        );
    }

    #[test]
    fn every_per_source_command_parses_with_the_name_first() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            (vec!["bandcamp", "sync"], "sync"),
            (vec!["bandcamp", "status"], "status"),
            (vec!["bandcamp", "downloads"], "downloads"),
            (vec!["bandcamp", "cancel", "d1"], "cancel"),
            (vec!["bandcamp", "pause"], "pause"),
            (vec!["bandcamp", "resume"], "resume"),
            (vec!["bandcamp", "resync", "p1"], "resync"),
            (vec!["bandcamp", "check-item", "p1"], "check-item"),
            (
                vec!["bandcamp", "check-updates", "--apply"],
                "check-updates",
            ),
            (vec!["bandcamp", "playlists"], "playlists"),
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
                "import-playlist",
            ),
            (vec!["bandcamp", "import-status"], "import-status"),
        ];
        for (args, command) in cases {
            let mut argv = vec!["source"];
            argv.extend(&args);
            assert_eq!(
                parses(&argv),
                ["source", command],
                "{args:?} (curried: the path leaves the source name out)"
            );
        }
    }

    #[test]
    fn import_playlist_args_are_read_in_any_order() {
        assert_eq!(
            run_arg::<ImportPlaylistArgs>(&["my-list", "--name", "x", "--dry-run", "--no-wait"]),
            Ok(ImportPlaylistArgs {
                playlist: SourcePlaylistArg("my-list".into()),
                name: Some("x".parse().unwrap()),
                dry_run: true,
                no_wait: true,
            })
        );
    }

    #[test]
    fn import_playlist_defaults_have_no_flags_set() {
        assert_eq!(
            run_arg::<ImportPlaylistArgs>(&["p"]),
            Ok(ImportPlaylistArgs {
                playlist: SourcePlaylistArg("p".into()),
                name: None,
                dry_run: false,
                no_wait: false,
            })
        );
    }

    #[test]
    fn import_playlist_name_is_sanitized_at_parse_time() {
        let args = run_arg::<ImportPlaylistArgs>(&["p", "--name", "a/b"]).unwrap();
        let name = args.name.expect("name given");
        assert!(name.change_note().is_some());
        assert_eq!(name.name().as_str(), sanitize_playlist_name("a/b"));
    }

    #[test]
    fn import_playlist_name_that_sanitizes_to_empty_is_a_usage_error() {
        let e = usage_error(&["source", "bandcamp", "import-playlist", "p", "--name", ""]);
        assert!(e.contains("name sanitizes to empty"), "{e}");
    }

    #[test]
    fn source_list_parses_as_the_global_command() {
        assert_eq!(parses(&["source", "list"]), ["source", "list"]);
    }

    #[test]
    fn source_name_then_command_reaches_the_command() {
        assert_eq!(parses(&["source", "bandcamp", "sync"]), ["source", "sync"]);
    }

    #[test]
    fn unknown_per_source_command_fails() {
        usage_error(&["source", "bandcamp", "frobnicate"]);
    }

    #[test]
    fn source_name_without_command_fails() {
        let e = usage_error(&["source", "bandcamp"]);
        assert!(e.contains("expected a command after `bandcamp`"), "{e}");
    }

    #[test]
    fn misspelled_per_source_command_gets_a_suggestion() {
        let e = usage_error(&["source", "bandcamp", "snyc"]);
        assert!(
            e.contains("no such command after `bandcamp`: `snyc`, did you mean `sync`?"),
            "{e}"
        );
    }

    #[test]
    fn old_name_last_order_is_rejected() {
        // `mdma source sync bandcamp` reads `sync` as the source name.
        usage_error(&["source", "sync", "bandcamp"]);
    }

    #[test]
    fn bandcamp_command_group_is_gone() {
        usage_error(&["bandcamp", "playlists"]);
    }

    #[test]
    fn source_help_lists_list_and_explains_the_name_first_form() {
        let help = help(&["source", "--help"]);
        assert!(help.contains("list"), "{help}");
        assert!(help.contains("SOURCE-NAME"), "{help}");
    }

    #[test]
    fn per_source_help_lists_the_commands() {
        // `source bandcamp --help` shows the group page (accepted).
        let help = help(&["source", "bandcamp", "--help"]);
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

    // ── source error messages ───────────────────────────────────────────────────

    use crate::test_support::failure_text;

    #[test]
    fn an_item_not_found_error_names_the_item_and_source() {
        let err = SourceError::ItemNotFound {
            identifier: "p1".to_string(),
        };
        assert_eq!(
            failure_text::<()>(Err(item_error("bandcamp", err))),
            "Item p1 not found in your bandcamp collection"
        );
    }

    #[test]
    fn another_item_error_is_reported_as_it_is() {
        let err = SourceError::NotAuthenticated {
            message: "login first".to_string(),
        };
        assert_eq!(
            failure_text::<()>(Err(item_error("bandcamp", err))),
            "not authenticated: login first"
        );
    }

    #[test]
    fn an_unsupported_source_error_names_the_source_and_feature() {
        let err = SourceError::unsupported("ListPlaylists");
        assert_eq!(
            failure_text::<()>(Err(playlist_source_error("beatport", "playlists", &err))),
            "source 'beatport' does not support playlists"
        );
    }

    #[test]
    fn another_playlist_source_error_is_reported_as_it_is() {
        let err = SourceError::Internal {
            message: "boom".to_string(),
        };
        assert_eq!(
            failure_text::<()>(Err(playlist_source_error("beatport", "playlists", &err))),
            "internal error: boom"
        );
    }
}
