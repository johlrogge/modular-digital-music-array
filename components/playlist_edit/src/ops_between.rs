//! The edit ops that turn a playlist's entries into a target sequence
//! (ADR-032 §6). Ops name entries by position and never re-serialize a line,
//! so every line the ops do not touch (comments, short IDs, trailing text)
//! stays byte for byte when `apply` writes the file.

use std::collections::{HashMap, HashSet};

use library_ipc_protocol::{ContentHash, Duplicates, EditOp, PlaylistEntry};
use sequence_merge::{lcs, occurrences};
use thiserror::Error;

/// Why no ops can reach the target.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OpsBetweenError {
    /// No op adds an unresolved entry, so the target can only keep one that
    /// the playlist already has. Always a caller bug.
    #[error("target entry {at} is unresolved and not among the current entries")]
    UnresolvedNotInCurrent { at: usize },
}

/// Ops that, applied in order with `apply`, leave exactly the entries of
/// `target` in the playlist whose entries are `current`.
///
/// Duplicates are matched by occurrence: the n-th copy of an entry in `target`
/// is the n-th copy in `current`, so a surplus copy is the last one in file
/// order. Entries both sequences share are kept; only those outside a longest
/// common in-order run are moved. The list is: removals (highest position
/// first), moves, then inserts (a run of new tracks is one op). An entry of
/// `current` that `target` omits is removed, unresolved or not.
///
/// An `Unresolved` entry in `target` must equal one in `current`, since there
/// is no op to add one: otherwise this is an error. O(n^2).
pub fn ops_between(
    current: &[PlaylistEntry],
    target: &[PlaylistEntry],
) -> Result<Vec<EditOp>, OpsBetweenError> {
    // The n-th copy of an entry in `target` is the n-th copy in `current`.
    let in_current: HashMap<_, usize> = occurrences(current)
        .into_iter()
        .enumerate()
        .map(|(i, occurrence)| (occurrence, i))
        .collect();
    let source: Vec<Option<usize>> = occurrences(target)
        .iter()
        .map(|occurrence| in_current.get(occurrence).copied())
        .collect();
    if let Some(at) = (0..target.len())
        .find(|&j| source[j].is_none() && matches!(target[j], PlaylistEntry::Unresolved { .. }))
    {
        return Err(OpsBetweenError::UnresolvedNotInCurrent { at });
    }

    // Which target entry each current entry is, if any.
    let mut owner: Vec<Option<usize>> = vec![None; current.len()];
    for (j, i) in source.iter().enumerate() {
        if let Some(i) = i {
            owner[*i] = Some(j);
        }
    }

    // Remove what is gone, highest position first so lower ones keep theirs.
    let mut ops: Vec<EditOp> = (0..current.len())
        .rev()
        .filter(|&i| owner[i].is_none())
        .map(|at| EditOp::Remove { at })
        .collect();

    // What is left, as target indices in current order; and in target order.
    let mut left: Vec<usize> = owner.iter().flatten().copied().collect();
    let matched: Vec<usize> = (0..target.len()).filter(|&j| source[j].is_some()).collect();

    // Entries in a longest in-order run stay; each other one moves once, to
    // just after its nearest matched predecessor in target (already in place:
    // it is in the run or moved before, as moves go in target order).
    let stays: HashSet<usize> = lcs(&left, &matched).into_iter().collect();
    for &j in matched.iter().filter(|j| !stays.contains(j)) {
        let from = position(&left, j);
        left.remove(from);
        let to = matched
            .iter()
            .take_while(|&&p| p < j)
            .last()
            .map_or(0, |&p| position(&left, p) + 1);
        left.insert(to, j);
        debug_assert_ne!(from, to, "an entry outside the longest run really moves");
        ops.push(EditOp::Move { from, to });
    }

    // `left` is now the matched entries in target order: insert the rest. A
    // run of new tracks goes in as one op.
    let mut run: Vec<ContentHash> = Vec::new();
    for (at, entry) in target.iter().enumerate() {
        match (source[at], entry) {
            (None, PlaylistEntry::Track { hash }) => run.push(hash.into()),
            _ => flush_run(&mut ops, &mut run, at),
        }
    }
    flush_run(&mut ops, &mut run, target.len());
    Ok(ops)
}

fn position(left: &[usize], j: usize) -> usize {
    left.iter()
        .position(|&x| x == j)
        .expect("every matched target entry is left")
}

/// Emit the pending run of new tracks, which ends just before position `end`.
fn flush_run(ops: &mut Vec<EditOp>, run: &mut Vec<ContentHash>, end: usize) {
    if run.is_empty() {
        return;
    }
    let at = end - run.len();
    ops.push(EditOp::Insert {
        at,
        hashes: std::mem::take(run),
        duplicates: Duplicates::Keep,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{info, test_track, TestTrack};
    use crate::{apply, entries, PlaylistDoc};
    use library_ipc_protocol::{is_entry_line, CanonicalHash};
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;
    use track_formatter::format_track_line;

    const TAGS: [char; 6] = ['a', 'b', 'c', 'd', 'e', 'f'];

    fn full(tag: char) -> String {
        format!("sha256:{}", tag.to_string().repeat(64))
    }

    fn track(tag: char) -> PlaylistEntry {
        PlaylistEntry::Track {
            hash: CanonicalHash::try_from(ContentHash::new(full(tag))).unwrap(),
        }
    }

    fn tracks_of(tags: &str) -> Vec<PlaylistEntry> {
        tags.chars().map(track).collect()
    }

    /// Tracks a to f, and a hidden track 9 (so "999" is unresolved).
    fn library() -> Vec<TestTrack> {
        let mut tracks: Vec<TestTrack> = TAGS
            .iter()
            .map(|&tag| test_track(full(tag), &tag.to_uppercase().to_string()))
            .collect();
        let mut hidden = test_track(full('9'), "Hidden");
        hidden.hidden = true;
        tracks.push(hidden);
        tracks
    }

    /// The ops from `current` (the entries of `text`) to `target`, and `text`
    /// with them applied.
    fn edited(text: &str, target: &[PlaylistEntry]) -> (String, Vec<EditOp>) {
        let tracks = library();
        let current = entries(text, &tracks);
        let ops = ops_between(&current, target).expect("target is reachable");
        let doc = apply(&PlaylistDoc::parse(text), &ops, &tracks, |t| t.info.clone())
            .expect("ops_between gives ops that apply")
            .doc;
        (doc.render(), ops)
    }

    fn text_of(tags: &str) -> String {
        tags.chars().map(|c| format!("{c}{c}{c}\n")).collect()
    }

    /// The tags the entries of `text` end up as.
    fn tags_in(text: &str) -> String {
        entries(text, &library())
            .iter()
            .map(|entry| match entry {
                PlaylistEntry::Track { hash } => {
                    hash.as_str().chars().nth("sha256:".len()).unwrap()
                }
                PlaylistEntry::Unresolved { .. } => '?',
            })
            .collect()
    }

    fn check(current: &str, target: &str) -> Vec<EditOp> {
        let (text, ops) = edited(&text_of(current), &tracks_of(target));
        assert_eq!(tags_in(&text), target, "from {current:?} to {target:?}");
        ops
    }

    // ----- examples -----

    #[test]
    fn same_sequence_needs_no_ops() {
        assert_eq!(check("abc", "abc"), vec![]);
        assert_eq!(check("", ""), vec![]);
    }

    #[test]
    fn empty_current_inserts_everything_in_one_op() {
        assert_eq!(
            check("", "abc"),
            vec![EditOp::Insert {
                at: 0,
                hashes: ["a", "b", "c"]
                    .map(|t| ContentHash::new(full(t.chars().next().unwrap())))
                    .to_vec(),
                duplicates: Duplicates::Keep,
            }]
        );
    }

    #[test]
    fn empty_target_removes_everything_from_the_back() {
        assert_eq!(
            check("abc", ""),
            vec![
                EditOp::Remove { at: 2 },
                EditOp::Remove { at: 1 },
                EditOp::Remove { at: 0 },
            ]
        );
    }

    #[test]
    fn remove_is_one_op() {
        assert_eq!(check("abc", "ac"), vec![EditOp::Remove { at: 1 }]);
    }

    #[test]
    fn insert_at_front_middle_and_end_is_one_op_each() {
        for (current, target, at) in [("bc", "abc", 0), ("ac", "abc", 1), ("ab", "abc", 2)] {
            let ops = check(current, target);
            assert!(
                matches!(ops.as_slice(), [EditOp::Insert { at: got, hashes, .. }]
                    if *got == at && hashes.len() == 1),
                "{ops:?}"
            );
        }
    }

    #[test]
    fn a_move_is_one_op() {
        assert_eq!(check("abc", "bca"), vec![EditOp::Move { from: 0, to: 2 }]);
        assert_eq!(check("abc", "cab"), vec![EditOp::Move { from: 2, to: 0 }]);
    }

    #[test]
    fn a_swap_is_one_move() {
        assert_eq!(check("abcd", "acbd").len(), 1);
    }

    #[test]
    fn a_removal_two_moves_and_an_insert_together() {
        // c goes; d, b, a is the reverse of a, b, d (longest run: one entry,
        // so two moves); e is new.
        let ops = check("abcd", "dbae");
        let count = |is: fn(&EditOp) -> bool| ops.iter().filter(|op| is(op)).count();
        assert_eq!(ops.len(), 4, "{ops:?}");
        assert_eq!(count(|op| matches!(op, EditOp::Remove { .. })), 1);
        assert_eq!(count(|op| matches!(op, EditOp::Move { .. })), 2);
        assert_eq!(count(|op| matches!(op, EditOp::Insert { .. })), 1);
    }

    #[test]
    fn a_surplus_duplicate_is_the_last_copy_in_file_order() {
        assert_eq!(check("aba", "ab"), vec![EditOp::Remove { at: 2 }]);
        assert_eq!(check("aab", "ab"), vec![EditOp::Remove { at: 1 }]);
        assert_eq!(check("aaa", "aa"), vec![EditOp::Remove { at: 2 }]);
    }

    #[test]
    fn an_added_duplicate_is_one_insert() {
        assert_eq!(check("ab", "aab").len(), 1);
        assert_eq!(check("a", "aaa").len(), 1);
    }

    #[test]
    fn duplicates_reorder_by_occurrence() {
        check("aabb", "baba");
        check("abab", "aabb");
    }

    fn unresolved_text() -> (&'static str, Vec<PlaylistEntry>) {
        let text = "aaa\n9999  # gone\nbbb\n";
        (text, entries(text, &library()))
    }

    #[test]
    fn an_unresolved_entry_the_target_keeps_is_kept_and_may_move() {
        let (text, current) = unresolved_text();
        assert!(matches!(current[1], PlaylistEntry::Unresolved { .. }));
        let target = vec![track('b'), current[1].clone(), track('a')];
        let (out, _) = edited(text, &target);
        assert_eq!(entries(&out, &library()), target);
        assert!(out.contains("9999  # gone\n"));
    }

    #[test]
    fn an_unresolved_entry_the_target_omits_is_removed() {
        let (text, current) = unresolved_text();
        let ops = ops_between(&current, &tracks_of("ab")).unwrap();
        assert_eq!(ops, vec![EditOp::Remove { at: 1 }]);
        let (out, _) = edited(text, &tracks_of("ab"));
        assert_eq!(out, "aaa\nbbb\n");
    }

    #[test]
    fn an_unresolved_entry_not_in_current_is_an_error() {
        let (_, current) = unresolved_text();
        let stranger = entries("7777\n", &library()).remove(0);
        for target in [
            vec![stranger.clone()],
            vec![track('a'), current[1].clone(), current[1].clone()],
        ] {
            let at = target.len() - 1;
            assert_eq!(
                ops_between(&current, &target),
                Err(OpsBetweenError::UnresolvedNotInCurrent { at })
            );
        }
    }

    #[test]
    fn untouched_lines_comments_and_short_ids_survive() {
        let text = "# set\naaa  Artist - A\n\n# second\nbbbbbbbbbbbb\nccc\n# end\n";
        let (out, _) = edited(text, &tracks_of("acd"));
        let new = format_track_line(&info(full('d'), "D"));
        assert_eq!(
            out,
            format!("# set\naaa  Artist - A\n\n# second\nccc\n{new}\n# end\n")
        );
    }

    // ----- properties -----

    fn tags(max: usize) -> impl Strategy<Value = String> {
        proptest::collection::vec(proptest::sample::select(TAGS.to_vec()), 0..=max)
            .prop_map(|tags| tags.into_iter().collect())
    }

    /// A playlist file line, without its line break: an entry in one of
    /// several forms (short, full, trailing text, CRLF), an unresolved one, a
    /// comment, or blank.
    fn file_line() -> impl Strategy<Value = String> {
        let tag = proptest::sample::select(TAGS.to_vec());
        let crlf = proptest::bool::weighted(0.3);
        prop_oneof![
            4 => (tag.clone(), 3usize..=12, proptest::option::of("[ A-Za-z-]{0,12}"), crlf)
                .prop_map(|(tag, len, trailing, crlf)| {
                    let token = tag.to_string().repeat(len);
                    let line = match trailing {
                        Some(text) => format!("{token}  {text}"),
                        None => token,
                    };
                    if crlf { format!("{line}\r") } else { line }
                }),
            1 => tag.prop_map(|tag| format!("sha256:{}", tag.to_string().repeat(64))),
            2 => (
                proptest::sample::select(vec!["9999  # gone", "999", "88", "9999"]),
                crlf,
            )
                .prop_map(|(line, crlf)| if crlf { format!("{line}\r") } else { line.to_string() }),
            1 => (Just("# "), "[a-z ]{0,10}", crlf)
                .prop_map(|(hash, text, crlf)| format!("{hash}{text}{}", if crlf { "\r" } else { "" })),
            1 => Just(String::new()),
        ]
    }

    /// `tags` as tracks, with some of `current`'s unresolved entries (each at
    /// most once) put at positions picked by `picks`.
    fn target_with_unresolved(
        tags: &str,
        current: &[PlaylistEntry],
        picks: &[(usize, usize)],
    ) -> Vec<PlaylistEntry> {
        let mut target = tracks_of(tags);
        let unresolved: Vec<&PlaylistEntry> = current
            .iter()
            .filter(|e| matches!(e, PlaylistEntry::Unresolved { .. }))
            .collect();
        let mut used = HashSet::new();
        for &(which, at) in picks {
            if unresolved.is_empty() {
                break;
            }
            let which = which % unresolved.len();
            if used.insert(which) {
                target.insert(at % (target.len() + 1), unresolved[which].clone());
            }
        }
        target
    }

    /// For each target entry, the current entry it is by the n-th occurrence
    /// rule, counted the slow way.
    fn sources(current: &[PlaylistEntry], target: &[PlaylistEntry]) -> Vec<Option<usize>> {
        (0..target.len())
            .map(|j| {
                let before = target[..j].iter().filter(|e| **e == target[j]).count();
                (0..current.len())
                    .filter(|&i| current[i] == target[j])
                    .nth(before)
            })
            .collect()
    }

    fn split_lines(text: &str) -> Vec<&str> {
        match text.strip_suffix('\n') {
            _ if text.is_empty() => vec![],
            Some(body) => body.split('\n').collect(),
            None => text.split('\n').collect(),
        }
    }

    proptest! {
        #[test]
        fn applying_ops_between_gives_the_target(current in tags(14), target in tags(14)) {
            let (text, _) = edited(&text_of(&current), &tracks_of(&target));
            prop_assert_eq!(tags_in(&text), target);
        }

        #[test]
        fn the_op_count_is_removals_plus_moves_plus_insert_runs(
            current in tags(14),
            target in tags(14),
        ) {
            let current = tracks_of(&current);
            let target = tracks_of(&target);
            let ops = ops_between(&current, &target).unwrap();
            let source = sources(&current, &target);
            let matched = source.iter().flatten().count();
            let in_target_order: Vec<usize> = (0..target.len()).filter(|&j| source[j].is_some()).collect();
            let in_current_order: Vec<usize> = {
                let mut owner = vec![None; current.len()];
                for (j, i) in source.iter().enumerate() {
                    if let Some(i) = i { owner[*i] = Some(j); }
                }
                owner.into_iter().flatten().collect()
            };
            let kept = lcs(&in_current_order, &in_target_order).len();
            let runs = (0..target.len())
                .filter(|&j| source[j].is_none() && (j == 0 || source[j - 1].is_some()))
                .count();
            prop_assert_eq!(ops.len(), (current.len() - matched) + (matched - kept) + runs);
        }

        #[test]
        fn the_file_is_exactly_the_oracle_file(
            lines in proptest::collection::vec(file_line(), 0..14),
            wanted in tags(14),
            picks in proptest::collection::vec((0usize..8, 0usize..16), 0..4),
        ) {
            let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
            let tracks = library();
            let current = entries(&text, &tracks);
            let target = target_with_unresolved(&wanted, &current, &picks);
            let (out, _) = edited(&text, &target);

            prop_assert_eq!(entries(&out, &tracks), target.clone());

            // Comments and blank lines: same lines in the same order.
            let others = |t: &str| -> Vec<String> {
                split_lines(t).into_iter().filter(|l| !is_entry_line(l)).map(str::to_string).collect()
            };
            prop_assert_eq!(others(&out), others(&text));

            // Entry lines: a target entry that is a current entry is that
            // original line, byte for byte (moved or not); a new track is a
            // freshly formatted line.
            let original: Vec<&str> =
                split_lines(&text).into_iter().filter(|l| is_entry_line(l)).collect();
            let expected: Vec<String> = sources(&current, &target)
                .iter()
                .zip(&target)
                .map(|(source, entry)| match (source, entry) {
                    (Some(i), _) => original[*i].to_string(),
                    (None, PlaylistEntry::Track { hash }) => {
                        let track = tracks
                            .iter()
                            .find(|t| t.info.content_hash == *hash)
                            .expect("target tracks are in the library");
                        format_track_line(&track.info)
                    }
                    (None, PlaylistEntry::Unresolved { .. }) => unreachable!("rejected"),
                })
                .collect();
            let got: Vec<String> = split_lines(&out)
                .into_iter()
                .filter(|l| is_entry_line(l))
                .map(str::to_string)
                .collect();
            prop_assert_eq!(got, expected);
        }
    }
}
