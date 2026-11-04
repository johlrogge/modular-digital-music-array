use crate::ctx::Ctx;
use crate::output::handle_error;
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::InboxPath;
use track_formatter::short_hash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
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

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::List => list(ctx),
        Command::Delete { filename } => delete(InboxFileArg(filename), ctx),
        Command::Ingest { filename } => ingest(InboxFileArg(filename), ctx),
        Command::IngestAll => ingest_all(ctx),
    }
}

/// File name in the inbox as typed. Validated into an `InboxPath` after the
/// library connection is made, so invalid names keep failing at the same
/// point as before (phase-2 parse-time candidate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboxFileArg(pub String);

impl InboxFileArg {
    fn path(&self) -> InboxPath {
        match InboxPath::new(&self.0) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Invalid filename: {}", e);
                std::process::exit(1);
            }
        }
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
    let path = file.path();

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
    let path = file.path();

    println!("Ingesting: {}", file.0);

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
