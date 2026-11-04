use crate::ctx::Ctx;
use crate::library::Yes;
use crate::stdin::is_yes;
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::{ArchiveEmptyReport, ArchiveListing};
use storage_primitives::ByteSize;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Permanently delete everything in the archive.
    ///
    /// Shows what the archive holds and asks for confirmation. This cannot be undone.
    Empty {
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

pub(crate) fn dispatch(command: Option<Command>, ctx: &Ctx) -> Result<()> {
    match command {
        None => list(ctx),
        Some(Command::Empty { yes }) => empty(Yes(yes), ctx),
    }
}

/// The archive as the user sees it: a summary line, then a line per file.
fn format_archive_listing(l: &ArchiveListing) -> String {
    if l.entries.is_empty() {
        return "The archive is empty".to_string();
    }
    let mut out = format!(
        "Archive: {} file(s), {}\n",
        l.entries.len(),
        ByteSize::new(l.total_bytes)
    );
    for e in &l.entries {
        let hash = e.hash.get(..12).unwrap_or(&e.hash);
        let at = e
            .archived_at
            .as_deref()
            .map(|a| a.get(..10).unwrap_or(a))
            .unwrap_or("?");
        out.push_str(&format!(
            "  {hash:<12}  {:>9}  archived {at}  {}\n",
            ByteSize::new(e.size_bytes).to_string(),
            e.path
        ));
    }
    out.trim_end().to_string()
}

fn archive_empty_prompt(l: &ArchiveListing) -> String {
    format!(
        "Permanently delete {} file(s) ({}) from the archive? This cannot be undone. [y/N] ",
        l.entries.len(),
        ByteSize::new(l.total_bytes)
    )
}

/// The newest `archived_at` in `l` (the listing is oldest first): what a confirmed
/// emptying may delete, so files archived after the listing survive. A listing
/// without any time falls back to the epoch (deletes nothing newer than "unknown").
fn archive_cutoff(l: &ArchiveListing) -> String {
    l.entries
        .iter()
        .rev()
        .find_map(|e| e.archived_at.clone())
        .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_owned())
}

fn format_archive_emptied(r: &ArchiveEmptyReport) -> String {
    let mut out = format!(
        "Deleted {} file(s), freed {}",
        r.files_deleted,
        ByteSize::new(r.bytes_freed)
    );
    for e in &r.errors {
        out.push_str(&format!("\n  error: {e}"));
    }
    out
}

pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let client = ctx.library();
    match client.archive_list() {
        Ok(l) => {
            println!("{}", format_archive_listing(&l));
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn empty(yes: Yes, ctx: &Ctx) -> Result<()> {
    let Yes(yes) = yes;
    let client = ctx.library();
    // `--yes` without a listing deletes everything; after a prompt, only what was shown.
    let mut up_to = None;
    if !yes {
        let listing = match client.archive_list() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        };
        println!("{}\n", format_archive_listing(&listing));
        if listing.entries.is_empty() {
            return Ok(());
        }
        print!("{}", archive_empty_prompt(&listing));
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !is_yes(&answer) {
            println!("Aborted");
            return Ok(());
        }
        up_to = Some(archive_cutoff(&listing));
    }

    match client.archive_empty(up_to.as_deref()) {
        Ok(report) => {
            println!("{}", format_archive_emptied(&report));
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;

    use library_ipc_client::{ArchiveEmptyReport, ArchiveListing};

    use crate::Cli;

    use pretty_assertions::assert_eq;

    use crate::library;
    use crate::Commands;

    // ── archive ───────────────────────────────────────────────────────────────

    fn archive_command(args: &[&str]) -> Option<Command> {
        let mut argv = vec!["mdma", "library", "archive"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(&argv)
            .unwrap_or_else(|e| panic!("{argv:?}: {e}"))
            .command
        {
            Commands::Library {
                command: library::Command::Archive { command },
            } => command,
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn archive_lists_by_default_and_empties_with_a_subcommand() {
        assert!(archive_command(&[]).is_none());
        assert!(matches!(
            archive_command(&["empty"]),
            Some(Command::Empty { yes: false })
        ));
        assert!(matches!(
            archive_command(&["empty", "--yes"]),
            Some(Command::Empty { yes: true })
        ));
        assert!(matches!(
            archive_command(&["empty", "-y"]),
            Some(Command::Empty { yes: true })
        ));
        assert!(Cli::try_parse_from(["mdma", "library", "archive", "empty", "x"]).is_err());
        assert!(Cli::try_parse_from(["mdma", "library", "archive", "--yes"]).is_err());
    }

    fn archive_listing_of_two() -> ArchiveListing {
        let entry = |hash: &str, size: u64, at: &str| library_ipc_client::ArchiveEntry {
            path: format!("blobs/{}/{hash}.flac", &hash[..2]),
            hash: hash.into(),
            size_bytes: size,
            archived_at: Some(at.into()),
        };
        ArchiveListing {
            entries: vec![
                entry("abcdef0123456789", 3_000_000, "2026-09-01T10:00:00+00:00"),
                entry("fedcba98", 2_000_000, "2026-09-02T10:00:00+00:00"),
            ],
            total_bytes: 5_000_000,
        }
    }

    #[test]
    fn archive_listing_shows_count_total_and_each_entry() {
        let text = format_archive_listing(&archive_listing_of_two());
        assert!(text.starts_with("Archive: 2 file(s), 5 MB\n"), "{text}");
        assert!(text.contains("abcdef012345  "), "{text}");
        assert!(text.contains("3 MB"), "{text}");
        assert!(text.contains("archived 2026-09-01"), "{text}");
        assert!(text.contains("blobs/ab/abcdef0123456789.flac"), "{text}");
        assert!(text.contains("fedcba98  "), "{text}");
        assert_eq!(
            format_archive_listing(&ArchiveListing::default()),
            "The archive is empty"
        );
    }

    #[test]
    fn a_confirmed_emptying_is_cut_off_at_the_newest_listed_time() {
        assert_eq!(
            archive_cutoff(&archive_listing_of_two()),
            "2026-09-02T10:00:00+00:00"
        );
        assert_eq!(
            archive_cutoff(&ArchiveListing::default()),
            "1970-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn archive_empty_asks_with_the_count_and_size_and_reports_the_result() {
        assert_eq!(
            archive_empty_prompt(&archive_listing_of_two()),
            "Permanently delete 2 file(s) (5 MB) from the archive? This cannot be undone. [y/N] "
        );
        let r = ArchiveEmptyReport {
            files_deleted: 2,
            bytes_freed: 5_000_000,
            errors: vec!["boom".into()],
        };
        assert_eq!(
            format_archive_emptied(&r),
            "Deleted 2 file(s), freed 5 MB\n  error: boom"
        );
    }
}
