use crate::ctx::Ctx;
use crate::output::handle_error;
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::{InboxPath, InboxPathError};
use std::str::FromStr;
use track_formatter::short_hash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// List files in inbox
    List,

    /// Delete a file from inbox without ingesting
    Delete {
        /// File name in inbox
        filename: InboxFileArg,
    },

    /// Ingest a specific file from inbox
    Ingest {
        /// File name in inbox
        filename: InboxFileArg,
    },

    /// Ingest all files in inbox
    IngestAll,
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::List => list(ctx),
        Command::Delete { filename } => delete(filename, ctx),
        Command::Ingest { filename } => ingest(filename, ctx),
        Command::IngestAll => ingest_all(ctx),
    }
}

/// File name in the inbox, validated by `InboxPath` at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboxFileArg(pub InboxPath);

impl FromStr for InboxFileArg {
    type Err = InboxPathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        InboxPath::new(s).map(Self)
    }
}

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
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn invalid_inbox_file_is_a_usage_error() {
        for sub in ["delete", "ingest"] {
            for bad in ["/abs/file.mp3", "../escape.mp3"] {
                let err = Cli::try_parse_from(["mdma", "inbox", sub, bad]).unwrap_err();
                assert_eq!(err.exit_code(), 2, "{sub} {bad}: {err}");
            }
        }
    }

    #[test]
    fn valid_inbox_file_is_accepted() {
        for sub in ["delete", "ingest"] {
            assert!(Cli::try_parse_from(["mdma", "inbox", sub, "dir/track.mp3"]).is_ok());
        }
    }
}
