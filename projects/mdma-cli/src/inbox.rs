use crate::ctx::Ctx;
use crate::output::{library_error, success_unless};
use color_eyre::eyre::eyre;
use color_eyre::Result;
use hicli::{Arg, Success};
use library_ipc_client::{ConversionState, InboxConversion, InboxPath, InboxPathError};
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

    // What the conversion worker says of the files it holds (AIFF, ADR-037). A node
    // that cannot say is not an error: the list is still the list.
    let conversions = library.status().map(|s| s.conversions).unwrap_or_default();
    // Finished files are gone from the inbox: they are listed apart.
    let finished: Vec<&InboxConversion> = conversions.iter().filter(|c| is_done(c)).collect();

    if files.is_empty() && finished.is_empty() {
        println!("Inbox is empty");
        return Ok(());
    }

    if files.is_empty() {
        println!("Inbox is empty");
    } else {
        println!("Inbox queue ({} files):", files.len());
        println!("{}", "=".repeat(65));
        for file in files {
            println!("{}", list_line(&file, &conversions));
        }
    }
    if !finished.is_empty() {
        println!("Recently converted:");
        for c in finished {
            println!("  {}  [{}]", c.path.as_str(), c.state);
        }
    }
    Ok(())
}

fn is_done(conversion: &InboxConversion) -> bool {
    matches!(conversion.state, ConversionState::Done { .. })
}

/// One line of the inbox list: the file, and its conversion state if the worker
/// holds it. (A finished one is not in the inbox: a file of its name there now is a
/// different one.)
fn list_line(file: &InboxPath, conversions: &[InboxConversion]) -> String {
    match conversions.iter().find(|c| c.path == *file && !is_done(c)) {
        Some(c) => format!("  {}  [{}]", file.as_str(), c.state),
        None => format!("  {}", file.as_str()),
    }
}

/// How often `ingest --wait` asks the worker.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Where a file's conversion is, from the worker's status.
#[derive(Debug, PartialEq, Eq)]
enum Progress<'a> {
    /// Queued, converting or waiting to retry (or in a state this client does not know).
    Working,
    /// Stored (`Done`) or given up on (`Failed`).
    Ended(&'a ConversionState),
    /// The worker does not hold it (a restart, or an outcome pushed out by newer ones).
    Unknown,
}

fn progress<'a>(conversions: &'a [InboxConversion], file: &InboxPath) -> Progress<'a> {
    match conversions.iter().find(|c| c.path == *file) {
        Some(InboxConversion {
            state: state @ (ConversionState::Done { .. } | ConversionState::Failed { .. }),
            ..
        }) => Progress::Ended(state),
        Some(_) => Progress::Working,
        None => Progress::Unknown,
    }
}

/// Poll `conversions` (the worker's status) until the worker is done with `file`,
/// and print how it ended.
fn wait_for_conversion(
    conversions: impl Fn() -> Result<Vec<InboxConversion>>,
    file: &InboxPath,
) -> Result<()> {
    loop {
        let conversions = conversions()?;
        match progress(&conversions, file) {
            Progress::Working => std::thread::sleep(POLL),
            Progress::Ended(ConversionState::Done { hash, warnings }) => {
                println!("Success: {}", hash.as_str());
                for warning in warnings {
                    println!("Warning: {}", warning);
                }
                return Ok(());
            }
            Progress::Ended(state) => return Err(eyre!("{}", state)),
            Progress::Unknown => {
                return Err(eyre!(
                    "The library no longer reports {}; see `mdma inbox list`",
                    file.as_str()
                ))
            }
        }
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

/// Wait for a conversion in the background (an AIFF) to end, and show how it ended
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "wait")]
pub(crate) struct Wait(pub bool);

/// Ingest a specific file from inbox
#[hicli::command]
pub(crate) fn ingest(wait: Wait, file: InboxFileArg, ctx: &Ctx) -> Result<()> {
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
        IngestResult::Pending { message } => {
            println!("Pending: {}", message);
            if wait.0 {
                wait_for_conversion(
                    || Ok(library.status().map_err(library_error)?.conversions),
                    &path,
                )?;
            }
        }
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
    fn a_finished_conversion_is_not_the_state_of_a_file_in_the_inbox() {
        use super::list_line;
        use library_ipc_client::{ConversionState, InboxConversion};
        let done = InboxConversion {
            path: InboxPath::new("a.aiff").unwrap(),
            state: ConversionState::Done {
                hash: library_ipc_client::ContentHash::new("sha256:abc"),
                warnings: Vec::new(),
            },
        };
        assert_eq!(
            list_line(&InboxPath::new("a.aiff").unwrap(), &[done]),
            "  a.aiff"
        );
    }

    #[test]
    fn wait_follows_the_workers_status_to_an_end() {
        use super::{progress, Progress};
        use library_ipc_client::{ContentHash, ConversionState, InboxConversion};
        let file = InboxPath::new("a.aiff").unwrap();
        let held = |state| {
            vec![InboxConversion {
                path: file.clone(),
                state,
            }]
        };
        for working in [
            ConversionState::Queued,
            ConversionState::Converting,
            ConversionState::Retrying {
                attempt: 1,
                last_error: "no flac".into(),
            },
            // Decoded from a newer node's status: keep polling.
            serde_json::from_str(r#"{"state":"hashing"}"#).unwrap(),
        ] {
            assert_eq!(progress(&held(working), &file), Progress::Working);
        }
        let done = ConversionState::Done {
            hash: ContentHash::new("sha256:abc"),
            warnings: Vec::new(),
        };
        let failed = ConversionState::Failed {
            message: "gave up".into(),
        };
        assert_eq!(progress(&held(done.clone()), &file), Progress::Ended(&done));
        assert_eq!(
            progress(&held(failed.clone()), &file),
            Progress::Ended(&failed)
        );
        assert_eq!(progress(&[], &file), Progress::Unknown);
    }

    #[test]
    fn ingest_takes_a_wait_switch() {
        assert!(parse(&["inbox", "ingest", "--wait", "a.aiff"]).is_ok());
        assert!(parse(&["inbox", "ingest", "a.aiff", "--wait"]).is_ok());
        assert!(parse(&["inbox", "ingest", "a.aiff"]).is_ok());
    }

    #[test]
    fn valid_inbox_file_is_accepted() {
        for sub in ["delete", "ingest"] {
            assert!(parse(&["inbox", sub, "dir/track.mp3"]).is_ok());
        }
    }
}
