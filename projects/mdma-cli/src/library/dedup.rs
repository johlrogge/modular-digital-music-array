use crate::ctx::Ctx;
use crate::stdin::is_yes;
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::{
    ConfirmationReason, ContentHash, DedupApplyReport, DedupDisposition, DedupGroup, DedupMember,
};
use mdma_client::LibraryBackend;
use track_formatter::short_hash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Merge tracks you have confirmed to be duplicates into a winner.
    ///
    /// The other tracks' audio files are moved to the archive. All tracks must be in
    /// one duplicate group as listed by `mdma library dedup`.
    Merge {
        /// The track to keep (prefix accepted, git-style)
        winner: String,
        /// The duplicates to merge into it (prefixes accepted)
        #[arg(required = true)]
        losers: Vec<String>,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

pub(crate) fn dispatch(apply: bool, command: Option<Command>, ctx: &Ctx) -> Result<()> {
    let lib = ctx.library();
    match &command {
        None => handle_dedup(&lib, apply),
        Some(Command::Merge {
            winner,
            losers,
            yes,
        }) => handle_dedup_merge(&lib, winner, losers, *yes),
    }
}

fn dedup_reason_text(reason: ConfirmationReason) -> &'static str {
    match reason {
        ConfirmationReason::DifferentItemIds => "different purchases (item ids)",
        ConfirmationReason::NoItemId => "a copy has no item id",
        ConfirmationReason::SoftDeleted => "a copy is deleted",
    }
}

fn format_dedup_member(role: &str, m: &DedupMember) -> String {
    let title = m.title.as_deref().unwrap_or("Unknown");
    let album = m
        .album
        .as_deref()
        .map(|a| format!("  [{a}]"))
        .unwrap_or_default();
    let added = m
        .added
        .as_deref()
        .map(|a| a.get(..10).unwrap_or(a))
        .unwrap_or("?");
    let item = m.item_id.as_deref().unwrap_or("-");
    let deleted = if m.deleted { "  (deleted)" } else { "" };
    format!(
        "  {role:<5}  {}  {title}{album}  added {added}  item {item}{deleted}",
        short_hash(&m.hash)
    )
}

/// The groups as the user sees them: automatic ones first, each member on a line
/// (`keep` is the winner), and the command that confirms a group that needs it.
fn format_dedup_groups(groups: &[DedupGroup]) -> String {
    if groups.is_empty() {
        return "No duplicates found".to_string();
    }
    let mut ordered: Vec<&DedupGroup> = groups.iter().collect();
    ordered.sort_by_key(|g| !g.is_automatic());

    let mut out = String::new();
    for g in ordered {
        match &g.disposition {
            DedupDisposition::Automatic => out.push_str("Automatic (merged by --apply)\n"),
            DedupDisposition::NeedsConfirmation { reasons } => {
                let why: Vec<&str> = reasons.iter().map(|r| dedup_reason_text(*r)).collect();
                out.push_str(&format!("Needs confirmation: {}\n", why.join(", ")));
            }
        }
        for m in &g.members {
            let role = if m.hash == g.winner { "keep" } else { "merge" };
            out.push_str(&format_dedup_member(role, m));
            out.push('\n');
        }
        if !g.is_automatic() {
            let losers: Vec<&str> = g.losers().map(|m| short_hash(&m.hash)).collect();
            out.push_str(&format!(
                "  confirm with: mdma library dedup merge {} {}\n",
                short_hash(&g.winner),
                losers.join(" ")
            ));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn format_dedup_applied(r: &DedupApplyReport) -> String {
    let mut out = format!(
        "Merged {} track(s), archived {} blob(s)",
        r.merged, r.blobs_archived
    );
    for e in &r.errors {
        out.push_str(&format!("\n  error: {e}"));
    }
    out
}

/// The group that has a member starting with `partial` (a git-style hash prefix):
/// `Ok(None)` when no group does, an error when more than one group does.
fn find_dedup_group<'a>(
    groups: &'a [DedupGroup],
    partial: &str,
) -> std::result::Result<Option<&'a DedupGroup>, String> {
    let clean = |h: &str| h.strip_prefix("sha256:").unwrap_or(h).to_lowercase();
    let partial = clean(partial);
    let mut matching = groups.iter().filter(|g| {
        g.members
            .iter()
            .any(|m| clean(m.hash.as_str()).starts_with(&partial))
    });
    let found = matching.next();
    if matching.next().is_some() {
        return Err(format!("'{partial}' matches more than one duplicate group"));
    }
    Ok(found)
}

fn handle_dedup(client: &LibraryBackend, apply: bool) -> Result<()> {
    let report = match client.dedup_scan(apply) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };
    let failed = report
        .applied
        .as_ref()
        .is_some_and(|a| !a.errors.is_empty());
    if let Some(applied) = &report.applied {
        println!("{}", format_dedup_applied(applied));
        if !report.groups.is_empty() {
            println!("\nLeft for your confirmation:");
        }
    }
    // After `--apply` an empty list is already said by the summary.
    if !apply || !report.groups.is_empty() {
        println!("{}", format_dedup_groups(&report.groups));
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn handle_dedup_merge(
    client: &LibraryBackend,
    winner: &str,
    losers: &[String],
    yes: bool,
) -> Result<()> {
    if !yes {
        if let Ok(report) = client.dedup_scan(false) {
            // Every prefix must name at most one group before anything is confirmed.
            let mut shown = None;
            for prefix in std::iter::once(winner).chain(losers.iter().map(String::as_str)) {
                match find_dedup_group(&report.groups, prefix) {
                    Ok(group) => shown = shown.or(group),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                }
            }
            if let Some(group) = shown {
                println!("{}\n", format_dedup_groups(std::slice::from_ref(group)));
            }
        }
        print!(
            "Merge {} track(s) into {winner}? Their audio files are moved to the archive. [y/N] ",
            losers.len()
        );
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !is_yes(&answer) {
            println!("Aborted");
            return Ok(());
        }
    }

    let loser_hashes: Vec<ContentHash> = losers.iter().map(ContentHash::new).collect();
    match client.dedup_merge(&ContentHash::new(winner), &loser_hashes) {
        Ok(report) => {
            println!("{}", format_dedup_applied(&report));
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

    use library_ipc_client::{
        ConfirmationReason, ContentHash, DedupApplyReport, DedupDisposition, DedupGroup,
        DedupMember,
    };

    use crate::Cli;

    use pretty_assertions::assert_eq;

    use super::Command as DedupCommands;
    use crate::library::Command as LibraryCommands;
    use crate::Commands;

    // ── dedup ─────────────────────────────────────────────────────────────────

    fn dedup_args(args: &[&str]) -> (bool, Option<DedupCommands>) {
        let mut argv = vec!["mdma", "library", "dedup"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(&argv)
            .unwrap_or_else(|e| panic!("{argv:?}: {e}"))
            .command
        {
            Commands::Library {
                command: LibraryCommands::Dedup { apply, command },
            } => (apply, command),
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn dedup_lists_by_default_and_applies_with_the_flag() {
        assert!(matches!(dedup_args(&[]), (false, None)));
        assert!(matches!(dedup_args(&["--apply"]), (true, None)));
    }

    #[test]
    fn dedup_merge_takes_a_winner_losers_and_yes() {
        match dedup_args(&["merge", "abc123", "def456", "0a1b2c"]) {
            (
                false,
                Some(DedupCommands::Merge {
                    winner,
                    losers,
                    yes,
                }),
            ) => {
                assert_eq!(winner, "abc123");
                assert_eq!(losers, vec!["def456", "0a1b2c"]);
                assert!(!yes);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            dedup_args(&["merge", "--yes", "a", "b"]),
            (false, Some(DedupCommands::Merge { yes: true, .. }))
        ));
        assert!(matches!(
            dedup_args(&["merge", "-y", "a", "b"]),
            (false, Some(DedupCommands::Merge { yes: true, .. }))
        ));
    }

    #[test]
    fn dedup_merge_needs_a_loser_and_does_not_mix_with_apply() {
        assert!(Cli::try_parse_from(["mdma", "library", "dedup", "merge", "abc"]).is_err());
        assert!(Cli::try_parse_from(["mdma", "library", "dedup", "merge"]).is_err());
        assert!(
            Cli::try_parse_from(["mdma", "library", "dedup", "--apply", "merge", "a", "b"])
                .is_err()
        );
    }

    fn dedup_member(hash: &str, item: Option<&str>) -> DedupMember {
        DedupMember {
            hash: ContentHash::new(format!("sha256:{hash}")),
            title: Some("Espansione".into()),
            album: Some("Part 3 & 4".into()),
            added: Some("2026-09-01T10:00:00+00:00".into()),
            item_id: item.map(str::to_owned),
            deleted: false,
        }
    }

    fn dedup_group(winner: &str, loser: &str, disposition: DedupDisposition) -> DedupGroup {
        DedupGroup {
            members: vec![
                dedup_member(winner, Some("p1")),
                dedup_member(loser, Some("p1")),
            ],
            winner: ContentHash::new(format!("sha256:{winner}")),
            disposition,
        }
    }

    #[test]
    fn dedup_listing_puts_automatic_groups_first_and_says_how_to_confirm_the_rest() {
        let groups = [
            dedup_group(
                "bbbbbbbbbbbbbbbb",
                "cccccccccccccccc",
                DedupDisposition::NeedsConfirmation {
                    reasons: vec![ConfirmationReason::DifferentItemIds],
                },
            ),
            dedup_group(
                "aaaaaaaaaaaaaaaa",
                "dddddddddddddddd",
                DedupDisposition::Automatic,
            ),
        ];

        let text = format_dedup_groups(&groups);

        let auto = text.find("Automatic").unwrap();
        let confirm = text
            .find("Needs confirmation: different purchases")
            .unwrap();
        assert!(auto < confirm, "{text}");
        assert!(text.contains("keep   aaaaaaaaaaaa"), "{text}");
        assert!(text.contains("merge  dddddddddddd"), "{text}");
        assert!(text.contains("added 2026-09-01"), "{text}");
        assert!(
            text.contains("confirm with: mdma library dedup merge bbbbbbbbbbbb cccccccccccc"),
            "{text}"
        );
        assert_eq!(
            text.matches("confirm with").count(),
            1,
            "automatic groups need no confirmation: {text}"
        );
        assert_eq!(format_dedup_groups(&[]), "No duplicates found");
    }

    #[test]
    fn dedup_applied_summary_lists_errors() {
        let r = DedupApplyReport {
            merged: 2,
            blobs_archived: 1,
            errors: vec!["boom".into()],
        };
        assert_eq!(
            format_dedup_applied(&r),
            "Merged 2 track(s), archived 1 blob(s)\n  error: boom"
        );
    }

    #[test]
    fn dedup_group_is_found_by_a_hash_prefix_with_or_without_sha256() {
        let groups = [dedup_group(
            "abcdef0123",
            "fedcba9876",
            DedupDisposition::Automatic,
        )];
        assert!(find_dedup_group(&groups, "abcd").unwrap().is_some());
        assert!(find_dedup_group(&groups, "sha256:FEDC").unwrap().is_some());
        assert!(find_dedup_group(&groups, "0000").unwrap().is_none());
    }

    #[test]
    fn a_hash_prefix_that_matches_two_groups_is_an_error() {
        let groups = [
            dedup_group("abcdef0123", "fedcba9876", DedupDisposition::Automatic),
            dedup_group("abcd001122", "112233aabb", DedupDisposition::Automatic),
        ];
        assert!(find_dedup_group(&groups, "abcd").is_err());
        // Long enough to tell them apart, and a prefix of two members of one group.
        assert_eq!(
            find_dedup_group(&groups, "abcdef").unwrap().unwrap().winner,
            groups[0].winner
        );
        assert!(find_dedup_group(&groups, "112").unwrap().is_some());
    }
}
