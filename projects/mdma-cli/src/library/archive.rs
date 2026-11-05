use crate::ctx::Ctx;
use crate::library::Yes;
use crate::stdin::confirm;
use color_eyre::Result;
use library_ipc_client::{ArchiveEmptyReport, ArchiveListing};
use storage_primitives::ByteSize;

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

/// List the files that duplicate merges moved to the archive
#[hicli::command(default)]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    let library = ctx.library();
    match library.archive_list() {
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

/// Permanently delete everything in the archive
///
/// Shows what the archive holds and asks for confirmation. This cannot be undone.
#[hicli::command]
pub(crate) fn empty(yes: Yes, ctx: &Ctx) -> Result<()> {
    let Yes(yes) = yes;
    let library = ctx.library();
    // `--yes` without a listing deletes everything; after a prompt, only what was shown.
    let mut up_to = None;
    if !yes {
        let listing = match library.archive_list() {
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
        if !confirm(&archive_empty_prompt(&listing))? {
            println!("Aborted");
            return Ok(());
        }
        up_to = Some(archive_cutoff(&listing));
    }

    match library.archive_empty(up_to.as_deref()) {
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

    use library_ipc_client::{ArchiveEmptyReport, ArchiveListing};

    use crate::test_support::{help, parses, usage_error};

    use pretty_assertions::assert_eq;

    // ── archive ───────────────────────────────────────────────────────────────

    /// The path `mdma library archive <args>` reaches.
    fn archive_path(args: &[&str]) -> Vec<&'static str> {
        let mut argv = vec!["library", "archive"];
        argv.extend_from_slice(args);
        parses(&argv)
    }

    #[test]
    fn archive_lists_by_default_and_empties_with_a_subcommand() {
        assert_eq!(archive_path(&[]), ["library", "archive"]);
        for args in [&["empty"][..], &["empty", "--yes"], &["empty", "-y"]] {
            assert_eq!(archive_path(args), ["library", "archive", "empty"]);
        }
        usage_error(&["library", "archive", "empty", "x"]);
        usage_error(&["library", "archive", "--yes"]);
    }

    #[test]
    fn archive_help_shows_the_default_listing_and_command_usage() {
        let help = help(&["library", "archive", "--help"]);
        assert!(
            help.contains("Usage: mdma library archive [COMMAND ...]"),
            "{help}"
        );
        assert!(
            help.contains("List the files that duplicate merges moved to the archive"),
            "{help}"
        );
        assert!(help.contains("empty"), "{help}");
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
