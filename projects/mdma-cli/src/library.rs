use crate::args::impl_arg;
use crate::ctx::Ctx;
use color_eyre::Result;

/// Re-extract and store cover art for tracks that don't have a CoverArtPath fact yet
///
/// Reads embedded artwork from each track's audio blob and writes the image to
/// `/music/cover-art/<hash>.<ext>`. Only tracks without an existing CoverArtPath
/// fact are processed.
#[hicli::command]
pub(crate) fn reindex_covers(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse};
    let library = ctx.library();
    let response = library.request(&LibraryRequest::ReindexCovers);
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

/// Manage individual tracks (replace, delete, restore, list orphans)
#[hicli::group]
mod track;

/// Compute track identity hashes (used to find duplicates)
#[hicli::group]
mod identity;

/// Find duplicate tracks (the same audio with different tags or files) and merge them
///
/// Without options, lists the groups: those that can be merged automatically (all
/// copies come from one purchase and none is deleted) first, then those that need
/// your confirmation. The newest copy is kept; its blob stays, the other copies'
/// facts are folded into it (latest value per attribute wins), playlists are
/// rewritten, and the other copies' audio files are moved to the archive (see
/// `mdma library archive`), not deleted.
///
/// Needs identity hashes: run `mdma library identity backfill` first.
#[hicli::group]
mod dedup;

/// List the files that duplicate merges moved to the archive
///
/// A merge never deletes the other copies' audio files and cover art: it moves
/// them to `archive/` in the music directory, keeping their layout. Shows how
/// many files the archive holds, their total size, and for each its hash, size
/// and when it was archived. `mdma library archive empty` deletes them for good.
#[hicli::group]
mod archive;

/// Do not ask for confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Yes(pub bool);

impl_arg!(Yes, switch "yes", short 'y', "Do not ask for confirmation");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_arg;

    #[test]
    fn yes_arg_parser_is_a_switch() {
        assert_eq!(run_arg::<Yes>(&[]), Ok(Yes(false)));
        assert_eq!(run_arg::<Yes>(&["--yes"]), Ok(Yes(true)));
        assert_eq!(run_arg::<Yes>(&["-y"]), Ok(Yes(true)));
        assert!(run_arg::<Yes>(&["--maybe"]).is_err());
    }
}
