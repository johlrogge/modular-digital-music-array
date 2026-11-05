use crate::args::impl_arg;
use crate::ctx::Ctx;
use crate::output::handle_error;
use color_eyre::Result;
use library_ipc_client::{InboxPath, InboxPathError};
use std::str::FromStr;
use track_formatter::short_hash;

/// File name in the inbox, validated by `InboxPath` at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboxFileArg(pub InboxPath);

impl FromStr for InboxFileArg {
    type Err = InboxPathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        InboxPath::new(s).map(Self)
    }
}

impl_arg!(InboxFileArg, positional "FILENAME", "File name in inbox");

/// List files in inbox
#[hicli::command]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    match library.inbox_queue() {
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

/// Delete a file from inbox without ingesting
#[hicli::command]
pub(crate) fn delete(file: InboxFileArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    let InboxFileArg(path) = file;

    match library.delete_inbox_file(&path) {
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

/// Ingest a specific file from inbox
#[hicli::command]
pub(crate) fn ingest(file: InboxFileArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    let InboxFileArg(path) = file;

    println!("Ingesting: {}", path.as_str());

    match library.ingest_file(&path) {
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

/// Ingest all files in inbox
#[hicli::command]
pub(crate) fn ingest_all(ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    println!("Ingesting all files in inbox...");

    match library.ingest_all() {
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
    fn valid_inbox_file_is_accepted() {
        for sub in ["delete", "ingest"] {
            assert!(parse(&["inbox", sub, "dir/track.mp3"]).is_ok());
        }
    }
}
