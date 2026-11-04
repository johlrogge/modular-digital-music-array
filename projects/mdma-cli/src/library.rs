mod archive;
mod dedup;
mod identity;
mod track;

use crate::ctx::Ctx;
use clap::Subcommand;
use color_eyre::Result;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Re-extract and store cover art for tracks that don't have a CoverArtPath fact yet.
    ///
    /// Reads embedded artwork from each track's audio blob and writes the image to
    /// `/music/cover-art/<hash>.<ext>`. Only tracks without an existing CoverArtPath
    /// fact are processed.
    ReindexCovers,

    /// Manage individual tracks (replace, delete, restore, list orphans)
    Track {
        #[command(subcommand)]
        command: track::Command,
    },

    /// Compute track identity hashes (used to find duplicates)
    Identity {
        #[command(subcommand)]
        command: identity::Command,
    },

    /// Find duplicate tracks (the same audio with different tags or files) and merge them.
    ///
    /// Without options, lists the groups: those that can be merged automatically (all
    /// copies come from one purchase and none is deleted) first, then those that need
    /// your confirmation. The newest copy is kept; its blob stays, the other copies'
    /// facts are folded into it (latest value per attribute wins), playlists are
    /// rewritten, and the other copies' audio files are moved to the archive (see
    /// `mdma library archive`), not deleted.
    ///
    /// Needs identity hashes: run `mdma library identity backfill` first.
    #[command(args_conflicts_with_subcommands = true)]
    Dedup(dedup::Args),

    /// List the files that duplicate merges moved to the archive.
    ///
    /// A merge never deletes the other copies' audio files and cover art: it moves
    /// them to `archive/` in the music directory, keeping their layout. Shows how
    /// many files the archive holds, their total size, and for each its hash, size
    /// and when it was archived. `mdma library archive empty` deletes them for good.
    Archive {
        #[command(subcommand)]
        command: Option<archive::Command>,
    },
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::ReindexCovers => reindex_covers(ctx),
        Command::Track { command } => track::dispatch(command, ctx),
        Command::Identity { command } => identity::dispatch(command, ctx),
        Command::Dedup(args) => dedup::dispatch(args, ctx),
        Command::Archive { command } => archive::dispatch(command, ctx),
    }
}

pub(crate) fn reindex_covers(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let client = ctx.library();
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

/// Do not ask for confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Yes(pub bool);
