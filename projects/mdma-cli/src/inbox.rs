use crate::ctx::Ctx;
use crate::output::{library_error, success_unless};
use color_eyre::eyre::eyre;
use color_eyre::Result;
use hicli::{Arg, Success};
use library_ipc_client::{InboxConversion, InboxPath, InboxPathError};
use std::str::FromStr;
use track_formatter::short_hash;

/// File name in inbox
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "FILENAME")]
pub(crate) struct InboxFileArg(pub InboxPath);

impl FromStr for InboxFileArg {
    type Err = InboxPathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        InboxPath::new(s).map(Self)
    }
}

/// List files in inbox
#[hicli::command]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let files = library.inbox_queue().map_err(library_error)?;
    if files.is_empty() {
        println!("Inbox is empty");
        return Ok(());
    }

    // What the conversion worker says of the files it holds (AIFF, ADR-037). A node
    // that cannot say is not an error: the list is still the list.
    let conversions = library.status().map(|s| s.conversions).unwrap_or_default();

    println!("Inbox queue ({} files):", files.len());
    println!("{}", "=".repeat(65));

    for file in files {
        println!("{}", list_line(&file, &conversions));
    }
    Ok(())
}

/// One line of the inbox list: the file, and its conversion state if the worker
/// holds it.
fn list_line(file: &InboxPath, conversions: &[InboxConversion]) -> String {
    match conversions.iter().find(|c| c.path == *file) {
        Some(c) => format!("  {}  [{}]", file.as_str(), c.state),
        None => format!("  {}", file.as_str()),
    }
}

/// Delete a file from inbox without ingesting
#[hicli::command]
pub(crate) fn delete(file: InboxFileArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let InboxFileArg(path) = file;

    use mdma_client::IngestResult;
    match library.delete_inbox_file(&path).map_err(library_error)? {
        IngestResult::Success { message, .. } | IngestResult::Pending { message } => {
            println!("{}", message)
        }
        IngestResult::Failure { message } => return Err(ingest_failed(&message)),
    }
    Ok(())
}

/// The failure for a file the library could not ingest or delete.
fn ingest_failed(message: &str) -> color_eyre::Report {
    eyre!("Failed: {}", message)
}

/// Ingest a specific file from inbox
#[hicli::command]
pub(crate) fn ingest(file: InboxFileArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let InboxFileArg(path) = file;

    println!("Ingesting: {}", path.as_str());

    use mdma_client::IngestResult;
    match library.ingest_file(&path).map_err(library_error)? {
        IngestResult::Success {
            hash,
            message,
            warnings,
        } => {
            if let Some(h) = hash {
                println!("Success: {}", h.as_str());
            } else {
                println!("Success: {}", message);
            }
            for warning in warnings {
                println!("Warning: {}", warning);
            }
        }
        IngestResult::Pending { message } => println!("Pending: {}", message),
        IngestResult::Failure { message } => return Err(ingest_failed(&message)),
    }
    Ok(())
}

/// Ingest all files in inbox
#[hicli::command]
pub(crate) fn ingest_all(ctx: &Ctx) -> Result<Success> {
    let library = ctx.node().library()?;
    println!("Ingesting all files in inbox...");

    let results = library.ingest_all().map_err(library_error)?;
    if results.is_empty() {
        println!("Inbox is empty - nothing to ingest");
        return Ok(Success::OK);
    }

    let mut success_count = 0;
    let mut pending_count = 0;
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
            IngestResult::Pending { message } => {
                pending_count += 1;
                println!("  PENDING: {} - {}", item.path.as_str(), message);
            }
            IngestResult::Failure { message } => {
                fail_count += 1;
                println!("  FAIL: {} - {}", item.path.as_str(), message);
            }
        }
    }

    println!();
    println!(
        "Done: {} succeeded, {} pending, {} failed",
        success_count, pending_count, fail_count
    );

    Ok(success_unless(fail_count > 0))
}

#[cfg(test)]
mod tests {
    use super::InboxFileArg;
    use crate::test_support::{parse, run_arg, usage_error};
    use library_ipc_client::InboxPath;

    #[test]
    fn inbox_file_arg_parser_validates_the_path() {
        assert_eq!(
            run_arg::<InboxFileArg>(&["song.mp3"]),
            Ok(InboxFileArg(InboxPath::new("song.mp3").unwrap()))
        );
        let reason = InboxPath::new("../escape.mp3").unwrap_err().to_string();
        let err = run_arg::<InboxFileArg>(&["../escape.mp3"]).unwrap_err();
        assert!(err.contains(&reason), "{err}");
        assert!(run_arg::<InboxFileArg>(&[]).is_err());
    }

    #[test]
    fn invalid_inbox_file_is_a_usage_error() {
        for sub in ["delete", "ingest"] {
            for bad in ["/abs/file.mp3", "../escape.mp3"] {
                usage_error(&["inbox", sub, bad]);
            }
        }
    }

    #[test]
    fn the_list_shows_the_conversion_state_of_the_files_the_worker_holds() {
        use super::list_line;
        use library_ipc_client::{ConversionState, InboxConversion};
        let held = InboxConversion {
            path: InboxPath::new("a.aiff").unwrap(),
            state: ConversionState::Retrying {
                attempt: 2,
                last_error: "no flac".into(),
            },
        };
        assert_eq!(
            list_line(
                &InboxPath::new("a.aiff").unwrap(),
                std::slice::from_ref(&held)
            ),
            "  a.aiff  [conversion failed (attempt 2), retrying: no flac]"
        );
        assert_eq!(
            list_line(&InboxPath::new("b.mp3").unwrap(), &[held]),
            "  b.mp3"
        );
    }

    #[test]
    fn valid_inbox_file_is_accepted() {
        for sub in ["delete", "ingest"] {
            assert!(parse(&["inbox", sub, "dir/track.mp3"]).is_ok());
        }
    }
}
