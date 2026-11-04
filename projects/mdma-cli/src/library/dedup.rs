use crate::args::{impl_arg, TrackArg};
use crate::ctx::Ctx;
use crate::library::Yes;
use crate::stdin::confirm;
use bpaf::Bpaf;
use clap::Subcommand;
use color_eyre::Result;
use hicli::Arg;
use library_ipc_client::{
    ConfirmationReason, ContentHash, DedupApplyReport, DedupDisposition, DedupGroup, DedupMember,
};
use track_formatter::short_hash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Merge tracks you have confirmed to be duplicates into a winner.
    ///
    /// The other tracks' audio files are moved to the archive. All tracks must be in
    /// one duplicate group as listed by `mdma library dedup`.
    Merge {
        /// The track to keep (prefix accepted, git-style)
        winner: TrackArg,
        /// The duplicates to merge into it (prefixes accepted)
        #[arg(required = true)]
        losers: Vec<TrackArg>,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// What `mdma library dedup` takes: the listing's `--apply`, or a subcommand.
#[derive(clap::Args, Debug)]
pub(crate) struct DedupCli {
    /// Merge the automatic groups (the others are only listed)
    #[arg(long)]
    apply: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Whether the listing also merges the automatic groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Apply(pub bool);

impl_arg!(
    Apply,
    switch "apply",
    "Merge the automatic groups (the others are only listed)"
);

#[derive(Debug, Clone, PartialEq, Eq, Bpaf, Arg)]
pub(crate) struct MergeArgs {
    #[bpaf(external(Yes::parser))]
    pub(crate) yes: Yes,
    /// The track to keep (prefix accepted, git-style)
    #[bpaf(positional("WINNER"))]
    pub(crate) winner: TrackArg,
    /// The duplicates to merge into it (prefixes accepted)
    #[bpaf(
        positional("LOSERS"),
        some("at least one duplicate to merge is required")
    )]
    pub(crate) losers: Vec<TrackArg>,
}

impl MergeArgs {
    fn new(yes: bool, winner: TrackArg, losers: Vec<TrackArg>) -> Self {
        Self {
            yes: Yes(yes),
            winner,
            losers,
        }
    }
}

pub(crate) fn dispatch(args: DedupCli, ctx: &Ctx) -> Result<()> {
    let DedupCli { apply, command } = args;
    match command {
        None => scan(Apply(apply), ctx),
        Some(Command::Merge {
            winner,
            losers,
            yes,
        }) => merge(MergeArgs::new(yes, winner, losers), ctx),
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
    partial: &ContentHash,
) -> std::result::Result<Option<&'a DedupGroup>, String> {
    // `partial` is already lowercase hex (a parsed `TrackArg`); only the
    // service's member hashes need normalising.
    fn strip(h: &str) -> &str {
        h.strip_prefix("sha256:").unwrap_or(h)
    }
    let partial = strip(partial.as_str());
    let mut matching = groups.iter().filter(|g| {
        g.members
            .iter()
            .any(|m| strip(m.hash.as_str()).to_lowercase().starts_with(partial))
    });
    let found = matching.next();
    if matching.next().is_some() {
        return Err(format!("'{partial}' matches more than one duplicate group"));
    }
    Ok(found)
}

pub(crate) fn scan(apply: Apply, ctx: &Ctx) -> Result<()> {
    let Apply(apply) = apply;
    let library = ctx.library();
    let report = match library.dedup_scan(apply) {
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

pub(crate) fn merge(args: MergeArgs, ctx: &Ctx) -> Result<()> {
    let MergeArgs {
        yes: Yes(yes),
        winner: TrackArg(winner),
        losers,
    } = args;
    let losers: Vec<ContentHash> = losers.into_iter().map(|TrackArg(h)| h).collect();
    let library = ctx.library();
    if !yes {
        if let Ok(report) = library.dedup_scan(false) {
            // Every prefix must name at most one group before anything is confirmed.
            let mut shown = None;
            for prefix in std::iter::once(&winner).chain(&losers) {
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
        let prompt = format!(
            "Merge {} track(s) into {winner}? Their audio files are moved to the archive. [y/N] ",
            losers.len()
        );
        if !confirm(&prompt)? {
            println!("Aborted");
            return Ok(());
        }
    }

    match library.dedup_merge(&winner, &losers) {
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

    use library_ipc_client::{
        ConfirmationReason, ContentHash, DedupApplyReport, DedupDisposition, DedupGroup,
        DedupMember,
    };

    use crate::test_support::{parse, parses, usage_error};

    use pretty_assertions::assert_eq;

    use crate::library;
    use crate::test_support::run_arg;
    use crate::Commands;

    #[test]
    fn apply_arg_parser_is_a_switch() {
        assert_eq!(run_arg::<Apply>(&[]), Ok(Apply(false)));
        assert_eq!(run_arg::<Apply>(&["--apply"]), Ok(Apply(true)));
        assert!(run_arg::<Apply>(&["--appply"]).is_err());
    }

    #[test]
    fn merge_args_parser_reads_winner_losers_and_yes() {
        let hash = |h: &str| TrackArg(ContentHash::new(h));
        assert_eq!(
            run_arg::<MergeArgs>(&["abc12345", "def45678", "0a1b2c3d"]),
            Ok(MergeArgs {
                yes: Yes(false),
                winner: hash("abc12345"),
                losers: vec![hash("def45678"), hash("0a1b2c3d")],
            })
        );
        assert_eq!(
            run_arg::<MergeArgs>(&["-y", "abc12345", "def45678"]),
            Ok(MergeArgs {
                yes: Yes(true),
                winner: hash("abc12345"),
                losers: vec![hash("def45678")],
            })
        );
    }

    #[test]
    fn merge_args_parser_wants_a_loser_and_valid_hashes() {
        assert!(run_arg::<MergeArgs>(&[]).is_err());
        let err = run_arg::<MergeArgs>(&["abc12345"]).unwrap_err();
        assert!(err.contains("at least one duplicate"), "{err}");
        let err = run_arg::<MergeArgs>(&["abc12345", "nothash"]).unwrap_err();
        assert!(err.contains("is not a content hash"), "{err}");
    }

    // ── dedup ─────────────────────────────────────────────────────────────────

    fn dedup_args(args: &[&str]) -> (bool, Option<MergeArgs>) {
        let mut argv = vec!["library", "dedup"];
        argv.extend_from_slice(args);
        match parses(&argv) {
            Commands::Library {
                command: library::Command::Dedup(DedupCli { apply, command }),
            } => (
                apply,
                command.map(
                    |Command::Merge {
                         winner,
                         losers,
                         yes,
                     }| MergeArgs::new(yes, winner, losers),
                ),
            ),
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
        assert_eq!(
            dedup_args(&["merge", "abc12345", "def45678", "0a1b2c3d"]),
            (
                false,
                Some(MergeArgs {
                    yes: Yes(false),
                    winner: TrackArg(ContentHash::new("abc12345")),
                    losers: vec![
                        TrackArg(ContentHash::new("def45678")),
                        TrackArg(ContentHash::new("0a1b2c3d"))
                    ],
                })
            )
        );
        for flag in ["--yes", "-y"] {
            let (apply, merge) = dedup_args(&["merge", flag, "aaaaaaaa", "bbbbbbbb"]);
            assert!(!apply);
            assert_eq!(merge.map(|m| m.yes), Some(Yes(true)));
        }
    }

    #[test]
    fn dedup_merge_needs_a_loser_and_does_not_mix_with_apply() {
        usage_error(&["library", "dedup", "merge", "abcdef12"]);
        usage_error(&["library", "dedup", "merge"]);
        usage_error(&[
            "library", "dedup", "--apply", "merge", "aaaaaaaa", "bbbbbbbb",
        ]);
    }

    #[test]
    fn dedup_merge_hashes_must_be_valid_hashes() {
        let merge =
            |winner: &str, loser: &str| parse(&["library", "dedup", "merge", winner, loser]);
        for bad in ["abcd123", "ABCD1234"] {
            usage_error(&["library", "dedup", "merge", bad, "bbbbbbbb"]);
            usage_error(&["library", "dedup", "merge", "aaaaaaaa", bad]);
        }
        assert!(merge("aaaaaaaa", "sha256:bbbbbbbb").is_ok());
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
        assert!(find_dedup_group(&groups, &hash("abcd")).unwrap().is_some());
        assert!(find_dedup_group(&groups, &hash("sha256:fedc"))
            .unwrap()
            .is_some());
        assert!(find_dedup_group(&groups, &hash("0000")).unwrap().is_none());
    }

    fn hash(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    #[test]
    fn a_hash_prefix_that_matches_two_groups_is_an_error() {
        let groups = [
            dedup_group("abcdef0123", "fedcba9876", DedupDisposition::Automatic),
            dedup_group("abcd001122", "112233aabb", DedupDisposition::Automatic),
        ];
        assert!(find_dedup_group(&groups, &hash("abcd")).is_err());
        // Long enough to tell them apart, and a prefix of two members of one group.
        assert_eq!(
            find_dedup_group(&groups, &hash("abcdef"))
                .unwrap()
                .unwrap()
                .winner,
            groups[0].winner
        );
        assert!(find_dedup_group(&groups, &hash("112")).unwrap().is_some());
    }
}
