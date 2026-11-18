//! Which kinds of fact `mdma facts` shows: `--include` and `--exclude`.
//!
//! Filtering runs in the client, on the facts the library returns, so the protocol
//! is unchanged.

use crate::args::HashInput;
use hicli::{Arg, Candidate};
use music_facts::{FactKind, MusicValue};

/// Show only facts of this kind (repeatable; case is ignored)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "include", metavar = "KIND", complete = complete_fact_kinds)]
pub(crate) struct IncludeKinds(pub Vec<FactKind>);

/// Leave out facts of this kind (repeatable; case is ignored; wins over --include)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "exclude", metavar = "KIND", complete = complete_fact_kinds)]
pub(crate) struct ExcludeKinds(pub Vec<FactKind>);

/// The arguments of `mdma facts`.
#[derive(Debug, Clone, Arg)]
#[allow(missing_docs)]
pub(crate) struct FactsArgs {
    pub include: IncludeKinds,
    pub exclude: ExcludeKinds,
    pub hash: HashInput,
}

/// The attribute names that start with `typed`, ignoring case.
///
/// hicli hands over the raw word and does not filter, so this must.
pub(crate) fn complete_fact_kinds(typed: &str) -> Vec<Candidate> {
    let typed = typed.to_ascii_lowercase();
    MusicValue::ATTRIBUTE_NAMES
        .iter()
        .filter(|name| name.to_ascii_lowercase().starts_with(&typed))
        .map(|name| (name.to_string(), None))
        .collect()
}

/// Which kinds `--include` lets through: all of them, or only those named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Include {
    All,
    Only(Vec<FactKind>),
}

impl From<IncludeKinds> for Include {
    fn from(IncludeKinds(kinds): IncludeKinds) -> Self {
        if kinds.is_empty() {
            Self::All
        } else {
            Self::Only(kinds)
        }
    }
}

impl Include {
    fn admits(&self, kind: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(kinds) => kinds.iter().any(|k| k.as_str() == kind),
        }
    }
}

/// The kinds of fact to show: those included and not excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KindFilter {
    include: Include,
    exclude: Vec<FactKind>,
}

impl KindFilter {
    pub(crate) fn new(include: IncludeKinds, ExcludeKinds(exclude): ExcludeKinds) -> Self {
        Self {
            include: include.into(),
            exclude,
        }
    }

    /// Whether a fact of the kind named `kind` is shown. The name is compared as
    /// text, so a kind this CLI does not know still shows when nothing is included.
    pub(crate) fn keeps(&self, kind: &str) -> bool {
        self.include.admits(kind) && !self.exclude.iter().any(|k| k.as_str() == kind)
    }
}

/// The lines `mdma facts` prints for one track: a header, then the kept facts in the
/// order the library gave them. A track whose facts are all filtered out keeps its header.
pub(crate) fn facts_block(
    full_hash: &str,
    facts: &[(String, String)],
    filter: &KindFilter,
) -> Vec<String> {
    let header = [format!("Facts for: {full_hash}"), "=".repeat(65)];
    let kept = facts
        .iter()
        .filter(|(kind, _)| filter.keeps(kind))
        .map(|(kind, value)| format!("{kind:20} | {value}"));
    header.into_iter().chain(kept).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{parses, run_arg, usage_error};
    use pretty_assertions::assert_eq;

    fn kinds(names: &[&str]) -> Vec<FactKind> {
        names.iter().map(|n| n.parse().unwrap()).collect()
    }

    fn filter(include: &[&str], exclude: &[&str]) -> KindFilter {
        KindFilter::new(IncludeKinds(kinds(include)), ExcludeKinds(kinds(exclude)))
    }

    #[test]
    fn no_includes_keeps_every_kind_even_one_the_cli_does_not_know() {
        let all = filter(&[], &[]);
        assert!(all.keeps("Title"));
        assert!(all.keeps("SomeFutureKind"));
    }

    #[test]
    fn includes_keep_only_the_named_kinds() {
        let only = filter(&["bpm", "Key"], &[]);
        assert!(only.keeps("BPM"));
        assert!(only.keeps("Key"));
        assert!(!only.keeps("Title"));
        assert!(!only.keeps("SomeFutureKind"));
    }

    #[test]
    fn exclude_wins_over_include() {
        let f = filter(&["BPM", "Key"], &["Key"]);
        assert!(f.keeps("BPM"));
        assert!(!f.keeps("Key"));
    }

    #[test]
    fn excludes_without_includes_remove_from_all() {
        let f = filter(&[], &["TrackStarted"]);
        assert!(!f.keeps("TrackStarted"));
        assert!(f.keeps("Title"));
        assert!(f.keeps("SomeFutureKind"));
    }

    #[test]
    fn repeated_flags_all_count() {
        assert_eq!(
            run_arg::<IncludeKinds>(&["--include=BPM", "--include", "key"]),
            Ok(IncludeKinds(kinds(&["BPM", "Key"])))
        );
        assert_eq!(
            run_arg::<ExcludeKinds>(&["--exclude", "title", "--exclude=Album"]),
            Ok(ExcludeKinds(kinds(&["Title", "Album"])))
        );
        assert_eq!(run_arg::<IncludeKinds>(&[]), Ok(IncludeKinds(vec![])));
    }

    #[test]
    fn unknown_kind_is_a_usage_error() {
        let e = usage_error(&["facts", "--include=Nope", "-"]);
        assert!(e.contains("unknown fact kind"), "{e}");
        usage_error(&["facts", "--exclude=Nope", "-"]);
        assert_eq!(parses(&["facts", "--include=bpm", "-"]), ["facts"]);
    }

    #[test]
    fn complete_fact_kinds_filters_by_prefix_ignoring_case() {
        let names = |typed: &str| -> Vec<String> {
            complete_fact_kinds(typed)
                .into_iter()
                .map(|(n, _)| n)
                .collect()
        };
        assert_eq!(names("tracks"), ["TrackStarted", "TrackStopped"]);
        assert_eq!(names("BP"), ["BPM"]);
        assert_eq!(
            names("rack"),
            Vec::<String>::new(),
            "a prefix, not a substring"
        );
        assert_eq!(names("zzz"), Vec::<String>::new());
        assert_eq!(names("").len(), MusicValue::ATTRIBUTE_NAMES.len());
    }

    fn sample_facts() -> Vec<(String, String)> {
        [("Title", "Song"), ("BPM", "128"), ("Key", "8A")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .to_vec()
    }

    #[test]
    fn block_is_header_then_kept_facts_in_reply_order() {
        let only = filter(&["Key", "Title"], &[]);
        let block = facts_block("sha256:abcd", &sample_facts(), &only);
        assert_eq!(
            block,
            [
                "Facts for: sha256:abcd".to_string(),
                "=".repeat(65),
                format!("{:20} | Song", "Title"),
                format!("{:20} | 8A", "Key"),
            ]
        );
    }

    #[test]
    fn block_with_no_kept_facts_still_has_its_header() {
        let block = facts_block("sha256:abcd", &sample_facts(), &filter(&["Album"], &[]));
        assert_eq!(
            block,
            ["Facts for: sha256:abcd".to_string(), "=".repeat(65)]
        );
    }
}
